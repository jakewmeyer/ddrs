use std::net::IpAddr;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::Error;

#[async_trait]
#[typetag::serde(tag = "type")]
pub trait Provider: std::fmt::Debug + Send + Sync {
    async fn update(&self, ip: IpAddr) -> Result<(), Error>;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Cloudflare {
    pub api_key: String,
    pub proxied: bool,
    pub host: String,
}

#[async_trait]
#[typetag::serde(name = "cloudflare")]
impl Provider for Cloudflare {
    async fn update(&self, ip: IpAddr) -> Result<(), Error> {
        // Update Cloudflare DNS record
        info!("Updating Cloudflare DNS record");
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DynDns {
    pub host: String,
}

#[async_trait]
#[typetag::serde(name = "dyndns")]
impl Provider for DynDns {
    async fn update(&self, ip: IpAddr) -> Result<(), Error> {
        // Update Dyndns DNS record
        info!("Updating Dyndns DNS record");
        Ok(())
    }
}
