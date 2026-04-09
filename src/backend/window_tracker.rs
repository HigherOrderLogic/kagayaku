use std::collections::HashMap;

use anyhow::{Context, Error as AnyError};
use zbus::Connection;

use crate::backend::generated::org_gnome_shell_introspect::IntrospectProxy;

#[derive(Clone)]
pub struct Window {
  pub app_id: String,
  pub title: String,
}

pub struct WindowStateTracker {
  proxy: IntrospectProxy<'static>,
  windows: HashMap<u64, Window>,
}

impl WindowStateTracker {
  pub async fn new(conn: &Connection) -> Result<Self, AnyError> {
    let proxy = IntrospectProxy::new(conn).await?;
    let mut tracker = Self {
      proxy,
      windows: HashMap::new(),
    };
    tracker.refresh().await.context("failed to fetch window state")?;

    Ok(tracker)
  }

  pub async fn refresh(&mut self) -> Result<(), AnyError> {
    let mut windows = HashMap::new();
    let proxy_resp = self.proxy.get_windows().await?;

    for (wid, window) in proxy_resp.iter() {
      let app_id = window.get("app-id").unwrap().downcast_ref::<&str>().unwrap().into();
      let title = window.get("title").unwrap().downcast_ref::<&str>().unwrap().into();

      windows.insert(*wid, Window { app_id, title });
    }

    self.windows = windows;

    Ok(())
  }

  pub fn windows(&self) -> &HashMap<u64, Window> {
    &self.windows
  }
}
