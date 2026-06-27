#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![forbid(unsafe_code)]

use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result};
use clap::Parser;
use client::Client;
use tokio::signal;
use tracing::info;

use crate::config::Config;

const CONFIG_PATH: &str = "/etc/ddrs/config.toml";

mod cache;
mod client;
mod config;
mod ip;
mod providers;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    // Config file path
    #[arg(short, long)]
    config: Option<OsString>,
}

#[tokio::main(worker_threads = 1)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let config_path = match args.config {
        Some(path) => Path::new(&path)
            .canonicalize()
            .context("failed to canonicalize config arg path")?,
        None => Path::new(CONFIG_PATH).to_path_buf(),
    };

    let config = toml::from_str::<Config>(&std::fs::read_to_string(config_path)?)?;

    for provider in &config.providers {
        provider.validate_config()?;
    }

    let client = Client::new(config)?;

    // Handle SIGINT
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .context("failed to listen for Ctrl+C")
    };

    // Unix SIGTERM
    #[cfg(unix)]
    let terminate = async {
        let mut signal = signal::unix::signal(signal::unix::SignalKind::terminate())
            .context("failed to install SIGTERM handler")?;
        signal.recv().await;
        Ok::<(), anyhow::Error>(())
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<Result<()>>();

    let graceful = client.clone();
    let mut client_handle = client.run();

    tokio::select! {
        result = ctrl_c => result?,
        result = terminate => result?,
        result = &mut client_handle => {
            result??;
            anyhow::bail!("client task exited unexpectedly");
        }
    }

    info!("Starting graceful shutdown...");

    graceful.shutdown();
    client_handle.await??;

    info!("Graceful shutdown complete");

    Ok(())
}
