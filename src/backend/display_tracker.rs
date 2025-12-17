use std::collections::HashMap;

use anyhow::{Context, Error as AnyError};
use futures_util::StreamExt;
use zbus::Connection;

use super::generated::org_gnome_mutter_displayconfig::{DisplayConfigProxy, MonitorsChangedStream};

#[derive(Clone)]
pub struct Monitor {
    pub connector: String,
    pub vendor: String,
    pub product: String,
    pub serial: String,
    pub display_name: Option<String>,
    pub builtin: bool,
    pub size: Option<(i32, i32)>,
}

impl Monitor {
    pub fn match_string(&self) -> String {
        if self.vendor == "unknown" && self.product == "unknown" && self.serial == "unknown" {
            self.connector.to_string()
        } else {
            format!("{}:{}:{}", self.vendor, self.product, self.serial)
        }
    }
}

pub struct DisplayStateTracker {
    proxy: DisplayConfigProxy<'static>,
    changed_stream: MonitorsChangedStream,
    monitors: HashMap<String, Monitor>,
}

impl DisplayStateTracker {
    pub async fn new(conn: &Connection) -> Result<Self, AnyError> {
        let proxy = DisplayConfigProxy::new(conn).await?;
        let changed_stream = proxy.receive_monitors_changed().await?;
        let mut tracker = Self {
            proxy,
            changed_stream,
            monitors: HashMap::new(),
        };
        tracker
            .refresh()
            .await
            .context("failed to fetch display state")?;

        Ok(tracker)
    }

    pub async fn refresh(&mut self) -> Result<(), AnyError> {
        let mut monitors = HashMap::new();

        let (_, monitors_data, _, _) = self.proxy.get_current_state().await?;

        for ((connector, vendor, product, serial), modes, props) in monitors_data {
            let display_name = props
                .get("display-name")
                .and_then(|v| v.downcast_ref::<&str>().ok())
                .map(|s| s.to_string());
            let builtin = props
                .get("is-builtin")
                .is_some_and(|v| v.downcast_ref().unwrap_or(false));
            let size = modes
                .iter()
                .find(|(_, _, _, _, _, _, p)| {
                    p.get("is-current")
                        .is_some_and(|v| v.downcast_ref().unwrap_or(false))
                })
                .map(|(_, w, h, _, _, _, _)| (*w, *h));

            monitors.insert(
                connector.to_string(),
                Monitor {
                    connector,
                    vendor,
                    product,
                    serial,
                    display_name,
                    builtin,
                    size,
                },
            );
        }

        self.monitors = monitors;

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

    pub fn find_monitor(&self, match_string: &str) -> Option<&Monitor> {
        self.monitors
            .values()
            .find(|m| m.match_string() == match_string)
    }

    pub fn monitors(&self) -> &HashMap<String, Monitor> {
        &self.monitors
    }
}
