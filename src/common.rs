use ashpd::{desktop::screencast::SourceType, enumflags2::BitFlags};
use async_channel::Sender;

use crate::backend::ScreencastStream;

pub struct PopupData {
    pub dbus_tx: Sender<Vec<ScreencastStream>>,
    pub source_type: BitFlags<SourceType>,
}
