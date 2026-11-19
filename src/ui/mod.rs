mod screencast;
mod wayland;

use std::{
  collections::{HashMap, VecDeque},
  sync::Arc,
  thread::Builder as ThreadBuilder,
};

use ashpd::desktop::HandleToken;
use futures_util::SinkExt;
use iced::{
  Element, Settings, Size, Subscription, Task, daemon, exit, stream,
  wgpu::rwh::{RawDisplayHandle, RawWindowHandle},
  widget::space,
  window::{self, Level, close_requests, settings::PlatformSpecific},
};
use sctk::reexports::{
  calloop::channel::{Sender as CalloopSender, channel as calloop_channel},
  client::{
    Connection, Proxy,
    backend::{Backend, ObjectId},
    protocol::wl_surface::WlSurface,
  },
};
use smol::{
  channel::{Receiver, Sender, unbounded},
  lock::RwLock,
};
use tracing::instrument;

use crate::{
  common::{PopupData, PopupType, ScreencastData, ToBackendMessage, ToUiMessage},
  ui::{
    screencast::{Icon, Message as ScreencastMessage, Screencast, icon_tasks},
    wayland::{ToWaylandMessage, run_wayland_loop},
  },
};

const APP_ID: &str = "com.hol.kagayaku";

#[derive(Clone)]
enum WaylandMessage {
  ParentDestroyed(window::Id),
}

pub enum PopupMessage {
  Screencast(ScreencastMessage),
}

enum Message {
  PopupReceived(Option<PopupData>),
  PopupCloseRequested(window::Id),
  PopupSessionClosed(HandleToken),
  PopupRequestClosed(HandleToken),
  Popup(PopupMessage),
  WaylandReady(Connection, WlSurface, Option<String>, window::Id),
  Wayland(WaylandMessage),
  Exit,
  None,
}

enum ActivePopupContent {
  Screencast(Screencast),
}

struct ActivePopup {
  session_token: HandleToken,
  app_id: Option<String>,
  backend_tx: Sender<ToBackendMessage>,
  window_id: window::Id,
  wayland_tx: Option<CalloopSender<ToWaylandMessage>>,
  content: ActivePopupContent,
}

struct Daemon {
  active_popup: Option<ActivePopup>,
  queued_popups: VecDeque<PopupData>,
  icons_cache: Arc<RwLock<HashMap<String, Option<Icon>>>>,
}

impl Daemon {
  fn activate_popup(&mut self, popup_data: PopupData) -> Task<Message> {
    let PopupData {
      session_token,
      popup_type,
    } = popup_data;

    match popup_type {
      PopupType::Screencast(d, w) => self.activate_screencast(session_token, d, w),
    }
  }

  fn activate_screencast(
    &mut self,
    session_token: HandleToken,
    data: ScreencastData,
    parent_window: Option<String>,
  ) -> Task<Message> {
    let app_id = data.app_id.clone();
    let backend_tx = data.backend_tx.clone();

    tracing::info!("starting ui popup for {}", session_token);

    let (window_id, open_task) = window::open(window::Settings {
      platform_specific: PlatformSpecific {
        application_id: APP_ID.into(),
        ..Default::default()
      },
      size: Size::new(640., 540.),
      position: window::Position::Centered,
      level: Level::AlwaysOnTop,
      exit_on_close_request: false,
      ..Default::default()
    });

    let icon_task = Task::batch(icon_tasks(&data.windows, self.icons_cache.clone()));
    let content = Screencast::new(data);

    self.active_popup = Some(ActivePopup {
      session_token,
      app_id,
      backend_tx,
      window_id,
      wayland_tx: None,
      content: ActivePopupContent::Screencast(content),
    });

    Task::batch([
      icon_task.map(|m| Message::Popup(PopupMessage::Screencast(m))),
      open_task.then(move |id| {
        let parent_clone = parent_window.clone();
        window::run(id, move |w| {
          let Ok(RawWindowHandle::Wayland(window_handle)) = w.window_handle().map(|h| h.as_raw()) else {
            return Message::None;
          };
          let Ok(RawDisplayHandle::Wayland(display_handle)) = w.display_handle().map(|h| h.as_raw()) else {
            return Message::None;
          };
          let backend = unsafe { Backend::from_foreign_display(display_handle.display.as_ptr().cast()) };
          let conn = Connection::from_backend(backend);
          let Ok(surface_id) =
            (unsafe { ObjectId::from_ptr(WlSurface::interface(), window_handle.surface.as_ptr().cast()) })
          else {
            return Message::None;
          };
          let Ok(surface) = WlSurface::from_id(&conn, surface_id) else {
            tracing::warn!("invalid wl_surface id");
            return Message::None;
          };
          Message::WaylandReady(conn, surface, parent_clone, id)
        })
      }),
    ])
  }

  fn close_active_with(&mut self, backend_message: ToBackendMessage) -> Task<Message> {
    let Some(mut active_popup) = self.active_popup.take() else {
      tracing::warn!("there's no popup to close");
      return Task::none();
    };

    if let Err(e) = active_popup.backend_tx.try_send(backend_message) {
      tracing::warn!("failed to send message to backend: {}", e);
    }
    active_popup.wayland_tx.take();

    let close_task = window::close(active_popup.window_id);

    if let Some(next_popup) = self.queued_popups.pop_front() {
      tracing::info!("opening queued popup request");
      Task::batch([close_task, self.activate_popup(next_popup)])
    } else {
      close_task
    }
  }

  #[instrument(skip_all)]
  fn update(&mut self, msg: Message) -> Task<Message> {
    match msg {
      Message::PopupReceived(Some(popup_data)) => {
        if self.active_popup.is_some() {
          tracing::info!("other popup already running, queueing request");
          self.queued_popups.push_back(popup_data);
          return Task::none();
        }
        self.activate_popup(popup_data)
      }
      Message::PopupReceived(None) => {
        tracing::info!("ui request channel closed; exiting ui loop");
        exit()
      }
      Message::PopupCloseRequested(window_id) => {
        let Some(active_popup) = self.active_popup.as_ref() else {
          return Task::none();
        };
        if active_popup.window_id != window_id {
          return Task::none();
        }
        tracing::info!("popup close requested, cancelling request");
        self.close_active_with(ToBackendMessage::Cancel)
      }
      Message::PopupSessionClosed(session_token) => {
        if self
          .active_popup
          .as_ref()
          .is_some_and(|p| p.session_token == session_token)
        {
          tracing::info!("popup cancelled by backend for session {}", session_token);
          self.close_active_with(ToBackendMessage::Cancel)
        } else {
          self.queued_popups.retain(|p| p.session_token != session_token);
          Task::none()
        }
      }
      Message::PopupRequestClosed(session_token) => {
        if self
          .active_popup
          .as_ref()
          .is_some_and(|p| p.session_token == session_token)
        {
          tracing::info!("popup closed by request for session {}", session_token);
          self.close_active_with(ToBackendMessage::Cancel)
        } else {
          self.queued_popups.retain(|p| p.session_token != session_token);
          Task::none()
        }
      }
      Message::Popup(message) => {
        if let Some(active_popup) = self.active_popup.as_mut() {
          match (message, &mut active_popup.content) {
            (PopupMessage::Screencast(msg), ActivePopupContent::Screencast(c)) => match msg {
              ScreencastMessage::Close(m) => self.close_active_with(m),
              other => c.update(other).map(|m| Message::Popup(PopupMessage::Screencast(m))),
            },
          }
        } else {
          Task::none()
        }
      }
      Message::WaylandReady(conn, surface, parent, wid) => {
        if let Some(p) = self.active_popup.as_mut()
          && p.window_id == wid
          && p.wayland_tx.is_none()
        {
          let (tx, channel) = calloop_channel();
          let (to_ui, to_ui_rx) = unbounded();

          p.wayland_tx = tx.clone().into();

          if let Some(p) = parent
            && let Err(e) = tx.send(ToWaylandMessage::SetParent(surface, p, wid))
          {
            tracing::warn!("failed to send message to wayland thread: {}", e)
          };

          ThreadBuilder::new()
            .spawn(|| {
              if let Err(e) = run_wayland_loop(conn, channel, to_ui) {
                tracing::error!("wayland event loop stopped: {}", e);
              }
            })
            .expect("failed to spawn wayland thread");

          Task::stream(to_ui_rx).map(Message::Wayland)
        } else {
          Task::none()
        }
      }
      Message::Wayland(WaylandMessage::ParentDestroyed(wid)) => {
        if self.active_popup.as_ref().is_some_and(|p| p.window_id == wid) {
          tracing::info!("parent window for popup {} closed, cancelling", wid);
          self.close_active_with(ToBackendMessage::Cancel)
        } else {
          Task::none()
        }
      }
      Message::Exit => {
        if let Some(active_popup) = self.active_popup.take() {
          tracing::info!("closing active popup before exiting");
          if let Err(e) = active_popup.backend_tx.try_send(ToBackendMessage::Cancel) {
            tracing::warn!("failed to send message to backend: {}", e);
          }
        };

        exit()
      }
      Message::None => Task::none(),
    }
  }

  fn view(&self, id: window::Id) -> Element<'_, Message> {
    let Some(active_popup) = self.active_popup.as_ref().filter(|a| a.window_id == id) else {
      return space().into();
    };

    match &active_popup.content {
      ActivePopupContent::Screencast(content) => content.view().map(|m| Message::Popup(PopupMessage::Screencast(m))),
    }
  }

  fn subscription(&self) -> Subscription<Message> {
    if self.active_popup.is_some() {
      close_requests().map(Message::PopupCloseRequested)
    } else {
      Subscription::none()
    }
  }
}

pub fn ui_main(ui_rx: Receiver<ToUiMessage>) -> iced::Result {
  tracing::info!("starting UI loop");
  daemon(
    move || {
      let ui_rx_clone = ui_rx.clone();
      (
        Daemon {
          active_popup: None,
          queued_popups: VecDeque::new(),
          icons_cache: RwLock::new(HashMap::new()).into(),
        },
        Task::stream(stream::channel(10, async move |mut out| {
          loop {
            match ui_rx_clone.recv().await {
              Ok(ToUiMessage::NewPopup(d)) => {
                out.send(Message::PopupReceived(Some(d))).await.unwrap();
              }
              Ok(ToUiMessage::CloseSession(t)) => {
                out.send(Message::PopupSessionClosed(t)).await.unwrap();
              }
              Ok(ToUiMessage::RequestClosed(t)) => {
                out.send(Message::PopupRequestClosed(t)).await.unwrap();
              }
              Err(e) => {
                tracing::info!("channel from backend closed: {}", e);
                out.send(Message::Exit).await.unwrap();
                break;
              }
            }
          }
        })),
      )
    },
    Daemon::update,
    Daemon::view,
  )
  .settings(Settings {
    id: Some(APP_ID.into()),
    ..Default::default()
  })
  .title(|daemon: &Daemon, _| {
    if let Some(active_popup) = daemon.active_popup.as_ref()
      && let Some(app_id) = active_popup.app_id.as_ref()
    {
      format!("Share with {}", app_id)
    } else {
      "Kagayaku".into()
    }
  })
  .subscription(Daemon::subscription)
  .run()
}
