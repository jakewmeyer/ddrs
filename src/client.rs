use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use core::fmt;
use dyn_clone::DynClone;
use local_ip_address::list_afinet_netifas;
use reqwest::{Client as InnerHttpClient, Response};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware as HttpClient};
use reqwest_retry::RetryTransientMiddleware;
use reqwest_retry::policies::ExponentialBackoff;
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::cache::Cache;
use crate::config::{Config, NonEmptyString};

static USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);
const MAX_IP_LOOKUP_BODY_BYTES: usize = 256;

/// IP version without associated address
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpVersion {
    V4,
    V6,
}

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

/// Update sent to each provider
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    fn is_empty(&self) -> bool {
        self.v4.is_none() && self.v6.is_none()
    }

    fn changed_since(&self, cached: Option<&Self>) -> Self {
        Self {
            v4: self
                .v4
                .filter(|observed| cached.and_then(|cached| cached.v4) != Some(*observed)),
            v6: self
                .v6
                .filter(|observed| cached.and_then(|cached| cached.v6) != Some(*observed)),
        }
    }

    fn merge_into_cache(self, cached: Option<Self>) -> Self {
        let mut merged = cached.unwrap_or(Self { v4: None, v6: None });

        if self.v4.is_some() {
            merged.v4 = self.v4;
        }
        if self.v6.is_some() {
            merged.v6 = self.v6;
        }

        merged
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
    fn validate_config(&self) -> Result<()>;

    async fn update(&self, update: IpUpdate, request: HttpClient) -> Result<bool>;
}

dyn_clone::clone_trait_object!(Provider);

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
            .http2_adaptive_window(true)
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

    /// Fetches the IP address via a HTTP request
    async fn fetch_ip_http(&self, version: IpVersion) -> Result<IpAddr> {
        let urls = match version {
            IpVersion::V4 => &self.config.http_ipv4,
            IpVersion::V6 => &self.config.http_ipv6,
        };
        let mut last_err: Option<anyhow::Error> = None;
        for url in urls {
            match self.request.get(url.as_str()).send().await {
                Ok(response) => {
                    match parse_ip_lookup_response(response, version, url.as_str()).await {
                        Ok(ip) => return Ok(ip),
                        Err(error) => {
                            debug!("IP lookup failed for {url}: {error}");
                            last_err = Some(error);
                        }
                    }
                }
                Err(e) => {
                    debug!("HTTP request failed for {}: {}", url, e);
                    last_err = Some(e.into());
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("failed to fetch IP address from HTTP")))
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
                                IpSource::Http => self.fetch_ip_http(version).await.context("failed to fetch IP via HTTP"),
                                IpSource::Interface(interface) => fetch_ip_interface(interface, version).context("failed to fetch IP via interface"),
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

/// Fetches the IP address of a specific network interface
fn fetch_ip_interface(interface: &IpSourceInterface, version: IpVersion) -> Result<IpAddr> {
    let interfaces = list_afinet_netifas()?;
    for iface in interfaces {
        if iface.0 == interface.name.as_str() {
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
        "failed to find network interface: {}",
        interface.name.as_str()
    ))
}

fn parse_ip_for_version(version: IpVersion, body: &str) -> Result<IpAddr> {
    let body = body.trim();
    match version {
        IpVersion::V4 => body
            .parse::<Ipv4Addr>()
            .map(IpAddr::V4)
            .context("expected IPv4 address"),
        IpVersion::V6 => body
            .parse::<Ipv6Addr>()
            .map(IpAddr::V6)
            .context("expected IPv6 address"),
    }
}

async fn parse_ip_lookup_response(
    response: Response,
    version: IpVersion,
    url: &str,
) -> Result<IpAddr> {
    let status = response.status();
    let body = read_ip_lookup_body(response).await;

    if !status.is_success() {
        let detail = match body {
            Ok(body) => response_body_detail(&body),
            Err(error) => format!(": failed to read response body: {error}"),
        };
        return Err(anyhow!("IP lookup {url} returned HTTP {status}{detail}"));
    }

    let body = body?;
    parse_ip_for_version(version, &body)
        .with_context(|| format!("failed to parse {version:?} IP lookup response from {url}"))
}

async fn read_ip_lookup_body(mut response: Response) -> Result<String> {
    if let Some(length) = response.content_length() {
        let max_length = u64::try_from(MAX_IP_LOOKUP_BODY_BYTES)?;
        if length > max_length {
            return Err(anyhow!(
                "IP lookup response body exceeded {MAX_IP_LOOKUP_BODY_BYTES} bytes"
            ));
        }
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_IP_LOOKUP_BODY_BYTES.saturating_sub(body.len()) {
            return Err(anyhow!(
                "IP lookup response body exceeded {MAX_IP_LOOKUP_BODY_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }

    String::from_utf8(body).context("IP lookup response body was not valid UTF-8")
}

fn response_body_detail(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        String::new()
    } else {
        format!(": {body}")
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    use super::{Client, IpUpdate, IpVersion, MAX_IP_LOOKUP_BODY_BYTES};
    use crate::config::Config;

    const OLD_V4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
    const NEW_V4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
    const OLD_V6: Ipv6Addr = Ipv6Addr::LOCALHOST;
    const NEW_V6: Ipv6Addr = Ipv6Addr::UNSPECIFIED;

    fn config_with_http_ipv4(urls: &[String]) -> Config {
        let urls = urls
            .iter()
            .map(|url| format!("\"{url}\""))
            .collect::<Vec<_>>()
            .join(", ");

        toml::from_str(&format!(
            r#"
retries = 0
http_ipv4 = [{urls}]

[[providers]]
type = "cloudflare"
zone = "example.com"
api_token = "token"

[[providers.domains]]
name = "example.com"
"#
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn fetch_ip_http_rejects_non_success_status() {
        let mock = MockServer::start().await;
        let url = mock.uri();
        let client = Client::new(config_with_http_ipv4(std::slice::from_ref(&url))).unwrap();

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string("192.0.2.1"))
            .expect(1)
            .mount(&mock)
            .await;

        let error = client.fetch_ip_http(IpVersion::V4).await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("IP lookup"),
            "error should identify the IP lookup: {message}"
        );
        assert!(
            message.contains(&url),
            "error should include URL: {message}"
        );
        assert!(
            message.contains("HTTP 500 Internal Server Error"),
            "error should include status: {message}"
        );
        assert!(
            message.contains("192.0.2.1"),
            "error should include bounded response detail: {message}"
        );
    }

    #[tokio::test]
    async fn fetch_ip_http_uses_next_url_after_non_success_status() {
        let bad = MockServer::start().await;
        let good = MockServer::start().await;
        let urls = [bad.uri(), good.uri()];
        let client = Client::new(config_with_http_ipv4(&urls)).unwrap();

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string("192.0.2.1"))
            .expect(1)
            .mount(&bad)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("192.0.2.2\n"))
            .expect(1)
            .mount(&good)
            .await;

        let ip = client.fetch_ip_http(IpVersion::V4).await.unwrap();

        assert_eq!(ip, Ipv4Addr::new(192, 0, 2, 2));
    }

    #[tokio::test]
    async fn fetch_ip_http_rejects_oversized_body() {
        let mock = MockServer::start().await;
        let url = mock.uri();
        let client = Client::new(config_with_http_ipv4(std::slice::from_ref(&url))).unwrap();

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("x".repeat(MAX_IP_LOOKUP_BODY_BYTES + 1)),
            )
            .expect(1)
            .mount(&mock)
            .await;

        let error = client.fetch_ip_http(IpVersion::V4).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("IP lookup response body exceeded 256 bytes")
        );
    }

    #[test]
    fn ip_update_changed_since_cache_only_keeps_observed_changes() {
        let cached = IpUpdate {
            v4: Some(OLD_V4),
            v6: Some(OLD_V6),
        };
        let observed = IpUpdate {
            v4: Some(NEW_V4),
            v6: None,
        };

        assert_eq!(
            observed.changed_since(Some(&cached)),
            IpUpdate {
                v4: Some(NEW_V4),
                v6: None,
            }
        );
    }

    #[test]
    fn ip_update_changed_since_cache_skips_unchanged_values() {
        let cached = IpUpdate {
            v4: Some(OLD_V4),
            v6: Some(OLD_V6),
        };
        let observed = IpUpdate {
            v4: Some(OLD_V4),
            v6: Some(NEW_V6),
        };

        assert_eq!(
            observed.changed_since(Some(&cached)),
            IpUpdate {
                v4: None,
                v6: Some(NEW_V6),
            }
        );
    }

    #[test]
    fn ip_update_merge_into_cache_preserves_unobserved_cached_values() {
        let cached = IpUpdate {
            v4: Some(OLD_V4),
            v6: Some(OLD_V6),
        };
        let observed = IpUpdate {
            v4: Some(NEW_V4),
            v6: None,
        };

        assert_eq!(
            observed.merge_into_cache(Some(cached)),
            IpUpdate {
                v4: Some(NEW_V4),
                v6: Some(OLD_V6),
            }
        );
    }

    #[test]
    fn ip_update_merge_into_cache_uses_observed_values_without_cache() {
        let observed = IpUpdate {
            v4: None,
            v6: Some(NEW_V6),
        };

        assert_eq!(
            observed.merge_into_cache(None),
            IpUpdate {
                v4: None,
                v6: Some(NEW_V6),
            }
        );
    }
}
