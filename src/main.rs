#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![forbid(unsafe_code)]

use std::{ffi::OsString, path::Path};

use clap::Parser;
use client::Client;
use figment::{
    providers::{Format, Toml},
    Figment,
};
use miette::{miette, IntoDiagnostic, Result};

use crate::config::Config;

const CONFIG_PATH: &str = "/ddrs/config.toml";

mod client;
mod config;
mod error;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    // Config file path
    #[arg(short, long)]
    config: Option<OsString>,
}

#[tokio::main]
async fn main() -> Result<()> {
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
    let config: Config = Figment::new()
        .merge(Toml::file(config_path))
        .extract()
        .into_diagnostic()?;

    let client = Client::new(config);
    let ip = client
        .fetch_ip_stun(client::Version::V4)
        .await
        .into_diagnostic()?;
    println!("IP: {ip}");
    Ok(())
}
