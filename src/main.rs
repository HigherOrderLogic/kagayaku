mod backend;
mod common;
mod ui;

use std::thread::{Builder as ThreadBuilder, available_parallelism};

use anyhow::{Context, Error as AnyError};
use async_channel::unbounded;
use async_global_executor::{GlobalExecutorConfig, block_on, init_with_config};

use crate::{backend::backend_main, ui::ui_main};

fn main() -> Result<(), AnyError> {
    init_with_config(
        GlobalExecutorConfig::default()
            .with_max_threads(available_parallelism().map_or(1, |n| n.get())),
    );

    let (tx, rx) = unbounded();

    ThreadBuilder::new()
        .name("ui".into())
        .spawn(move || ui_main(rx))
        .context("failed to spawn ui thread")?;

    block_on(backend_main(tx)).context("main function returns error")
}
