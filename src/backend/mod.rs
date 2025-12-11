mod display_tracker;
mod window_tracker;

use std::{collections::HashMap, future::pending, sync::Arc};

use anyhow::{Context, Error as AnyError};
use ashpd::{
    AppID, PortalError, WindowIdentifierType,
    backend::{
        Builder,
        request::RequestImpl,
        screencast::{
            CreateSessionOptions, ScreencastImpl, SelectSourcesOptions, SelectSourcesResponse,
            StartCastOptions, StartCastResponse, StartCastResponseBuilder,
        },
        session::{CreateSessionResponse, SessionImpl},
    },
    desktop::{
        HandleToken, PersistMode,
        screencast::{CursorMode, SourceType, StreamBuilder},
    },
    enumflags2::BitFlags,
};
use async_channel::{Sender, unbounded};
use async_lock::Mutex;
use futures_util::StreamExt;
use zbus::{
    Connection, Error as ZbusError,
    fdo::RequestNameFlags,
    zvariant::{Array, OwnedObjectPath, OwnedValue, Signature, Structure, Value},
};

use crate::{
    backend::{
        display_tracker::DisplayStateTracker,
        generated::{
            org_gnome_mutter_screencast::ScreenCastProxy,
            org_gnome_mutter_screencast_session::SessionProxy,
            org_gnome_mutter_screencast_stream::{PipeWireStreamAddedStream, StreamProxy},
        },
        window_tracker::WindowStateTracker,
    },
    common::PopupData,
};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/dbus.rs"));
}

const RESTORE_DATA_PROVIDER: &str = "Kagayaku";
const RESTORE_DATA_VERSION: u32 = 1;

pub async fn backend_main(tx: Sender<PopupData>) -> Result<(), AnyError> {
    Builder::new("org.freedesktop.impl.portal.desktop.kagayaku")
        .context("failed to create builder")?
        .with_flags(
            RequestNameFlags::AllowReplacement
                | RequestNameFlags::DoNotQueue
                | RequestNameFlags::ReplaceExisting,
        )
        .screencast(ScreencastBackend::new(tx).await?)
        .build()
        .await
        .context("failed to build DBus backend")?;

    tracing::debug!("starting loop");

    pending().await
}

pub enum GnomeStreamRestoreData {
    Monitor { match_string: String },
    Window { app_id: String, title: String },
}

struct GnomeStream {
    id: u32,
    pipewire_node_id: Option<u32>,
    source_type: SourceType,
    added_stream: PipeWireStreamAddedStream,
    restore_data: GnomeStreamRestoreData,
}

pub struct GnomeSession {
    proxy: SessionProxy<'static>,
    streams: Vec<GnomeStream>,
}

impl GnomeSession {
    pub async fn new(
        connection: &Connection,
        object_path: OwnedObjectPath,
    ) -> Result<Self, ZbusError> {
        let proxy = SessionProxy::builder(connection)
            .path(object_path)?
            .build()
            .await?;

        Ok(Self {
            proxy,
            streams: Vec::new(),
        })
    }

    pub async fn start(&mut self) -> Result<(), ZbusError> {
        self.proxy.start().await?;

        for stream in self.streams.iter_mut() {
            if let Some(a) = stream.added_stream.next().await
                && let Ok(args) = a.args()
            {
                stream.pipewire_node_id = Some(args.node_id);
            };
        }

        Ok(())
    }

    pub async fn stop(&self) -> Result<(), ZbusError> {
        self.proxy.stop().await
    }

    pub async fn record_monitor(
        &mut self,
        connection: &Connection,
        id: u32,
        connector: String,
        match_string: String,
        cursor_mode: CursorMode,
    ) -> Result<(), ZbusError> {
        let mut props = HashMap::new();
        let cursor_mode_value = (cursor_mode as u32).into();
        props.insert("cursor-mode", &cursor_mode_value);

        let object_path = self.proxy.record_monitor(&connector, props).await?;
        self.new_stream(
            connection,
            id,
            SourceType::Monitor,
            object_path,
            GnomeStreamRestoreData::Monitor { match_string },
        )
        .await?;

        Ok(())
    }

    pub async fn record_window(
        &mut self,
        connection: &Connection,
        id: u32,
        window_id: u64,
        app_id: String,
        title: String,
        cursor_mode: CursorMode,
    ) -> Result<(), ZbusError> {
        let mut props = HashMap::new();
        let window_id_value = window_id.into();
        let cursor_mode_value = (cursor_mode as u32).into();
        props.insert("window-id", &window_id_value);
        props.insert("cursor-mode", &cursor_mode_value);

        let object_path = self.proxy.record_window(props).await?;
        self.new_stream(
            connection,
            id,
            SourceType::Window,
            object_path,
            GnomeStreamRestoreData::Window { app_id, title },
        )
        .await?;

        Ok(())
    }

    async fn new_stream(
        &mut self,
        connection: &Connection,
        id: u32,
        source_type: SourceType,
        object_path: OwnedObjectPath,
        restore_data: GnomeStreamRestoreData,
    ) -> Result<(), ZbusError> {
        let proxy = StreamProxy::builder(connection)
            .path(object_path)?
            .build()
            .await?;
        let added_stream = proxy.receive_pipe_wire_stream_added().await?;

        self.streams.push(GnomeStream {
            id,
            pipewire_node_id: None,
            source_type,
            added_stream,
            restore_data,
        });

        Ok(())
    }
}

#[derive(Clone)]
pub enum ScreencastStream {
    Monitor {
        id: u32,
        connector: String,
        match_string: String,
    },
    Window {
        id: u32,
        window_id: u64,
        app_id: String,
        title: String,
    },
}

struct ScreencastSession {
    multiple: bool,
    cursor_mode: CursorMode,
    source_type: BitFlags<SourceType>,
    persist_mode: PersistMode,
    gnome_session: Option<GnomeSession>,
    streams: Vec<ScreencastStream>,
}

impl Default for ScreencastSession {
    fn default() -> Self {
        Self {
            multiple: false,
            cursor_mode: CursorMode::Hidden,
            source_type: SourceType::Monitor.into(),
            persist_mode: PersistMode::DoNot,
            gnome_session: None,
            streams: Vec::new(),
        }
    }
}

pub struct ScreencastBackend {
    ui_tx: Sender<PopupData>,
    connection: Connection,
    display_state_tracker: Arc<Mutex<DisplayStateTracker>>,
    window_state_tracker: Arc<Mutex<WindowStateTracker>>,
    sessions: Arc<Mutex<HashMap<HandleToken, ScreencastSession>>>,
    mutter_screencast_proxy: ScreenCastProxy<'static>,
}

impl ScreencastBackend {
    pub async fn new(ui_tx: Sender<PopupData>) -> Result<Self, AnyError> {
        let connection = Connection::session().await?;
        let display_state_tracker = Mutex::new(DisplayStateTracker::new(&connection).await?).into();
        let window_state_tracker = Mutex::new(WindowStateTracker::new(&connection).await?).into();
        let sessions = Mutex::new(HashMap::new()).into();
        let mutter_screencast_proxy = ScreenCastProxy::new(&connection).await?;

        Ok(Self {
            ui_tx,
            connection,
            display_state_tracker,
            window_state_tracker,
            sessions,
            mutter_screencast_proxy,
        })
    }
}

#[async_trait::async_trait]
impl RequestImpl for ScreencastBackend {
    async fn close(&self, _: HandleToken) {}
}

#[async_trait::async_trait]
impl SessionImpl for ScreencastBackend {
    async fn session_closed(&self, session_token: HandleToken) -> Result<(), PortalError> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.remove(&session_token)
            && let Some(gnome_session) = &session.gnome_session
        {
            gnome_session.stop().await?;
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl ScreencastImpl for ScreencastBackend {
    fn available_source_types(&self) -> BitFlags<SourceType> {
        SourceType::Monitor | SourceType::Window
    }

    fn available_cursor_mode(&self) -> BitFlags<CursorMode> {
        CursorMode::Hidden | CursorMode::Embedded | CursorMode::Metadata
    }

    async fn create_session(
        &self,
        token: HandleToken,
        session_token: HandleToken,
        _: Option<AppID>,
        _: CreateSessionOptions,
    ) -> Result<CreateSessionResponse, PortalError> {
        let mut sessions = self.sessions.lock().await;

        sessions.insert(session_token, Default::default());

        Ok(CreateSessionResponse::new(token))
    }

    // TODO: support remote desktop session
    async fn select_sources(
        &self,
        session_token: HandleToken,
        _: Option<AppID>,
        options: SelectSourcesOptions,
    ) -> Result<SelectSourcesResponse, PortalError> {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(&session_token) else {
            return Err(PortalError::InvalidArgument("unknown session token".into()));
        };

        if let Some(m) = options.is_multiple() {
            session.multiple = m;
        }
        if let Some(c) = options.cursor_mode() {
            session.cursor_mode = c;
        }
        if let Some(p) = options.persist_mode() {
            session.persist_mode = p;
        }
        if let Some(s) = options.types() {
            session.source_type = if s.is_empty() {
                SourceType::Monitor.into()
            } else {
                s
            };
        }

        if session.persist_mode != PersistMode::DoNot
            && let Some((provider, version, data)) = options.restore_data()
            && provider == RESTORE_DATA_PROVIDER
            && version == RESTORE_DATA_VERSION
            && let Ok((_, _, a)) = data.to_owned().downcast::<(i64, i64, Array)>()
        {
            let s = self.restore_streams(a.iter()).await;
            if !s.is_empty() {
                session.streams = s;
            }
        }

        Ok(SelectSourcesResponse {})
    }

    async fn start_cast(
        &self,
        session_token: HandleToken,
        _: Option<AppID>,
        _: Option<WindowIdentifierType>,
        _: StartCastOptions,
    ) -> Result<StartCastResponse, PortalError> {
        let sessions = self.sessions.lock().await;
        let Some(session) = sessions.get(&session_token) else {
            return Err(PortalError::InvalidArgument("unknown session token".into()));
        };

        let session_path = self
            .mutter_screencast_proxy
            .create_session(HashMap::new())
            .await?;
        let mut gnome_session = GnomeSession::new(&self.connection, session_path).await?;
        let prompt_session = session.streams.is_empty();
        let source_type = session.source_type;
        // drop while running the UI
        drop(sessions);

        let prompted_streams = if prompt_session {
            let (tx, rx) = unbounded();
            if let Err(e) = self
                .ui_tx
                .send(PopupData {
                    dbus_tx: tx,
                    source_type,
                })
                .await
            {
                tracing::warn!("failed to send UI message: {}", e);
                return Err(PortalError::Failed(format!("cannot start UI: {}", e)));
            }
            match rx.recv().await {
                Ok(s) => s,
                Err(e) => {
                    return Err(PortalError::Failed(format!(
                        "failed to receive data from UI: {}",
                        e
                    )));
                }
            }
        } else {
            Vec::new()
        };

        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(&session_token).unwrap();

        for stream in session.streams.iter().chain(prompted_streams.iter()) {
            match stream {
                ScreencastStream::Monitor {
                    id,
                    connector,
                    match_string,
                } => {
                    if session.source_type.contains(SourceType::Monitor) {
                        gnome_session
                            .record_monitor(
                                &self.connection,
                                *id,
                                connector.to_string(),
                                match_string.to_string(),
                                session.cursor_mode,
                            )
                            .await?;
                    }
                }
                ScreencastStream::Window {
                    id,
                    window_id,
                    app_id,
                    title,
                } => {
                    if session.source_type.contains(SourceType::Window) {
                        gnome_session
                            .record_window(
                                &self.connection,
                                *id,
                                *window_id,
                                app_id.to_string(),
                                title.to_string(),
                                session.cursor_mode,
                            )
                            .await?;
                    }
                }
            }
        }

        gnome_session.start().await?;

        let mut streams = Vec::new();
        let mut restore_data = Array::new(&Signature::try_from("uuv").unwrap());

        for stream in gnome_session.streams.iter() {
            if let Some(node_id) = stream.pipewire_node_id {
                streams.push(
                    StreamBuilder::new(node_id)
                        .id(Some(stream.id.to_string()))
                        .source_type(stream.source_type)
                        .build(),
                );

                if session.persist_mode != PersistMode::DoNot {
                    let stream_data = match &stream.restore_data {
                        GnomeStreamRestoreData::Monitor { match_string } => {
                            Value::from(match_string.to_string())
                        }
                        GnomeStreamRestoreData::Window { app_id, title } => {
                            Value::from((app_id.to_string(), title.to_string()))
                        }
                    };

                    restore_data
                        .append((stream.id, stream.source_type as u32, stream_data).into())
                        .unwrap();
                }
            }
        }

        let mut resp = StartCastResponseBuilder::new(streams);

        if session.persist_mode != PersistMode::DoNot {
            resp = resp.restore_data(Some((
                RESTORE_DATA_PROVIDER.to_string(),
                RESTORE_DATA_VERSION,
                // we currently dont use timestamp
                Value::from((0, 0, restore_data)).try_into_owned().unwrap(),
            )));
        }

        session.gnome_session = Some(gnome_session);

        Ok(resp.build())
    }
}

impl ScreencastBackend {
    async fn restore_streams<'a>(
        &'a self,
        iter: impl Iterator<Item = &'a Value<'a>>,
    ) -> Vec<ScreencastStream> {
        let mut streams = Vec::new();
        let mut display_state = self.display_state_tracker.lock().await;
        let mut window_state = self.window_state_tracker.lock().await;

        if display_state.has_changed().await {
            if let Err(e) = display_state.refresh().await {
                tracing::warn!("failed to refresh display state: {}", e);
            }
        }
        if window_state.has_changed().await {
            if let Err(e) = window_state.refresh().await {
                tracing::warn!("failed to refresh window state: {}", e);
            }
        }

        for stream in iter {
            let Ok((id, source_type, data)) =
                stream.to_owned().downcast::<(u32, u32, OwnedValue)>()
            else {
                continue;
            };

            match source_type {
                v if v == SourceType::Monitor as u32 => {
                    let Ok(match_string) = data.downcast_ref() else {
                        continue;
                    };

                    if let Some(monitor) = display_state.find_monitor(match_string) {
                        streams.push(ScreencastStream::Monitor {
                            id,
                            connector: monitor.connector(),
                            match_string: monitor.match_string(),
                        });
                    };
                }
                v if v == SourceType::Window as u32 => {
                    let Ok(s) = data.downcast_ref::<Structure>() else {
                        continue;
                    };
                    let Ok((app_id, title)): Result<(String, String), _> = s.try_into() else {
                        continue;
                    };

                    for (wid, window) in window_state.windows().iter() {
                        if window.app_id != app_id {
                            continue;
                        }

                        // TODO: levenshtein distance search
                        if title == window.title {
                            streams.push(ScreencastStream::Window {
                                id: id,
                                window_id: *wid,
                                app_id,
                                title,
                            });
                            break;
                        }
                    }
                }
                v if v == SourceType::Virtual as u32 => {
                    continue;
                }
                v => {
                    tracing::debug!("unknown source type: {}", v);
                    continue;
                }
            }
        }

        streams
    }
}
