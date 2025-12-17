use std::{
    collections::{HashMap, HashSet},
    thread::Builder as ThreadBuilder,
};

use ashpd::{
    desktop::{PersistMode, screencast::SourceType},
    enumflags2::BitFlags,
};
use async_channel::{Receiver, Sender};
use iced::{
    Alignment, Element, Font, Length, Settings, Task,
    alignment::{Horizontal, Vertical},
    application, exit,
    font::Weight,
    widget::{button, checkbox, column, container, grid, rich_text, row, scrollable, span, text},
    window::Level,
};

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

struct App {
    app_id: Option<String>,
    backend_tx: Sender<ToBackendMessage>,
    multiple: bool,
    source_type: BitFlags<SourceType, u32>,
    persist_mode: PersistMode,
    monitors: HashMap<String, Monitor>,
    windows: HashMap<u64, Window>,
    state: State,
}

impl App {
    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::ToggleChoice(c, b) => {
                match c {
                    ChoiceType::Monitor(connector) => {
                        if b {
                            if self.multiple || self.state.selected_count() < 1 {
                                self.state.selected_monitors.insert(connector);
                            }
                        } else {
                            self.state.selected_monitors.remove(&connector);
                        }
                    }
                    ChoiceType::Window(wid) => {
                        if b {
                            if self.multiple || self.state.selected_count() < 1 {
                                self.state.selected_windows.insert(wid);
                            }
                        } else {
                            self.state.selected_windows.remove(&wid);
                        }
                    }
                }
                Task::none()
            }
            Message::ToggleInclude(t, b) => {
                match t {
                    IncludeType::Monitor => self.state.include_monitor = b,
                    IncludeType::Window => self.state.include_window = b,
                    IncludeType::Virtual => self.state.include_virtual = b,
                }
                Task::none()
            }
            Message::ToggleRemember(b) => {
                self.state.remember_choice = b;
                Task::none()
            }
            Message::Cancel => {
                let _ = self.backend_tx.send_blocking(ToBackendMessage::Cancel);
                exit()
            }
            Message::Share => {
                let mut res = Vec::new();
                for connector in &self.state.selected_monitors {
                    let monitor = self.monitors.get(connector).unwrap();
                    res.push(ScreencastStreamChoice::Monitor {
                        connector: connector.to_string(),
                        match_string: monitor.match_string(),
                    })
                }
                for wid in &self.state.selected_windows {
                    let window = self.windows.get(wid).unwrap();
                    res.push(ScreencastStreamChoice::Window {
                        window_id: *wid,
                        app_id: window.app_id.to_string(),
                        title: window.title.to_string(),
                    });
                }

                let _ = self.backend_tx.send_blocking(ToBackendMessage::Success((
                    self.state.remember_choice,
                    Vec::new(),
                )));
                exit()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let prompt: Element<_> = if let Some(s) = self.app_id.as_ref() {
            rich_text![
                "Choose what to share with ",
                span::<(), _>(s).font(Font {
                    weight: Weight::Bold,
                    ..Default::default()
                }),
                ":"
            ]
            .into()
        } else {
            "Choose what to share with the requesting application:".into()
        };

        let mut choices = Vec::new();

        if self.source_type.contains(SourceType::Monitor) && self.state.include_monitor {
            for (connector, monitor) in self.monitors.iter() {
                let selected = self.state.selected_monitors.contains(connector);
                let monitor_type = if monitor.builtin {
                    "Built-in"
                } else {
                    "External"
                };
                let body_text = if let Some((w, h)) = monitor.size {
                    text!("{} display ({}x{})", monitor_type, w, h)
                } else {
                    text!("{} display (unknown size)", monitor_type)
                };
                choices.push(
                    button(column![
                        row![
                            text!(
                                "{}",
                                monitor.display_name.as_ref().unwrap_or(&monitor.product)
                            )
                            .width(Length::Fill),
                            checkbox(selected)
                        ]
                        .spacing(2),
                        body_text
                            .align_x(Alignment::Center)
                            .align_y(Vertical::Center)
                            .height(Length::Fill)
                    ])
                    .on_press(Message::ToggleChoice(
                        ChoiceType::Monitor(connector.to_string()),
                        !selected,
                    ))
                    .into(),
                );
            }
        }
        if self.source_type.contains(SourceType::Window) && self.state.include_window {
            for (wid, window) in self.windows.iter() {
                let selected = self.state.selected_windows.contains(wid);
                choices.push(
                    button(column![
                        row![
                            text!("{}", window.title).width(Length::Fill),
                            checkbox(selected)
                        ]
                        .spacing(2),
                        text!("{}", window.app_id)
                            .align_x(Alignment::Center)
                            .align_y(Vertical::Center)
                            .height(Length::Fill)
                    ])
                    .on_press(Message::ToggleChoice(ChoiceType::Window(*wid), !selected))
                    .into(),
                );
            }
        }

        let mut filter_children = Vec::new();
        filter_children.push("Include: ".into());

        for ty in self.source_type {
            let (state, label, include_type) = match ty {
                SourceType::Monitor => {
                    (self.state.include_monitor, "Monitor", IncludeType::Monitor)
                }
                SourceType::Window => (self.state.include_window, "Window", IncludeType::Window),
                SourceType::Virtual => {
                    (self.state.include_virtual, "Virtual", IncludeType::Virtual)
                }
            };
            filter_children.push(
                checkbox(state)
                    .label(label)
                    .on_toggle(move |b| Message::ToggleInclude(include_type, b))
                    .into(),
            );
        }

        let last_row = if self.persist_mode == PersistMode::DoNot {
            row![]
        } else {
            row![
                checkbox(self.state.remember_choice)
                    .label("Remember this choice")
                    .on_toggle(Message::ToggleRemember)
            ]
        }
        .extend([
            button("Share").on_press(Message::Share).into(),
            button("Cancel").on_press(Message::Cancel).into(),
        ])
        .spacing(4);

        column![
            prompt,
            scrollable(grid(choices).spacing(2).fluid(120))
                .auto_scroll(true)
                .height(Length::Fill),
            row(filter_children),
            container(last_row).align_x(Horizontal::Right)
        ]
        .spacing(4)
        .padding(4)
        .into()
    }
}

pub fn ui_main(ui_rx: Receiver<PopupData>) {
    loop {
        match ui_rx.recv_blocking() {
            Ok(d) => {
                let PopupData {
                    session_token,
                    app_id,
                    backend_tx,
                    multiple,
                    source_type,
                    persist_mode,
                    monitors,
                    windows,
                } = d;
                let backend_tx_clone = backend_tx.clone();
                if let Err(e) = ThreadBuilder::new()
                    .name(format!("ui-{}", session_token))
                    .spawn(move || {
                        tracing::info!("starting ui thread");

                        let backend_tx_clone = backend_tx.clone();
                        if let Err(e) = application(
                            move || {
                                let app = App {
                                    app_id: app_id.clone(),
                                    backend_tx: backend_tx.clone(),
                                    multiple,
                                    source_type,
                                    persist_mode,
                                    monitors: monitors.clone(),
                                    windows: windows.clone(),
                                    state: Default::default(),
                                };
                                (app, Task::none())
                            },
                            App::update,
                            App::view,
                        )
                        .settings(Settings {
                            id: Some("com.hol.kagayaku".into()),
                            ..Default::default()
                        })
                        .centered()
                        .window_size((360, 660))
                        .resizable(false)
                        .level(Level::AlwaysOnTop)
                        .run()
                        {
                            tracing::warn!("ui thread finished unsucessfully: {}", e);
                        }
                        let _ = backend_tx_clone.send_blocking(ToBackendMessage::Cancel);
                    })
                {
                    tracing::warn!("failed to start UI thread: {}", e);
                    let _ = backend_tx_clone.send_blocking(ToBackendMessage::Cancel);
                }
            }
            Err(e) => {
                tracing::error!("failed to receive data from backend: {}", e);
                break;
            }
        }
    }
}
