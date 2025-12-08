use std::collections::HashMap;

use anyhow::{Context, Error as AnyError};
use zbus::Connection;

use crate::dbus::org_gnome_mutter_displayconfig::DisplayConfigProxy;

#[derive(Clone)]
pub struct Monitor {
    connector: String,
    vendor: String,
    product: String,
    serial: String,
    display_name: String,
    builtin: bool,
    width: i32,
    height: i32,
}

impl Monitor {
    pub fn match_string(&self) -> String {
        if self.vendor == "unknown" && self.product == "unknown" && self.serial == "unknown" {
            self.connector.to_string()
        } else {
            format!("{}:{}:{}", self.vendor, self.product, self.serial)
        }
    }

    pub fn connector(&self) -> String {
        self.connector.to_string()
    }
}

pub struct DisplayStateTracker {
    proxy: DisplayConfigProxy<'static>,
    monitors: HashMap<String, Monitor>,
}

impl DisplayStateTracker {
    pub async fn new(conn: &Connection) -> Result<Self, AnyError> {
        let proxy = DisplayConfigProxy::new(conn).await?;
        let mut tracker = Self {
            proxy,
            monitors: HashMap::new(),
        };
        tracker
            .refresh()
            .await
            .context("failed to fetch display state")?;

        Ok(tracker)
    }

    pub async fn refresh(&mut self) -> Result<(), AnyError> {
        self.monitors.clear();

        let (_, monitors_data, _, _) = self.proxy.get_current_state().await?;

        for ((connector, vendor, product, serial), modes, props) in monitors_data {
            let display_name = if let Some(v) = props.get("display-name") {
                v.downcast_ref::<&str>()
                    .context("display-name")?
                    .to_string()
            } else {
                connector.to_string()
            };
            let builtin = if let Some(v) = props.get("is-builtin") {
                v.downcast_ref().context("is-builtin")?
            } else {
                false
            };
            let (width, height) = modes
                .iter()
                .find(|(_, _, _, _, _, _, p)| {
                    p.get("is-current")
                        .map_or(false, |v| v.downcast_ref().unwrap_or(false))
                })
                .map_or((0, 0), |(_, w, h, _, _, _, _)| (*w, *h));

            self.monitors.insert(
                connector.to_string(),
                Monitor {
                    connector,
                    vendor,
                    product,
                    serial,
                    display_name,
                    builtin,
                    width,
                    height,
                },
            );
        }

        Ok(())
    }

    pub fn find_monitor(&self, match_string: &str) -> Option<&Monitor> {
        self.monitors
            .values()
            .find(|m| m.match_string() == match_string)
    }
}
