use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::client::{IpUpdate, Provider};
use crate::error::Error;

/// Cloudflare DNS update provider
#[derive(Debug, Serialize, Deserialize)]
struct Cloudflare {
    host: String,
    api_token: String,
    proxied: bool,
    ttl: u32,
}

#[async_trait]
#[typetag::serde(name = "cloudflare")]
impl Provider for Cloudflare {
    async fn update(&self, update: &IpUpdate) -> Result<bool, Error> {
        info!("Updating cloudflare");
        Ok(true)
    }
}

/// Dyndns update provider
#[derive(Debug, Serialize, Deserialize)]
struct DynDns {
    host: String,
    username: String,
    password: String,
}

#[async_trait]
#[typetag::serde(name = "dyndns")]
impl Provider for DynDns {
    async fn update(&self, update: &IpUpdate) -> Result<bool, Error> {
        info!("Updating dyndns");
        Ok(true)
    }
}
