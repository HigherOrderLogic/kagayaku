use std::collections::HashMap;

use ashpd::{
    desktop::{PersistMode, screencast::SourceType},
    enumflags2::BitFlags,
};
use async_channel::Sender;

use crate::backend::{display_tracker::Monitor, window_tracker::Window};

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

pub enum ToBackendMessage {
    Success((bool, Vec<ScreencastStreamChoice>)),
    Cancel,
}

pub struct PopupData {
    pub session_token: String,
    pub app_id: Option<String>,
    pub backend_tx: Sender<ToBackendMessage>,
    pub multiple: bool,
    pub source_type: BitFlags<SourceType>,
    pub persist_mode: PersistMode,
    pub monitors: HashMap<String, Monitor>,
    pub windows: HashMap<u64, Window>,
}
