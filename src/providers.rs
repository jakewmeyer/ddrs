use std::fmt::Debug;

use anyhow::Result;
use async_trait::async_trait;
use dyn_clone::DynClone;
use reqwest_middleware::ClientWithMiddleware as HttpClient;

use crate::client::IpUpdate;

mod cloudflare;
mod duckdns;
mod porkbun;

/// DNS update provider.
#[async_trait]
#[typetag::deserialize(tag = "type")]
pub trait Provider: Debug + DynClone + Send + Sync {
    fn validate_config(&self) -> Result<()>;

    async fn update(&self, update: IpUpdate, request: HttpClient) -> Result<bool>;
}

dyn_clone::clone_trait_object!(Provider);
