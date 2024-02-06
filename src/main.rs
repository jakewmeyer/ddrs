#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![forbid(unsafe_code)]

use std::{ffi::OsString, path::Path};

use clap::Parser;
use client::Client;
use figment::{
    providers::{Format, Serialized, Toml},
    Figment,
};
use miette::{miette, IntoDiagnostic, Result};
use tokio::signal;
use tracing::{error, info};

use crate::config::Config;

const CONFIG_PATH: &str = "ddrs/config.toml";

mod client;
mod config;
mod error;
mod providers;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    config: Option<OsString>,
}

#[tokio::main]
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

    let handle = tokio::spawn(async move { client.run().await.into_diagnostic() });

    match handle.await {
        Ok(Ok(())) => {
            error!("Client exited successfully");
        }
        Ok(Err(e)) => {
            error!("Client error occurred: {}", e);
        }
        Err(e) => {
            error!("Task error occurred: {}", e);
        }
    }

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    info!("Shutting down client...");

    Ok(())
}
