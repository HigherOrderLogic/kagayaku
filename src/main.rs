mod backend;
mod common;

use std::thread::available_parallelism;

use anyhow::{Context, Error as AnyError};
use async_channel::unbounded;
use async_global_executor::{GlobalExecutorConfig, block_on, init_with_config};

use crate::backend::backend_main;

fn main() -> Result<(), AnyError> {
    init_with_config(
        GlobalExecutorConfig::default()
            .with_max_threads(available_parallelism().map_or(1, |n| n.get())),
    );

    let (tx, rx) = unbounded();
    block_on(backend_main(tx)).context("main function returns error")
}
