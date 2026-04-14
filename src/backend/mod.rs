pub mod display_tracker;
pub mod window_tracker;

use std::{
  collections::HashMap,
  future::pending,
  sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
  },
  time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Error as AnyError};
use ashpd::{
  AppID, PortalError, WindowIdentifierType,
  backend::{
    Builder,
    request::RequestImpl,
    screencast::{ScreencastImpl, SelectSourcesResponse},
    session::{CreateSessionResponse, SessionImpl},
  },
  desktop::{
    CreateSessionOptions, HandleToken, PersistMode,
    screencast::{
      CursorMode, SelectSourcesOptions, SourceType, StartCastOptions, StreamBuilder, Streams, StreamsBuilder,
    },
  },
  enumflags2::BitFlags,
};
use async_channel::{Sender, unbounded};
use async_lock::Mutex;
use futures_util::{
  StreamExt,
  task::{FutureObj, Spawn, SpawnError},
};
use tracing::instrument;
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
  common::{PopupData, ScreencastStreamChoice, ToBackendMessage, ToUiMessage},
};

mod generated {
  include!(concat!(env!("OUT_DIR"), "/dbus.rs"));
}

const RESTORE_DATA_PROVIDER: &str = "Kagayaku";
const RESTORE_DATA_VERSION: u32 = 1;

struct GlobalExecutorSpawner;

impl Spawn for GlobalExecutorSpawner {
  fn spawn_obj(&self, future: FutureObj<'static, ()>) -> Result<(), SpawnError> {
    async_global_executor::spawn(future).detach();
    Ok(())
  }
}

pub async fn backend_main(tx: Sender<ToUiMessage>) -> Result<(), AnyError> {
  Builder::new("org.freedesktop.impl.portal.desktop.kagayaku")
    .context("failed to create builder")?
    .with_flags(RequestNameFlags::AllowReplacement | RequestNameFlags::DoNotQueue | RequestNameFlags::ReplaceExisting)
    .with_spawn(GlobalExecutorSpawner)
    .screencast(ScreencastBackend::new(tx).await?)
    .build()
    .await
    .context("failed to build DBus backend")?;

  tracing::info!("starting backend loop");

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
  position: Option<(i32, i32)>,
  size: Option<(i32, i32)>,
  mapping_id: Option<String>,
  added_stream: PipeWireStreamAddedStream,
  restore_data: GnomeStreamRestoreData,
}

pub struct GnomeSession {
  proxy: SessionProxy<'static>,
  streams: Vec<GnomeStream>,
}

impl GnomeSession {
  pub async fn new(connection: &Connection, object_path: OwnedObjectPath) -> Result<Self, ZbusError> {
    let proxy = SessionProxy::new(connection, object_path).await?;

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
    self
      .new_stream(
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
    self
      .new_stream(
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
    let proxy = StreamProxy::new(connection, object_path).await?;
    let added_stream = proxy.receive_pipe_wire_stream_added().await?;
    let (position, size, mapping_id) = match proxy.parameters().await {
      Ok(parameters) => {
        let position = parameters
          .get("position")
          .and_then(|v| v.downcast_ref::<(i32, i32)>().ok());
        let size = parameters.get("size").and_then(|v| v.downcast_ref::<(i32, i32)>().ok());
        let mapping_id = parameters
          .get("mapping-id")
          .and_then(|v| v.downcast_ref::<&str>().ok())
          .map(|s| s.to_string())
          .or_else(|| {
            parameters
              .get("mapping-id")
              .and_then(|v| v.downcast_ref::<String>().ok())
          });
        (position, size, mapping_id)
      }
      Err(e) => {
        tracing::warn!("failed to fetch stream parameters: {}", e);
        (None, None, None)
      }
    };

    self.streams.push(GnomeStream {
      id,
      pipewire_node_id: None,
      source_type,
      position,
      size,
      mapping_id,
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
  restore_data: Option<OwnedValue>,
}

impl Default for ScreencastSession {
  fn default() -> Self {
    Self {
      multiple: false,
      cursor_mode: CursorMode::Hidden,
      source_type: SourceType::Monitor.into(),
      persist_mode: PersistMode::DoNot,
      gnome_session: None,
      restore_data: None,
    }
  }
}

pub struct ScreencastBackend {
  ui_tx: Sender<ToUiMessage>,
  connection: Connection,
  display_state_tracker: Arc<Mutex<DisplayStateTracker>>,
  window_state_tracker: Arc<Mutex<WindowStateTracker>>,
  sessions: Arc<Mutex<HashMap<HandleToken, ScreencastSession>>>,
  mutter_screencast_proxy: ScreenCastProxy<'static>,
  counter: AtomicU32,
}

impl ScreencastBackend {
  pub async fn new(ui_tx: Sender<ToUiMessage>) -> Result<Self, AnyError> {
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
      counter: AtomicU32::new(0),
    })
  }
}

#[async_trait::async_trait]
impl RequestImpl for ScreencastBackend {
  #[instrument(skip_all, fields(token = %_token))]
  async fn close(&self, _token: HandleToken) {
    tracing::info!("closing request");
  }
}

#[async_trait::async_trait]
impl SessionImpl for ScreencastBackend {
  #[instrument(skip(self))]
  async fn session_closed(&self, session_token: HandleToken) -> Result<(), PortalError> {
    let mut sessions = self.sessions.lock().await;
    if let Some(session) = sessions.remove(&session_token) {
      tracing::info!("closing session");
      drop(sessions);
      if let Err(e) = self
        .ui_tx
        .try_send(ToUiMessage::CloseSession(session_token.to_string()))
      {
        tracing::warn!("failed to send close request to UI thread: {}", e);
      }
      if let Some(gnome_session) = session.gnome_session {
        gnome_session.stop().await?
      }
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
    // FIXME: ashpd 0.13.0-alpha cant deserialize metadata cursor mode (value 4)
    CursorMode::Hidden | CursorMode::Embedded
  }

  async fn create_session(
    &self,
    _: HandleToken,
    session_token: HandleToken,
    _: Option<AppID>,
    _: CreateSessionOptions,
  ) -> Result<CreateSessionResponse, PortalError> {
    let mut sessions = self.sessions.lock().await;
    sessions.insert(session_token.clone(), Default::default());

    Ok(CreateSessionResponse::new(session_token))
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
    if let Some(s) = options.sources() {
      session.source_type = if s.is_empty() { SourceType::Monitor.into() } else { s };
    }

    if session.persist_mode != PersistMode::DoNot
      && let Some((provider, version, data)) = options.restore_data()
      && provider == RESTORE_DATA_PROVIDER
      && version == RESTORE_DATA_VERSION
    {
      session.restore_data = Some(data.try_to_owned().unwrap());
    }

    Ok(SelectSourcesResponse {})
  }

  #[instrument(skip(self), fields(app_id = ?client_app_id))]
  async fn start_cast(
    &self,
    session_token: HandleToken,
    client_app_id: Option<AppID>,
    window_identifier: Option<WindowIdentifierType>,
    _: StartCastOptions,
  ) -> Result<Streams, PortalError> {
    tracing::info!("starting screencast session");

    let sessions = self.sessions.lock().await;
    let Some(session) = sessions.get(&session_token) else {
      return Err(PortalError::InvalidArgument("unknown session token".into()));
    };

    let session_path = self.mutter_screencast_proxy.create_session(HashMap::new()).await?;
    let mut gnome_session = GnomeSession::new(&self.connection, session_path).await?;

    let source_type = session.source_type;
    let restored_streams = if session.persist_mode != PersistMode::DoNot
      && let Some(d) = session.restore_data.as_ref()
    {
      if let Ok((_, _, a)) = d.downcast_ref::<(i64, i64, Array)>() {
        self.restore_streams(a.iter(), source_type).await
      } else {
        tracing::debug!("unknown restore data");
        None
      }
    } else {
      None
    };
    let multiple = session.multiple;
    let persist_mode = session.persist_mode;

    // drop while running the UI
    drop(sessions);

    let (remember, prompted_streams) = if restored_streams.is_none() {
      let (tx, rx) = unbounded();
      let (monitors, windows) = {
        let mut display_state = self.display_state_tracker.lock().await;
        let mut window_state = self.window_state_tracker.lock().await;

        if let Err(e) = display_state.refresh().await {
          tracing::warn!("failed to refresh display state: {}", e);
        }
        if let Err(e) = window_state.refresh().await {
          tracing::warn!("failed to refresh window state: {}", e);
        }

        (display_state.monitors().clone(), window_state.windows().clone())
      };

      let popup_data = PopupData {
        session_token: session_token.to_string(),
        app_id: client_app_id.map(|i| i.to_string()),
        parent_window: window_identifier.and_then(|ty| {
          if let WindowIdentifierType::Wayland(id) = ty {
            Some(id)
          } else {
            None
          }
        }),
        backend_tx: tx,
        multiple,
        source_type,
        persist_mode,
        monitors,
        windows,
      };

      if let Err(e) = self.ui_tx.send(ToUiMessage::NewPopup(popup_data)).await {
        tracing::warn!("failed to send UI message: {}", e);
        return Err(PortalError::Failed(format!("cannot start UI: {}", e)));
      }
      let backend_msg = rx.recv().await;

      match backend_msg {
        Ok(s) => match s {
          ToBackendMessage::Success((b, v)) => {
            tracing::info!(selected_sources = v.len(), "ui accepted screencast");
            let mut res = Vec::new();
            for choice in v {
              let id = self.counter.fetch_add(1, Ordering::Relaxed);
              match choice {
                ScreencastStreamChoice::Monitor {
                  connector,
                  match_string,
                } => res.push(ScreencastStream::Monitor {
                  id,
                  connector,
                  match_string,
                }),
                ScreencastStreamChoice::Window {
                  window_id,
                  app_id,
                  title,
                } => res.push(ScreencastStream::Window {
                  id,
                  window_id,
                  app_id,
                  title,
                }),
              }
            }
            (b, res)
          }
          ToBackendMessage::Cancel => {
            tracing::info!("ui cancelled screencast");
            return Err(PortalError::Cancelled("user cancelled".into()));
          }
        },
        Err(e) => {
          tracing::warn!("failed to receive data from UI: {}", e);
          return Err(PortalError::Failed(format!("failed to receive data from UI: {}", e)));
        }
      }
    } else {
      (false, Vec::new())
    };

    let mut sessions = self.sessions.lock().await;
    let session = sessions.get_mut(&session_token).unwrap();
    let streams_iter = if let Some(s) = restored_streams.as_ref() {
      s.iter()
    } else {
      prompted_streams.iter()
    };
    let should_return_restore_data =
      session.persist_mode != PersistMode::DoNot && (restored_streams.is_some() || remember);

    for stream in streams_iter {
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
    let last_used_time = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map(|duration| duration.as_micros().min(i64::MAX as u128) as i64)
      .unwrap_or(0);
    let creation_time = session
      .restore_data
      .as_ref()
      .and_then(|d| d.downcast_ref::<(i64, i64, Array)>().ok())
      .map(|(creation_time, _, _)| creation_time)
      .unwrap_or(last_used_time);

    for stream in gnome_session.streams.iter() {
      if let Some(node_id) = stream.pipewire_node_id {
        let mut stream_builder = StreamBuilder::new(node_id)
          .id(Some(stream.id.to_string()))
          .source_type(stream.source_type);

        stream_builder = stream_builder.position(stream.position);
        stream_builder = stream_builder.size(stream.size);
        stream_builder = stream_builder.mapping_id(stream.mapping_id.clone());

        streams.push(stream_builder.build());

        if should_return_restore_data {
          let stream_data = match &stream.restore_data {
            GnomeStreamRestoreData::Monitor { match_string } => Value::from(match_string.to_string()),
            GnomeStreamRestoreData::Window { app_id, title } => Value::from((app_id.to_string(), title.to_string())),
          };

          restore_data
            .append((stream.id, stream.source_type as u32, stream_data).into())
            .unwrap();
        }
      }
    }

    let mut resp = StreamsBuilder::new(streams);

    if should_return_restore_data {
      resp = resp.persist_mode(Some(session.persist_mode)).restore_data(Some((
        RESTORE_DATA_PROVIDER.to_string(),
        RESTORE_DATA_VERSION,
        Value::from((creation_time, last_used_time, restore_data))
          .try_into_owned()
          .unwrap(),
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
    source_types: BitFlags<SourceType>,
  ) -> Option<Vec<ScreencastStream>> {
    let mut streams = Vec::new();
    let mut display_state = self.display_state_tracker.lock().await;
    let mut window_state = self.window_state_tracker.lock().await;

    if let Err(e) = display_state.refresh().await {
      tracing::warn!("failed to refresh display state: {}", e);
    }
    if let Err(e) = window_state.refresh().await {
      tracing::warn!("failed to refresh window state: {}", e);
    }

    for stream in iter {
      let Ok((id, source_type, data)) = stream.to_owned().downcast::<(u32, u32, OwnedValue)>() else {
        tracing::debug!("failed to decode stream restore data");
        return None;
      };

      match source_type {
        v if v == SourceType::Monitor as u32 => {
          if !source_types.contains(SourceType::Monitor) {
            return None;
          }

          let Ok(match_string) = data.downcast_ref() else {
            return None;
          };

          let Some(monitor) = display_state.find_monitor(match_string) else {
            return None;
          };

          streams.push(ScreencastStream::Monitor {
            id,
            connector: monitor.connector.to_string(),
            match_string: monitor.match_string(),
          });
        }
        v if v == SourceType::Window as u32 => {
          if !source_types.contains(SourceType::Window) {
            return None;
          }

          let Ok(s) = data.downcast_ref::<Structure>() else {
            return None;
          };
          let Ok((app_id, title)): Result<(String, String), _> = s.try_into() else {
            return None;
          };

          let mut matched = false;
          for (wid, window) in window_state.windows().iter() {
            if window.app_id != app_id {
              continue;
            }

            // TODO: levenshtein distance search
            if title == window.title {
              streams.push(ScreencastStream::Window {
                id,
                window_id: *wid,
                app_id,
                title,
              });
              matched = true;
              break;
            }
          }

          if !matched {
            return None;
          }
        }
        v if v == SourceType::Virtual as u32 => {
          return None;
        }
        v => {
          tracing::debug!("unknown source type: {}", v);
          return None;
        }
      }
    }

    if streams.is_empty() { None } else { Some(streams) }
  }
}
