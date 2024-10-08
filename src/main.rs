#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![forbid(unsafe_code)]

use std::{ffi::OsString, path::Path};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use client::Client;
use figment::{
    providers::{Format, Serialized, Toml},
    Figment,
};
use tokio::signal;
use tracing::info;

use crate::config::Config;

const CONFIG_PATH: &str = "/etc/ddrs/config.toml";

mod client;
mod config;
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
            .context("Failed to canonicalize config arg path")?,
        None => Path::new(CONFIG_PATH).to_path_buf(),
    };

    let config: Config = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(config_path))
        .extract()
        .context("Figment failed to parse")?;

    if config.providers.is_empty() {
        return Err(anyhow!("No providers configured"));
    }

    let client = Client::new(config);

    // Handle SIGINT
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    // Handle SIGTERM
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    let graceful = client.clone();
    let client_handle = client.run();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    info!("Starting graceful shutdown...");

    graceful.shutdown();
    client_handle.await??;

    info!("Graceful shutdown complete");

    Ok(())
}
