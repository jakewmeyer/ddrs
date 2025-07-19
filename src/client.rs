use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use core::fmt;
use dyn_clone::DynClone;
use local_ip_address::list_afinet_netifas;
use reqwest::Client as HttpClient;
use serde::Deserialize;
use std::fmt::{Debug, Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::config::Config;

static USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

/// IP version without associated address
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpVersion {
    V4,
    V6,
}

/// IP interface source serde representation
#[derive(Debug, Deserialize)]
pub struct IpSourceInterface {
    name: String,
}

/// IP source for fetching the address
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum IpSource {
    Http,
    Interface(IpSourceInterface),
}

/// Update sent to each provider
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpUpdate {
    pub v4: Option<Ipv4Addr>,
    pub v6: Option<Ipv6Addr>,
}

impl IpUpdate {
    pub fn iter(&self) -> impl Iterator<Item = (IpVersion, IpAddr)> + '_ {
        let v4 = self.v4.map(|addr| (IpVersion::V4, IpAddr::V4(addr)));
        let v6 = self.v6.map(|addr| (IpVersion::V6, IpAddr::V6(addr)));
        [v4, v6].into_iter().flatten()
    }
}

impl Display for IpUpdate {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(
            f,
            "v4: {}, v6: {}",
            self.v4.map_or("None".to_string(), |ip| ip.to_string()),
            self.v6.map_or("None".to_string(), |ip| ip.to_string())
        )
    }
}

/// Provider trait for updating DNS records or DDNS services
#[async_trait]
#[typetag::deserialize(tag = "type")]
pub trait Provider: Debug + DynClone + Send + Sync {
    async fn update(&self, update: IpUpdate, request: HttpClient) -> Result<bool>;
}

dyn_clone::clone_trait_object!(Provider);

/// DDRS client
#[derive(Debug)]
pub struct Client {
    config: Config,
    cache: RwLock<IpUpdate>,
    request: HttpClient,
    shutdown: CancellationToken,
}

impl Client {
    pub fn new(config: Config) -> Arc<Client> {
        Arc::new(Client {
            config,
            cache: RwLock::new(IpUpdate { v4: None, v6: None }),
            request: HttpClient::builder()
                .timeout(Duration::from_secs(30))
                .user_agent(USER_AGENT)
                .build()
                .expect("Failed to build HTTP client"),
            shutdown: CancellationToken::new(),
        })
    }

    /// Fetches the IP address via a HTTP request
    async fn fetch_ip_http(&self, version: &IpVersion) -> Result<IpAddr> {
        let urls = match version {
            IpVersion::V4 => &self.config.http_ipv4,
            IpVersion::V6 => &self.config.http_ipv6,
        };
        for url in urls {
            let response = reqwest::get(url.as_str()).await?;
            if let Ok(ip) = response.text().await {
                if let Ok(ip) = ip.trim().parse() {
                    return Ok(ip);
                }
            }
        }
        Err(anyhow!("Failed to fetch IP address from HTTP"))
    }

    /// Starts the client
    pub fn run(self: Arc<Self>) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let mut interval = time::interval(self.config.interval);
            info!(
                "Started DDRS client, checking IP address every {:?}",
                self.config.interval
            );
            loop {
                tokio::select! {
                    biased;
                    () = self.shutdown.cancelled() => {
                        break;
                    }
                    _ = interval.tick() => {
                        debug!("Checking IP address...");
                        let mut update = IpUpdate {
                            v4: None,
                            v6: None,
                        };
                        for version in &self.config.versions {
                            let ip_result = match &self.config.source {
                                IpSource::Http => self.fetch_ip_http(version).await.context("Failed to fetch IP via HTTP"),
                                IpSource::Interface(interface) => fetch_ip_interface(interface, version).context("Failed to fetch IP via interface"),
                            };
                            match ip_result {
                                Ok(ip) => match version {
                                    IpVersion::V4 => {
                                        if let IpAddr::V4(ip) = ip {
                                            update.v4 = Some(ip);
                                        }
                                    },
                                    IpVersion::V6 => {
                                         if let IpAddr::V6(ip) = ip {
                                            update.v6 = Some(ip);
                                        }
                                    }
                                },
                                Err(error) => {
                                    error!("Error fetching IP: {}", error);
                                }
                            }
                        }
                        if update.v4.is_none() && update.v6.is_none() {
                            error!("Failed to fetch IP address, skipping update...");
                            continue;
                        }
                        debug!("Found IP(s): {update}");
                        if update == *self.cache.read().await {
                            debug!("No IP address change detected, skipping update...");
                            continue;
                        }
                        info!("IP address update detected, updating with IP(s): {update}");

                        if self.config.dry_run {
                            info!("Dry run mode enabled, skipping update...");
                            continue;
                        }

                        let mut set = JoinSet::new();
                        for provider in &self.config.providers {
                            let provider = provider.clone();
                            let update = update.clone();
                            let request = self.request.clone();
                            set.spawn(async move {
                                provider.update(update, request).await
                            });
                        }
                        let mut failed = false;
                        while let Some(result) = set.join_next().await {
                            match result {
                                Ok(result) => {
                                    if let Err(error) = result {
                                        error!("Error updating provider: {error}");
                                        failed = true;
                                    }
                                },
                                Err(error) => {
                                    error!("Provider task failed to complete: {error}");
                                }
                            }
                        }
                        if !failed {
                            info!("All providers updated successfully");
                            let mut cache = self.cache.write().await;
                            *cache = update;
                        }
                    }
                }
            }
            Ok(())
        })
    }

    /// Trigger a graceful shutdown of the client
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

/// Fetches the IP address of a specific network interface
fn fetch_ip_interface(interface: &IpSourceInterface, version: &IpVersion) -> Result<IpAddr> {
    let interfaces = list_afinet_netifas()?;
    for iface in interfaces {
        if iface.0 == interface.name {
            match version {
                IpVersion::V4 => {
                    if iface.1.is_ipv4() {
                        return Ok(iface.1);
                    }
                }
                IpVersion::V6 => {
                    if iface.1.is_ipv6() {
                        return Ok(iface.1);
                    }
                }
            }
        }
    }
    Err(anyhow!(
        "Failed to find network interface: {}",
        interface.name
    ))
}
