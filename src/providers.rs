use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::client::{IpUpdate, Provider};
use crate::error::Error;

#[derive(Debug, Serialize, Deserialize)]
struct Cloudflare {
    host: String,
    api_key: String,
    proxied: bool,
    ttl: u32,
}

#[async_trait]
#[typetag::serde(name = "cloudflare")]
impl Provider for Cloudflare {
    async fn update(&self, update: IpUpdate) -> Result<bool, Error> {
        todo!()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DynDns {
    host: String,
    username: String,
    password: String,
}

#[async_trait]
#[typetag::serde(name = "dyndns")]
impl Provider for DynDns {
    async fn update(&self, update: IpUpdate) -> Result<bool, Error> {
        todo!()
    }
}
