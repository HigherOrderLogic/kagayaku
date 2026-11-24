use std::collections::HashMap;

use ashpd::desktop::{HandleToken, PersistMode, screencast::SourceType};
use smol::channel::Sender;

use crate::backend::{display_tracker::Monitor, window_tracker::Window};

#[derive(Clone)]
pub enum ScreencastStreamChoice {
  Monitor {
    connector: String,
    match_string: String,
  },
  Window {
    window_id: u64,
    app_id: String,
    title: String,
  },
}

#[derive(Clone)]
pub enum ToBackendMessage {
  Success((bool, Vec<ScreencastStreamChoice>)),
  Cancel,
}

pub enum ToUiMessage {
  NewPopup(PopupData),
  // TODO: rename to `SessionClosed`
  CloseSession(HandleToken),
  RequestClosed(HandleToken),
}

pub struct PopupData {
  pub session_token: HandleToken,
  pub popup_type: PopupType,
}

pub enum PopupType {
  Screencast(ScreencastData, Option<String>),
}

pub struct ScreencastData {
  pub app_id: Option<String>,
  pub backend_tx: Sender<ToBackendMessage>,
  pub multiple: bool,
  pub source_type: SourceType,
  pub persist_mode: PersistMode,
  pub monitors: HashMap<String, Monitor>,
  pub windows: HashMap<u64, Window>,
}
