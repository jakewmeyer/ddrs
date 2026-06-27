use anyhow::{Context, Result};
use reqwest::Client as InnerHttpClient;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware as HttpClient};
use reqwest_retry::RetryTransientMiddleware;
use reqwest_retry::policies::ExponentialBackoff;
use serde::Deserialize;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::cache::Cache;
use crate::config::{Config, NonEmptyString};
use crate::ip::{IpUpdate, IpVersion};
use crate::ip_lookup;

static USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

/// IP interface source serde representation
#[derive(Debug, Deserialize)]
pub struct IpSourceInterface {
    name: NonEmptyString,
}

/// IP source for fetching the address
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum IpSource {
    Http,
    Interface(IpSourceInterface),
}

/// DDRS client
#[derive(Debug)]
pub struct Client {
    config: Config,
    cache: Cache,
    request: HttpClient,
    shutdown: CancellationToken,
}

impl Client {
    pub fn new(config: Config) -> Result<Arc<Client>> {
        let client = InnerHttpClient::builder()
            .timeout(config.timeout.get())
            .connect_timeout(config.connect_timeout.get())
            .user_agent(USER_AGENT)
            .pool_max_idle_per_host(1)
            .pool_idle_timeout(Duration::from_mins(1))
            .build()
            .context("failed to build HTTP client")?;
        let retry_policy =
            ExponentialBackoff::builder().build_with_max_retries(config.retries.get());
        let request = ClientBuilder::new(client)
            .with(RetryTransientMiddleware::new_with_policy(retry_policy))
            .build();
        Ok(Arc::new(Client {
            cache: Cache::new(config.cache_path.clone()),
            request,
            shutdown: CancellationToken::new(),
            config,
        }))
    }

    /// Starts the client
    pub fn run(self: Arc<Self>) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let mut interval = time::interval(self.config.interval.get());
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            info!(
                "Started DDRS client, checking IP address every {:?}",
                self.config.interval.get()
            );
            time::sleep(Duration::from_secs(2)).await;
            loop {
                tokio::select! {
                    biased;
                    () = self.shutdown.cancelled() => {
                        break;
                    }
                    _ = interval.tick() => {
                        debug!("Checking IP address...");
                        let mut observed = IpUpdate {
                            v4: None,
                            v6: None,
                        };
                        for version in self.config.versions.iter() {
                            let ip_result = match &self.config.source {
                                IpSource::Http => {
                                    let urls = match version {
                                        IpVersion::V4 => &self.config.http_ipv4,
                                        IpVersion::V6 => &self.config.http_ipv6,
                                    };
                                    ip_lookup::fetch_ip_http(
                                        &self.request,
                                        urls,
                                        self.config.http_lookup_quorum.get(),
                                        version,
                                    )
                                    .await
                                    .context("failed to fetch IP via HTTP")
                                },
                                IpSource::Interface(interface) => ip_lookup::fetch_ip_interface(
                                    interface.name.as_str(),
                                    version,
                                )
                                .context("failed to fetch IP via interface"),
                            };
                            match ip_result {
                                Ok(ip) => match version {
                                    IpVersion::V4 => {
                                        if let IpAddr::V4(ip) = ip {
                                            observed.v4 = Some(ip);
                                        }
                                    },
                                    IpVersion::V6 => {
                                         if let IpAddr::V6(ip) = ip {
                                            observed.v6 = Some(ip);
                                        }
                                    }
                                },
                                Err(error) => {
                                    error!("Error fetching IP: {}", error);
                                }
                            }
                        }
                        if observed.is_empty() {
                            error!("Failed to fetch IP address, skipping update...");
                            continue;
                        }

                        debug!("Found IP(s): {observed}");
                        let cached = match self.cache.get().await {
                            Ok(cached) => cached,
                            Err(e) => {
                                warn!("Failed to read cache: {}, updating with IP(s): {observed}", e);
                                None
                            }
                        };
                        let update = observed.changed_since(cached.as_ref());
                        if update.is_empty() {
                            debug!("No IP address cache change detected, skipping update...");
                            continue;
                        }
                        let had_cached = cached.is_some();
                        let next_cache = observed.merge_into_cache(cached);
                        if had_cached {
                            debug!("Cached IP change detected, updating with IP(s): {update}");
                        } else {
                            debug!("No cached IP found, updating with IP(s): {update}");
                        }

                        if self.config.dry_run {
                            info!("Dry run mode enabled, skipping update...");
                            continue;
                        }

                        if self.update_providers(update).await {
                            info!("All providers updated successfully");
                            if let Err(e) = self.cache.set(next_cache).await {
                                warn!("Failed to update cache: {}", e);
                            }
                        }
                    }
                }
            }
            Ok(())
        })
    }

    async fn update_providers(&self, update: IpUpdate) -> bool {
        let mut set = JoinSet::new();
        for provider in &self.config.providers {
            let provider = provider.clone();
            let update = update.clone();
            let request = self.request.clone();
            set.spawn(async move { provider.update(update, request).await });
        }

        let mut failed = false;
        while let Some(result) = set.join_next().await {
            match result {
                Ok(result) => {
                    if let Err(error) = result {
                        error!("Error updating provider: {error}");
                        failed = true;
                    }
                }
                Err(error) => {
                    error!("Provider task failed to complete: {error}");
                    failed = true;
                }
            }
        }
        !failed
    }

    /// Trigger a graceful shutdown of the client
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}
