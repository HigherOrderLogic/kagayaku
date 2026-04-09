use std::collections::{HashMap, HashSet, VecDeque};

use ashpd::{
    desktop::{PersistMode, screencast::SourceType},
    enumflags2::BitFlags,
};
use async_channel::{Receiver, Sender};
use iced::{
    Alignment, Element, Font, Length, Settings, Size, Subscription, Task, daemon, exit,
    font::Weight,
    widget::{
        self, button, checkbox, column, container, grid, rich_text, row, scrollable, space, span,
        text,
    },
    window::{self, Level, close_requests},
};
use tracing::instrument;

use crate::{
    backend::{display_tracker::Monitor, window_tracker::Window},
    common::{PopupData, ScreencastStreamChoice, ToBackendMessage},
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
    PopupWindowOpened(window::Id),
    PopupCloseRequested(window::Id),
    ToggleChoice(ChoiceType, bool),
    ToggleInclude(IncludeType, bool),
    ToggleRemember(bool),
    Cancel,
    Share,
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
    app_id: Option<String>,
    backend_tx: Sender<ToBackendMessage>,
    multiple: bool,
    source_type: BitFlags<SourceType, u32>,
    persist_mode: PersistMode,
    monitors: HashMap<String, Monitor>,
    windows: HashMap<u64, Window>,
    state: State,
    window_id: window::Id,
}

struct Daemon {
    ui_rx: Receiver<PopupData>,
    active_popup: Option<ActivePopup>,
    queued_popups: VecDeque<PopupData>,
}

impl Daemon {
    fn activate_popup(&mut self, popup_data: PopupData) -> Task<Message> {
        let PopupData {
            session_token,
            app_id,
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
            app_id,
            backend_tx,
            multiple,
            source_type,
            persist_mode,
            monitors,
            windows,
            state: Default::default(),
            window_id,
        });

        open_task.map(Message::PopupWindowOpened)
    }

    fn close_active_with(&mut self, backend_message: ToBackendMessage) -> Task<Message> {
        let Some(active_popup) = self.active_popup.take() else {
            tracing::warn!("there's no popup to close");
            return Task::none();
        };

        let _ = active_popup.backend_tx.send_blocking(backend_message);
        let close_task = window::close(active_popup.window_id);

        if let Some(next_popup) = self.queued_popups.pop_front() {
            tracing::info!("opening queued popup request");
            Task::batch([close_task, self.activate_popup(next_popup)])
        } else {
            Task::batch([close_task, {
                let ui_rx = self.ui_rx.clone();
                Task::perform(
                    async move { ui_rx.recv().await.ok() },
                    Message::PopupReceived,
                )
            }])
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
            Message::PopupWindowOpened(_window_id) => Task::none(),
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

                self.close_active_with(ToBackendMessage::Success((
                    active_popup.state.remember_choice,
                    choices,
                )))
            }
        }
    }

    fn view(&self, _window_id: window::Id) -> Element<'_, Message> {
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

        if active_popup.source_type.contains(SourceType::Monitor)
            && active_popup.state.include_monitor
        {
            for (connector, monitor) in &active_popup.monitors {
                let selected = active_popup.state.selected_monitors.contains(connector);
                let monitor_type = if monitor.builtin {
                    "Built-in"
                } else {
                    "External"
                };
                let body_text = if let Some((width, height)) = monitor.size {
                    text!("{} display ({}x{})", monitor_type, width, height)
                } else {
                    text!("{} display (unknown size)", monitor_type)
                };

                choices.push(
                    button(
                        column![
                            container(checkbox(selected)).center(Length::Fill),
                            text!(
                                "{}",
                                monitor.display_name.as_ref().unwrap_or(&monitor.product)
                            )
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

        if active_popup.source_type.contains(SourceType::Window)
            && active_popup.state.include_window
        {
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
                    .on_press(Message::ToggleChoice(
                        ChoiceType::Window(*window_id),
                        !selected,
                    ))
                    .into(),
                );
            }
        }

        let mut filter_children = Vec::new();
        filter_children.push("Include: ".into());

        for source_type in active_popup.source_type {
            let (state, label, include_type) = match source_type {
                SourceType::Monitor => (
                    active_popup.state.include_monitor,
                    "Monitor",
                    IncludeType::Monitor,
                ),
                SourceType::Window => (
                    active_popup.state.include_window,
                    "Window",
                    IncludeType::Window,
                ),
                SourceType::Virtual => (
                    active_popup.state.include_virtual,
                    "Virtual",
                    IncludeType::Virtual,
                ),
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
}

pub fn ui_main(ui_rx: Receiver<PopupData>) -> iced::Result {
    tracing::info!("starting UI loop");
    daemon(
        move || {
            let ui_rx_clone = ui_rx.clone();
            (
                Daemon {
                    ui_rx: ui_rx.clone(),
                    active_popup: None,
                    queued_popups: VecDeque::new(),
                },
                Task::perform(
                    async move { ui_rx_clone.recv().await.ok() },
                    Message::PopupReceived,
                ),
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
    .subscription(|state| {
        if state.active_popup.is_some() {
            close_requests().map(Message::PopupCloseRequested)
        } else {
            Subscription::none()
        }
    })
    .run()
}
