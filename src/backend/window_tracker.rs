use std::collections::HashMap;

use anyhow::{Context, Error as AnyError};
use futures_util::StreamExt;
use zbus::Connection;

use crate::backend::generated::org_gnome_shell_introspect::{
    IntrospectProxy, WindowsChangedStream,
};

#[derive(Clone)]
pub struct Window {
    pub app_id: String,
    pub title: String,
}

pub struct WindowStateTracker {
    proxy: IntrospectProxy<'static>,
    changed_stream: WindowsChangedStream,
    windows: HashMap<u64, Window>,
}

impl WindowStateTracker {
    pub async fn new(conn: &Connection) -> Result<Self, AnyError> {
        let proxy = IntrospectProxy::new(conn).await?;
        let changed_stream = proxy.receive_windows_changed().await?;
        let mut tracker = Self {
            proxy,
            changed_stream,
            windows: HashMap::new(),
        };
        tracker
            .refresh()
            .await
            .context("failed to fetch window state")?;

        Ok(tracker)
    }

    pub async fn refresh(&mut self) -> Result<(), AnyError> {
        let mut windows = HashMap::new();
        let proxy_resp = self.proxy.get_windows().await?;

        for (wid, window) in proxy_resp.iter() {
            let app_id = window
                .get("app-id")
                .unwrap()
                .downcast_ref::<&str>()
                .unwrap()
                .into();
            let title = window
                .get("title")
                .unwrap()
                .downcast_ref::<&str>()
                .unwrap()
                .into();

            windows.insert(*wid, Window { app_id, title });
        }

        self.windows = windows;

        Ok(())
    }

    pub async fn has_changed(&mut self) -> bool {
        if self.changed_stream.next().await.is_none() {
            return false;
        }

        loop {
            if self.changed_stream.next().await.is_none() {
                break;
            }
        }

        true
    }

    pub fn windows(&self) -> &HashMap<u64, Window> {
        &self.windows
    }
}
