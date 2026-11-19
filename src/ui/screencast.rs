use std::{
  collections::{HashMap, HashSet},
  env::var,
  path::{Path, PathBuf},
  process::Command,
  sync::{Arc, LazyLock},
};

use ashpd::desktop::{PersistMode, screencast::SourceType};
use freedesktop_entry_parser::Entry;
use freedesktop_icons::{default_theme_gtk, lookup};
use futures_util::AsyncReadExt;
use iced::{
  Alignment, Element, Font, Length, Task,
  advanced::{
    Renderer,
    image::{Handle as ImageHandle, Renderer as ImageRenderer},
    svg::{Handle as SvgHandle, Renderer as SvgRenderer},
    text::Renderer as TextRenderer,
  },
  font::Weight,
  widget::{
    self, Button, button,
    button::{Catalog as ButtonCatalog, StyleFn as ButtonStyleFn},
    checkbox,
    checkbox::Catalog as CheckboxCatalog,
    column, container,
    container::Catalog as ContainerCatalog,
    grid, image, rich_text, row, scrollable, space, span, svg,
    svg::Catalog as SvgCatalog,
    text,
    text::{Catalog as TextCatalog, Wrapping},
  },
};
use iced_palace::widget::ellipsized_text;
use smol::{
  fs::{OpenOptions, metadata},
  lock::RwLock,
  unblock,
};

use crate::{
  backend::{display_tracker::Monitor, window_tracker::Window},
  common::{ScreencastData, ScreencastStreamChoice, ToBackendMessage},
};

static DEFAULT_ICON_THEME: LazyLock<Option<String>> = LazyLock::new(|| {
  default_theme_gtk().or_else(|| {
    ["kreadconfig6", "kreadconfig5"].into_iter().find_map(|cmd| {
      Command::new(cmd)
        .args(["--file", "kdeglobals", "--group", "Icons", "--key", "Theme"])
        .output()
        .ok()
        .and_then(|out| {
          if out.status.success() {
            let theme = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if theme.is_empty() { None } else { theme.into() }
          } else {
            None
          }
        })
    })
  })
});
static IMAGE_MISSING_ICON: LazyLock<Option<Icon>> = LazyLock::new(|| find_icon("image-missing").map(Icon::new));
static COMPUTER_LAPTOP_ICON: LazyLock<Option<Icon>> = LazyLock::new(|| {
  find_icon("computer-laptop")
    .map(Icon::new)
    .or_else(|| IMAGE_MISSING_ICON.clone())
});
static DESKTOP_DISPLAY_ICON: LazyLock<Option<Icon>> = LazyLock::new(|| {
  find_icon("preferences-desktop-display")
    .map(Icon::new)
    .or_else(|| IMAGE_MISSING_ICON.clone())
});

static DESKTOP_PATHS: LazyLock<Vec<PathBuf>> = LazyLock::new(|| {
  let mut paths = Vec::new();

  if let Ok(s) = var("XDG_DATA_HOME").or_else(|_| var("HOME").map(|h| format!("{}/.local/share", h))) {
    paths.push(s);
  }
  paths.extend(
    var("XDG_DATA_DIRS")
      .unwrap_or_else(|_| "/usr/local/share:/usr/share".into())
      .split(':')
      .map(Into::into),
  );

  paths
    .into_iter()
    .map(|s| PathBuf::from(s).join("applications"))
    .collect()
});

async fn get_app_icon_name(desktop_name: impl AsRef<Path>) -> Option<String> {
  for p in DESKTOP_PATHS.iter() {
    let file_path = p.join(&desktop_name);
    if !metadata(&file_path).await.is_ok_and(|m| m.is_file()) {
      continue;
    }

    let mut content = String::new();
    let Ok(mut f) = OpenOptions::new().read(true).open(file_path).await else {
      continue;
    };
    if f.read_to_string(&mut content).await.is_err() {
      continue;
    }

    let Ok(e) = Entry::parse(content.as_bytes()) else {
      continue;
    };
    return e.section("Desktop Entry").and_then(|s| s.attr("Icon").first().cloned());
  }

  None
}

fn find_icon(icon_name: impl AsRef<str>) -> Option<PathBuf> {
  let builder = lookup(icon_name.as_ref()).force_svg().with_size(48);

  if let Some(t) = DEFAULT_ICON_THEME.as_ref() {
    builder.with_theme(t).find()
  } else {
    builder.find()
  }
}

#[derive(Clone, Copy)]
pub enum IncludeType {
  Monitor,
  Window,
  Virtual,
}

#[derive(Clone)]
pub enum ChoiceType {
  Monitor(String),
  Window(u64),
}

#[derive(Clone)]
pub enum Message {
  ToggleChoice(ChoiceType, bool),
  ToggleInclude(IncludeType, bool),
  ToggleRemember(bool),
  Cancel,
  Share,
  SetAppIcon(u64, Option<Icon>),
  Close(ToBackendMessage),
}

struct State {
  include_monitor: bool,
  include_window: bool,
  include_virtual: bool,
  selected_monitors: HashSet<String>,
  selected_windows: HashSet<u64>,
  remember_choice: bool,
}

impl State {
  fn selected_count(&self) -> usize {
    self.selected_monitors.len() + self.selected_windows.len()
  }
}

#[derive(Clone)]
struct WindowChoice {
  window: Window,
  icon: Option<Icon>,
}

impl From<Window> for WindowChoice {
  fn from(window: Window) -> Self {
    Self { window, icon: None }
  }
}

pub struct Screencast {
  app_id: Option<String>,
  multiple: bool,
  source_type: SourceType,
  persist_mode: PersistMode,
  monitors: HashMap<String, Monitor>,
  windows: HashMap<u64, WindowChoice>,
  state: State,
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

impl Screencast {
  pub fn new(data: ScreencastData) -> Self {
    let ScreencastData {
      app_id,
      backend_tx: _,
      multiple,
      source_type,
      persist_mode,
      monitors,
      windows,
    } = data;
    let windows = windows.into_iter().map(|(k, v)| (k, v.into())).collect();

    Self {
      app_id,
      multiple,
      source_type,
      persist_mode,
      monitors,
      windows,
      state: State::default(),
    }
  }

  pub fn update(&mut self, message: Message) -> Task<Message> {
    match message {
      Message::ToggleChoice(choice_type, selected) => {
        match choice_type {
          ChoiceType::Monitor(connector) => {
            if selected {
              if !self.multiple {
                self.state.selected_monitors.clear();
                self.state.selected_windows.clear();
              }
              self.state.selected_monitors.insert(connector);
            } else {
              self.state.selected_monitors.remove(&connector);
            }
          }
          ChoiceType::Window(window_id) => {
            if selected {
              if !self.multiple {
                self.state.selected_monitors.clear();
                self.state.selected_windows.clear();
              }
              self.state.selected_windows.insert(window_id);
            } else {
              self.state.selected_windows.remove(&window_id);
            }
          }
        }

        Task::none()
      }
      Message::ToggleInclude(include_type, include) => {
        match include_type {
          IncludeType::Monitor => self.state.include_monitor = include,
          IncludeType::Window => self.state.include_window = include,
          IncludeType::Virtual => self.state.include_virtual = include,
        }

        Task::none()
      }
      Message::ToggleRemember(remember_choice) => {
        self.state.remember_choice = remember_choice;
        Task::none()
      }
      Message::SetAppIcon(k, v) => {
        if let Some(w) = self.windows.get_mut(&k) {
          w.icon = v;
        }

        Task::none()
      }
      Message::Cancel => Task::done(Message::Close(ToBackendMessage::Cancel)),
      Message::Share => {
        let mut choices = Vec::new();

        for connector in &self.state.selected_monitors {
          if let Some(monitor) = self.monitors.get(connector) {
            choices.push(ScreencastStreamChoice::Monitor {
              connector: connector.to_string(),
              match_string: monitor.match_string(),
            });
          }
        }
        for window_id in &self.state.selected_windows {
          if let Some(window) = self.windows.get(window_id) {
            choices.push(ScreencastStreamChoice::Window {
              window_id: *window_id,
              app_id: window.window.app_id.to_string(),
              title: window.window.title.to_string(),
            });
          }
        }

        Task::done(Message::Close(ToBackendMessage::Success((
          self.state.remember_choice,
          choices,
        ))))
      }
      Message::Close(_) => unreachable!(),
    }
  }

  pub fn view(&self) -> Element<'_, Message> {
    let prompt: Element<_> = if let Some(ref app_id) = self.app_id {
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

    if self.source_type.contains(SourceType::MONITOR) && self.state.include_monitor {
      for (connector, monitor) in &self.monitors {
        let selected = self.state.selected_monitors.contains(connector);
        let monitor_type = if monitor.builtin { "Built-in" } else { "External" };
        let monitor_icon = if monitor.builtin {
          COMPUTER_LAPTOP_ICON.clone()
        } else {
          DESKTOP_DISPLAY_ICON.clone()
        };
        choices.push(
          choice_button(
            selected,
            monitor.display_name.clone().unwrap_or_else(|| monitor.product.clone()),
            format!(
              "{} display ({})",
              monitor_type,
              monitor
                .size
                .map_or_else(|| "unknown size".into(), |(w, h)| format!("{}x{}", w, h)),
            ),
            ChoiceType::Monitor(connector.to_string()),
            monitor_icon,
          )
          .into(),
        );
      }
    }

    if self.source_type.contains(SourceType::WINDOW) && self.state.include_window {
      for (window_id, window) in &self.windows {
        let selected = self.state.selected_windows.contains(window_id);
        choices.push(
          choice_button(
            selected,
            window.window.title.clone(),
            window.window.app_id.clone(),
            ChoiceType::Window(*window_id),
            window.icon.clone(),
          )
          .into(),
        );
      }
    }

    let mut filter_children = Vec::new();
    filter_children.push("Include: ".into());

    for source_type in self.source_type {
      let (state, label, include_type) = match source_type {
        SourceType::MONITOR => (self.state.include_monitor, "Monitor", IncludeType::Monitor),
        SourceType::WINDOW => (self.state.include_window, "Window", IncludeType::Window),
        SourceType::VIRTUAL => (self.state.include_virtual, "Virtual", IncludeType::Virtual),
        _ => continue,
      };

      filter_children.push(
        checkbox(state)
          .label(label)
          .on_toggle(move |include| Message::ToggleInclude(include_type, include))
          .into(),
      );
    }

    let share_button = if self.state.selected_count() > 0 {
      button("Share").on_press(Message::Share)
    } else {
      button("Share")
    };
    let cancel_button = button("Cancel").on_press(Message::Cancel);
    let bottom_row = if self.persist_mode == PersistMode::DoNot {
      row![]
    } else {
      row![
        checkbox(self.state.remember_choice)
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

#[derive(Clone)]
pub enum Icon {
  Svg(SvgHandle),
  Raster(ImageHandle),
}

impl Icon {
  fn new(p: impl AsRef<Path>) -> Self {
    let path = p.as_ref();
    if let Some(ext) = path.extension()
      && ext.to_string_lossy() == "svg"
    {
      Self::Svg(SvgHandle::from_path(path))
    } else {
      Self::Raster(ImageHandle::from_path(path))
    }
  }
}

impl<'a, M, T, R> From<Icon> for Element<'a, M, T, R>
where
  R: SvgRenderer + ImageRenderer,
  T: SvgCatalog + 'a,
  M: 'a,
  <R as ImageRenderer>::Handle: From<ImageHandle> + 'a,
{
  fn from(value: Icon) -> Element<'a, M, T, R> {
    match value {
      Icon::Svg(handle) => Element::new(svg(handle).width(48).height(48)),
      Icon::Raster(handle) => Element::new(image(handle).width(48).height(48)),
    }
  }
}

fn choice_button<'a, T, R>(
  selected: bool,
  title: String,
  subtitle: String,
  choice: ChoiceType,
  icon: Option<Icon>,
) -> Button<'a, Message, T, R>
where
  T: ButtonCatalog + CheckboxCatalog + ContainerCatalog + SvgCatalog + TextCatalog + 'a,
  <T as ButtonCatalog>::Class<'a>: From<ButtonStyleFn<'a, T>>,
  R: Renderer + ImageRenderer + SvgRenderer + TextRenderer + 'a,
  <R as ImageRenderer>::Handle: From<ImageHandle> + 'a,
  <R as TextRenderer>::Font: From<Font>,
  <R as TextRenderer>::Paragraph: Clone,
{
  let center: Element<'_, _, _, _> = if let Some(i) = icon {
    i.into()
  } else {
    space().height(Length::Fill).into()
  };
  button(
    column![
      container(center).center(Length::Fill),
      ellipsized_text(title)
        .font(Font {
          weight: Weight::Bold,
          ..Default::default()
        })
        .align_x(Alignment::Center)
        .wrapping(Wrapping::None)
        .width(Length::Fill),
      text(subtitle).size(12).align_x(Alignment::Center).width(Length::Fill)
    ]
    .spacing(4),
  )
  .on_press(Message::ToggleChoice(choice, !selected))
  .style(move |theme: &T, status| {
    let mut style = ButtonCatalog::style(theme, &<T as ButtonCatalog>::default(), status);

    if selected {
      style.border.width = 2.;
      style.border.color = style.text_color.inverse();
    }

    style
  })
}

pub fn icon_tasks(
  windows: &HashMap<u64, Window>,
  icons_cache: Arc<RwLock<HashMap<String, Option<Icon>>>>,
) -> Vec<Task<Message>> {
  windows
    .iter()
    .map(|(&k, w)| {
      let icons_cache_clone = icons_cache.clone();
      let app_id = w.app_id.clone();
      Task::future(async move {
        let icon = if let Some(i) = icons_cache_clone.read().await.get(&app_id) {
          i.clone()
        } else {
          let icon = if let Some(n) = get_app_icon_name(&app_id).await {
            unblock(move || find_icon(n)).await.map(Icon::new)
          } else {
            IMAGE_MISSING_ICON.clone()
          };

          icons_cache_clone.write().await.insert(app_id, icon.clone());
          icon
        };

        Message::SetAppIcon(k, icon)
      })
    })
    .collect()
}
