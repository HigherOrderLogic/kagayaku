use std::collections::HashMap;

use ashpd::{desktop::screencast::SourceType, enumflags2::BitFlags};
use async_channel::Sender;

use crate::backend::{ScreencastStream, display_tracker::Monitor, window_tracker::Window};

pub enum ToBackendMessage {
    Success(Vec<ScreencastStream>),
    Cancel,
}

pub struct PopupData {
    pub session_token: String,
    pub app_id: Option<String>,
    pub backend_tx: Sender<ToBackendMessage>,
    pub multiple: bool,
    pub source_type: BitFlags<SourceType>,
    pub monitors: HashMap<String, Monitor>,
    pub windows: HashMap<u64, Window>,
}
