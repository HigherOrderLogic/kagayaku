mod common;
mod dbus;
mod display_state_tracker;

use std::{future::pending, thread::available_parallelism};

use anyhow::{Context, Error as AnyError};
use ashpd::backend::Builder;
use async_channel::{Sender, unbounded};
use async_global_executor::{GlobalExecutorConfig, block_on, init_with_config};
use zbus::fdo::RequestNameFlags;

use crate::{common::PopupData, dbus::ScreencastBackend};

fn main() -> Result<(), AnyError> {
    init_with_config(
        GlobalExecutorConfig::default()
            .with_max_threads(available_parallelism().map_or(1, |n| n.get())),
    );

    let (tx, rx) = unbounded();
    block_on(async_main(tx)).context("main function returns error")
}

async fn async_main(tx: Sender<PopupData>) -> Result<(), AnyError> {
    Builder::new("org.freedesktop.impl.portal.desktop.kagayaku")
        .context("failed to create builder")?
        .with_flags(
            RequestNameFlags::AllowReplacement
                | RequestNameFlags::DoNotQueue
                | RequestNameFlags::ReplaceExisting,
        )
        .screencast(ScreencastBackend::new(tx).await?)
        .build()
        .await
        .context("failed to build DBus backend")?;

    tracing::debug!("starting loop");

    loop {
        pending::<()>().await;
    }
}
