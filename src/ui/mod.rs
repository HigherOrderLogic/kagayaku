mod wayland;

use std::collections::{HashMap, HashSet, VecDeque};

use ashpd::{
  desktop::{PersistMode, screencast::SourceType},
  enumflags2::BitFlags,
};
use async_channel::{Receiver, Sender};
use futures_util::SinkExt;
use iced::{
  Alignment, Element, Font, Length, Settings, Size, Subscription, Task, daemon, exit,
  font::Weight,
  stream,
  wgpu::rwh::{RawDisplayHandle, RawWindowHandle},
  widget::{self, button, checkbox, column, container, grid, rich_text, row, scrollable, space, span, text},
  window::{self, Level, close_requests},
};
use sctk::reexports::{
  client::{
    Connection, Proxy,
    backend::{Backend, ObjectId},
    globals::registry_queue_init,
    protocol::wl_surface::WlSurface,
  },
  protocols::xdg::foreign::zv2::client::zxdg_importer_v2::ZxdgImporterV2,
};
use tracing::instrument;

use crate::{
  backend::{display_tracker::Monitor, window_tracker::Window},
  common::{PopupData, ScreencastStreamChoice, ToBackendMessage, ToUiMessage},
  ui::wayland::WaylandState,
};

#[derive(Clone, Copy)]
enum IncludeType {
  Monitor,
  Window,
  Virtual,
}

#[derive(Clone)]
enum ChoiceType {
  Monitor(String),
  Window(u64),
}

#[derive(Clone)]
enum Message {
  PopupReceived(Option<PopupData>),
  PopupCloseRequested(window::Id),
  PopupSessionClosed(String),
  ToggleChoice(ChoiceType, bool),
  ToggleInclude(IncludeType, bool),
  ToggleRemember(bool),
  Cancel,
  Share,
  WaylandReady(Connection, WlSurface, String),
  Exit,
  None,
}

struct State {
  include_monitor: bool,
  include_window: bool,
  include_virtual: bool,
  selected_monitors: HashSet<String>,
  selected_windows: HashSet<u64>,
  remember_choice: bool,
}

impl Default for State {
  fn default() -> Self {
    Self {
      include_monitor: true,
      include_window: true,
      include_virtual: true,
      selected_monitors: HashSet::new(),
      selected_windows: HashSet::new(),
      remember_choice: true,
    }
  }
}

impl State {
  fn selected_count(&self) -> usize {
    self.selected_monitors.len() + self.selected_windows.len()
  }
}

struct ActivePopup {
  session_token: String,
  app_id: Option<String>,
  backend_tx: Sender<ToBackendMessage>,
  multiple: bool,
  source_type: BitFlags<SourceType, u32>,
  persist_mode: PersistMode,
  monitors: HashMap<String, Monitor>,
  windows: HashMap<u64, Window>,
  state: State,
  window_id: window::Id,
  parent_set: bool,
}

struct Daemon {
  active_popup: Option<ActivePopup>,
  queued_popups: VecDeque<PopupData>,
}

impl Daemon {
  fn activate_popup(&mut self, popup_data: PopupData) -> Task<Message> {
    let PopupData {
      session_token,
      app_id,
      parent_window,
      backend_tx,
      multiple,
      source_type,
      persist_mode,
      monitors,
      windows,
    } = popup_data;

    tracing::info!("starting ui popup for {}", session_token);

    let (window_id, open_task) = window::open(window::Settings {
      size: Size::new(360.0, 660.0),
      position: window::Position::Centered,
      level: Level::AlwaysOnTop,
      exit_on_close_request: false,
      ..Default::default()
    });

    self.active_popup = Some(ActivePopup {
      session_token,
      app_id,
      backend_tx,
      multiple,
      source_type,
      persist_mode,
      monitors,
      windows,
      state: Default::default(),
      window_id,
      parent_set: false,
    });

    open_task.then(move |id| {
      if let Some(parent) = parent_window.clone() {
        window::run(id, |w| {
          let Ok(RawWindowHandle::Wayland(window_handle)) = w.window_handle().map(|h| h.as_raw()) else {
            return Message::None;
          };
          let Ok(RawDisplayHandle::Wayland(display_handle)) = w.display_handle().map(|h| h.as_raw()) else {
            return Message::None;
          };
          let backend = unsafe { Backend::from_foreign_display(display_handle.display.as_ptr().cast()) };
          let conn = Connection::from_backend(backend);
          let surface_id =
            match unsafe { ObjectId::from_ptr(WlSurface::interface(), window_handle.surface.as_ptr().cast()) } {
              Ok(sid) => sid,
              Err(_) => return Message::None,
            };
          let Ok(surface) = WlSurface::from_id(&conn, surface_id) else {
            tracing::warn!("invalid wl_surface id");
            return Message::None;
          };
          Message::WaylandReady(conn, surface, parent)
        })
      } else {
        tracing::info!("no parent window to associate");
        Task::none()
      }
    })
  }

  fn close_active_with(&mut self, backend_message: ToBackendMessage) -> Task<Message> {
    let Some(active_popup) = self.active_popup.take() else {
      tracing::warn!("there's no popup to close");
      return Task::none();
    };

    if let Err(e) = active_popup.backend_tx.try_send(backend_message) {
      tracing::warn!("failed to send message to backend: {}", e);
    }
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
        if let Some(active_popup) = self.active_popup.as_ref()
          && active_popup.session_token == session_token
        {
          if active_popup.parent_set {
            tracing::info!(
              "popup for session {} is associated with a parent, ignoring",
              session_token
            );
            return Task::none();
          } else {
            tracing::info!("popup cancelled by backend for session {}", session_token);
            return self.close_active_with(ToBackendMessage::Cancel);
          }
        } else {
          self.queued_popups.retain(|p| p.session_token != session_token);
        }

        Task::none()
      }
      Message::ToggleChoice(choice_type, selected) => {
        let Some(active_popup) = self.active_popup.as_mut() else {
          return Task::none();
        };

        match choice_type {
          ChoiceType::Monitor(connector) => {
            if selected {
              if !active_popup.multiple {
                active_popup.state.selected_monitors.clear();
                active_popup.state.selected_windows.clear();
              }
              active_popup.state.selected_monitors.insert(connector);
            } else {
              active_popup.state.selected_monitors.remove(&connector);
            }
          }
          ChoiceType::Window(window_id) => {
            if selected {
              if !active_popup.multiple {
                active_popup.state.selected_monitors.clear();
                active_popup.state.selected_windows.clear();
              }
              active_popup.state.selected_windows.insert(window_id);
            } else {
              active_popup.state.selected_windows.remove(&window_id);
            }
          }
        }

        Task::none()
      }
      Message::ToggleInclude(include_type, include) => {
        let Some(active_popup) = self.active_popup.as_mut() else {
          return Task::none();
        };

        match include_type {
          IncludeType::Monitor => active_popup.state.include_monitor = include,
          IncludeType::Window => active_popup.state.include_window = include,
          IncludeType::Virtual => active_popup.state.include_virtual = include,
        }

        Task::none()
      }
      Message::ToggleRemember(remember_choice) => {
        let Some(active_popup) = self.active_popup.as_mut() else {
          return Task::none();
        };

        active_popup.state.remember_choice = remember_choice;
        Task::none()
      }
      Message::Cancel => {
        tracing::info!("cancel button pressed, cancelling request");
        self.close_active_with(ToBackendMessage::Cancel)
      }
      Message::Share => {
        let Some(active_popup) = self.active_popup.as_ref() else {
          return Task::none();
        };

        let mut choices = Vec::new();

        for connector in &active_popup.state.selected_monitors {
          if let Some(monitor) = active_popup.monitors.get(connector) {
            choices.push(ScreencastStreamChoice::Monitor {
              connector: connector.to_string(),
              match_string: monitor.match_string(),
            });
          }
        }

        for window_id in &active_popup.state.selected_windows {
          if let Some(window) = active_popup.windows.get(window_id) {
            choices.push(ScreencastStreamChoice::Window {
              window_id: *window_id,
              app_id: window.app_id.to_string(),
              title: window.title.to_string(),
            });
          }
        }
        tracing::info!("sharing screencast request");

        self.close_active_with(ToBackendMessage::Success((active_popup.state.remember_choice, choices)))
      }
      Message::WaylandReady(conn, surface, parent) => {
        let Ok((globals, event_queue)) = registry_queue_init::<WaylandState>(&conn) else {
          tracing::warn!("failed to init registry queue");
          return Task::none();
        };
        let qh = event_queue.handle();
        if let Ok(importer) = globals.bind::<ZxdgImporterV2, _, _>(&qh, 1..=1, ()) {
          importer.import_toplevel(parent, &qh, ()).set_parent_of(&surface);
          if let Err(e) = event_queue.flush() {
            tracing::warn!("failed to send Wayland request to compositor: {}", e);
          } else if let Some(popup) = self.active_popup.as_mut() {
            popup.parent_set = true;
          }
        } else {
          tracing::warn!("failed to bind {}", ZxdgImporterV2::interface().name);
        }

        Task::none()
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

  fn view(&self, _: window::Id) -> Element<'_, Message> {
    let Some(active_popup) = self.active_popup.as_ref() else {
      return text("Waiting for screencast request...").into();
    };

    let prompt: Element<_> = if let Some(ref app_id) = active_popup.app_id {
      rich_text![
        "Choose what to share with ",
        span::<(), _>(app_id).font(Font {
          weight: Weight::Bold,
          ..Default::default()
        }),
        ":"
      ]
      .into()
    } else {
      "Choose what to share with the requesting application:".into()
    };

    let mut choices: Vec<Element<'_, Message>> = Vec::new();

    if active_popup.source_type.contains(SourceType::Monitor) && active_popup.state.include_monitor {
      for (connector, monitor) in &active_popup.monitors {
        let selected = active_popup.state.selected_monitors.contains(connector);
        let monitor_type = if monitor.builtin { "Built-in" } else { "External" };
        let body_text = if let Some((width, height)) = monitor.size {
          text!("{} display ({}x{})", monitor_type, width, height)
        } else {
          text!("{} display (unknown size)", monitor_type)
        };

        choices.push(
          button(
            column![
              container(checkbox(selected)).center(Length::Fill),
              text!("{}", monitor.display_name.as_ref().unwrap_or(&monitor.product))
                .font(Font {
                  weight: Weight::Bold,
                  ..Default::default()
                })
                .align_x(Alignment::Center)
                .width(Length::Fill),
              body_text.align_x(Alignment::Center).width(Length::Fill)
            ]
            .spacing(4),
          )
          .on_press(Message::ToggleChoice(
            ChoiceType::Monitor(connector.to_string()),
            !selected,
          ))
          .into(),
        );
      }
    }

    if active_popup.source_type.contains(SourceType::Window) && active_popup.state.include_window {
      for (window_id, window) in &active_popup.windows {
        let selected = active_popup.state.selected_windows.contains(window_id);
        choices.push(
          button(
            column![
              container(checkbox(selected)).center(Length::Fill),
              text!("{}", window.title)
                .font(Font {
                  weight: Weight::Bold,
                  ..Default::default()
                })
                .align_x(Alignment::Center)
                .width(Length::Fill),
              text!("{}", window.app_id)
                .align_x(Alignment::Center)
                .width(Length::Fill)
            ]
            .spacing(4),
          )
          .on_press(Message::ToggleChoice(ChoiceType::Window(*window_id), !selected))
          .into(),
        );
      }
    }

    let mut filter_children = Vec::new();
    filter_children.push("Include: ".into());

    for source_type in active_popup.source_type {
      let (state, label, include_type) = match source_type {
        SourceType::Monitor => (active_popup.state.include_monitor, "Monitor", IncludeType::Monitor),
        SourceType::Window => (active_popup.state.include_window, "Window", IncludeType::Window),
        SourceType::Virtual => (active_popup.state.include_virtual, "Virtual", IncludeType::Virtual),
      };

      filter_children.push(
        checkbox(state)
          .label(label)
          .on_toggle(move |include| Message::ToggleInclude(include_type, include))
          .into(),
      );
    }

    let share_button: Element<_> = if active_popup.state.selected_count() > 0 {
      button("Share").on_press(Message::Share).into()
    } else {
      button("Share").into()
    };

    let cancel_button: Element<_> = button("Cancel").on_press(Message::Cancel).into();

    let bottom_row = if active_popup.persist_mode == PersistMode::DoNot {
      row![]
    } else {
      row![
        checkbox(active_popup.state.remember_choice)
          .label("Remember this choice")
          .on_toggle(Message::ToggleRemember)
      ]
    }
    .push(space::horizontal())
    .push(share_button)
    .push(cancel_button)
    .width(Length::Fill)
    .spacing(4);

    column![
      prompt,
      scrollable(
        grid(choices)
          .spacing(4)
          .columns(3)
          .height(widget::grid::aspect_ratio(16, 9)),
      )
      .auto_scroll(true)
      .height(Length::Fill)
      .width(Length::Fill),
      row(filter_children).spacing(4),
      bottom_row
    ]
    .spacing(4)
    .padding(4)
    .into()
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
        },
        Task::stream(stream::channel(10, async move |mut out| {
          let mut stop = false;
          while !stop {
            match ui_rx_clone.recv().await {
              Ok(ToUiMessage::NewPopup(d)) => {
                out.send(Message::PopupReceived(Some(d))).await.unwrap();
              }
              Ok(ToUiMessage::CloseSession(t)) => {
                out.send(Message::PopupSessionClosed(t)).await.unwrap();
              }
              Err(e) => {
                tracing::info!("channel from backend closed: {}", e);
                stop = true;
                out.send(Message::Exit).await.unwrap();
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
    id: Some("com.hol.kagayaku".into()),
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
