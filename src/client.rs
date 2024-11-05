use anyhow::{anyhow, Result};
use async_trait::async_trait;
use core::fmt;
use dyn_clone::DynClone;
use hickory_resolver::config::{LookupIpStrategy, ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;
use local_ip_address::list_afinet_netifas;
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use stun::agent::TransactionId;
use stun::client::ClientBuilder;
use stun::message::{Getter, Message, BINDING_REQUEST};
use stun::xoraddr::XorMappedAddress;
use tokio::net::UdpSocket;

use crate::config::Config;

/// IP version without associated address
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpVersion {
    V4,
    V6,
}

/// IP interface source serde representation
#[derive(Debug, Serialize, Deserialize)]
pub struct IpSourceInterface {
    name: String,
}

/// IP source for fetching the address
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum IpSource {
    Stun,
    Http,
    Interface(IpSourceInterface),
}

/// Update sent to each provider
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpUpdate {
    pub v4: Option<IpAddr>,
    pub v6: Option<IpAddr>,
}

impl IpUpdate {
    pub fn as_array(&self) -> [(IpVersion, Option<IpAddr>); 2] {
        [(IpVersion::V4, self.v4), (IpVersion::V6, self.v6)]
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
#[typetag::serde(tag = "type")]
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
    resolver: TokioAsyncResolver,
}

impl Client {
    pub fn new(config: Config) -> Arc<Client> {
        let mut resolver_opts = ResolverOpts::default();
        resolver_opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
        Arc::new(Client {
            config,
            cache: RwLock::new(IpUpdate { v4: None, v6: None }),
            request: HttpClient::new(),
            shutdown: CancellationToken::new(),
            resolver: TokioAsyncResolver::tokio(ResolverConfig::default(), resolver_opts),
        })
    }

    /// Fetches the IP address via a STUN request to a public server
    async fn fetch_ip_stun(&self, version: &IpVersion) -> Result<IpAddr> {
        let resolved = resolve_host(&self.resolver, &self.config.stun_url).await?;
        let (handler_tx, mut handler_rx) = tokio::sync::mpsc::unbounded_channel();
        let bind_address = match version {
            IpVersion::V4 => "0:0",
            IpVersion::V6 => "[::]:0",
        };
        let conn = UdpSocket::bind(bind_address).await?;
        let stun_ip = match version {
            IpVersion::V4 => {
                if let Some(v4) = resolved.v4 {
                    SocketAddr::new(IpAddr::V4(v4), self.config.stun_port)
                } else {
                    error!(
                        "Failed to create ipv4 socket address for STUN server, is ipv4 enabled?"
                    );
                    return Err(anyhow!(
                        "Failed to create ipv4 socket address for STUN server, is ipv4 enabled?"
                    ));
                }
            }
            IpVersion::V6 => {
                if let Some(v6) = resolved.v6 {
                    SocketAddr::new(IpAddr::V6(v6), self.config.stun_port)
                } else {
                    error!(
                        "Failed to create ipv6 socket address for STUN server, is ipv6 enabled?"
                    );
                    return Err(anyhow!(
                        "Failed to create ipv6 socket address for STUN server, is ipv6 enabled?"
                    ));
                }
            }
        };
        if let Err(e) = conn.connect(stun_ip).await {
            return Err(anyhow!(e).context("Failed to connect to STUN server"));
        }
        let mut client = ClientBuilder::new().with_conn(Arc::new(conn)).build()?;
        let mut msg = Message::new();
        msg.build(&[Box::<TransactionId>::default(), Box::new(BINDING_REQUEST)])?;
        let handler = Arc::new(handler_tx);
        client.send(&msg, Some(handler.clone())).await?;
        if let Some(event) = handler_rx.recv().await {
            let msg = event.event_body?;
            let mut xor_addr = XorMappedAddress {
                ip: match version {
                    IpVersion::V4 => IpAddr::V4(Ipv4Addr::from(0)),
                    IpVersion::V6 => IpAddr::V6(Ipv6Addr::from(0)),
                },
                port: 0,
            };
            xor_addr.get_from(&msg)?;
            client.close().await?;
            Ok(xor_addr.ip)
        } else {
            client.close().await?;
            error!("Failed to receive STUN response");
            Err(anyhow!("Failed to receive STUN response"))
        }
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
        error!("Failed to fetch IP address from HTTP");
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
                            if let Some(ip) = match &self.config.source {
                                IpSource::Stun => self.fetch_ip_stun(version).await.ok(),
                                IpSource::Http => self.fetch_ip_http(version).await.ok(),
                                IpSource::Interface(interface) => fetch_ip_interface(interface, version).ok(),
                            } {
                                match version {
                                    IpVersion::V4 => update.v4 = Some(ip),
                                    IpVersion::V6 => update.v6 = Some(ip),
                                }
                            }
                        }
                        debug!("Found IP(s): {update}");
                        if update == *self.cache.read().await {
                            debug!("No IP address change detected, skipping update...");
                            continue;
                        }
                        if self.config.dry_run {
                            info!("Dry run mode enabled, skipping update...");
                            continue;
                        }
                        info!("IP address update detected, updating providers...");

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
                                        error!("Failed to update provider: {error}");
                                        failed = true;
                                    }
                                },
                                Err(error) => {
                                    error!("Provider task failed to complete: {error}");
                                }
                            }
                        }
                        if !failed {
                            info!("Providers updated successfully wih IP(s): {update}");
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

/// Host response from DNS resolution
#[derive(Debug)]
struct HostResponse {
    v4: Option<Ipv4Addr>,
    v6: Option<Ipv6Addr>,
}

/// Resolve a host to an IP address
async fn resolve_host(resolver: &TokioAsyncResolver, host: &str) -> Result<HostResponse> {
    let mut ipv4 = None;
    let mut ipv6 = None;
    let response = resolver.lookup_ip(host).await?;
    for addr in response.iter() {
        match addr {
            IpAddr::V4(v4) if ipv4.is_none() => {
                ipv4 = Some(v4);
            }
            IpAddr::V6(v6) if ipv6.is_none() => {
                ipv6 = Some(v6);
            }
            _ => {}
        }
        if ipv4.is_some() && ipv6.is_some() {
            break;
        }
    }
    Ok(HostResponse { v4: ipv4, v6: ipv6 })
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
    error!("Failed to find network interface: {}", interface.name);
    Err(anyhow!(
        "Failed to find network interface: {}",
        interface.name
    ))
}
