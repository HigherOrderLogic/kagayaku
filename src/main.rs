mod backend;
mod common;
mod ui;

use std::{io::stderr, thread::Builder as ThreadBuilder};

use anyhow::{Context, Error as AnyError};
use smol::{block_on, channel::unbounded};
use tracing_subscriber::{EnvFilter, Registry, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{backend::backend_main, ui::ui_main};

fn main() -> Result<(), AnyError> {
  Registry::default()
    .with(
      EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(format!("{}=debug", env!("CARGO_BIN_NAME")))),
    )
    .with(fmt::layer().with_writer(stderr))
    .init();

  let (tx, rx) = unbounded();

  ThreadBuilder::new()
    .name("backend".into())
    .spawn(move || {
      if let Err(e) = block_on(backend_main(tx)) {
        tracing::error!("main function returns error: {}", e);
      }
    })
    .context("failed to spawn backend thread")?;

  ui_main(rx).context("ui event loop returns error")
}
