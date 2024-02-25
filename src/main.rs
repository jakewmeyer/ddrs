#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![forbid(unsafe_code)]

use std::{ffi::OsString, path::Path, sync::Arc};

use clap::Parser;
use client::Client;
use figment::{
    providers::{Format, Serialized, Toml},
    Figment,
};
use miette::{miette, IntoDiagnostic, Result};
use tokio::signal;
use tracing::info;

use crate::config::Config;

const CONFIG_PATH: &str = "ddrs/config.toml";

mod client;
mod config;
mod error;
mod providers;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    // Config file path
    #[arg(short, long)]
    config: Option<OsString>,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let config_path = match args.config {
        Some(path) => Path::new(&path)
            .canonicalize()
            .into_diagnostic()?
            .to_str()
            .ok_or_else(|| miette!("Invalid config path"))?
            .to_string(),
        None => dirs::config_dir()
            .ok_or_else(|| miette!("No config directory found"))?
            .join(CONFIG_PATH)
            .to_str()
            .ok_or_else(|| miette!("Invalid config path"))?
            .to_string(),
    };

    let config: Config = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(config_path))
        .extract()
        .into_diagnostic()?;

    let client = Arc::new(Client::new(config));

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

    let spawn_client = client.clone();
    let run_client = client.clone();
    spawn_client
        .tracker
        .spawn(async move { run_client.run().await.into_diagnostic() });

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    client.shutdown().await;

    info!("Shutting down client...");

    Ok(())
}
