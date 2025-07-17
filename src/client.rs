use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use core::fmt;
use dyn_clone::DynClone;
use hickory_resolver::config::LookupIpStrategy;
use hickory_resolver::{Resolver, TokioResolver};
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
use url::Url;

use stun::agent::TransactionId;
use stun::message::{BINDING_REQUEST, Getter, Message};
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
    resolver: TokioResolver,
}

impl Client {
    pub fn new(config: Config) -> Arc<Client> {
        let mut resolver_builder = Resolver::builder_tokio().unwrap();
        let resolver_opts = resolver_builder.options_mut();
        resolver_opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;

        Arc::new(Client {
            config,
            cache: RwLock::new(IpUpdate { v4: None, v6: None }),
            request: HttpClient::new(),
            shutdown: CancellationToken::new(),
            resolver: resolver_builder.build(),
        })
    }

    /// Fetches the IP address via a STUN request to a public server
    async fn fetch_ip_stun(&self, version: &IpVersion) -> Result<IpAddr> {
        for url in &self.config.stun_urls {
            let url = Url::parse(url)?;
            let host = url
                .host_str()
                .ok_or(anyhow!("Unable to parse host for url: {}", url))?;
            let port = url
                .port()
                .ok_or(anyhow!("Unable to parse port for url: {}", url))?;
            let resolved = resolve_host(&self.resolver, host).await?;
            let bind_address = match version {
                IpVersion::V4 => "0:0",
                IpVersion::V6 => "[::]:0",
            };
            let sock = UdpSocket::bind(bind_address).await?;
            let stun_ip = match version {
                IpVersion::V4 => {
                    if let Some(v4) = resolved.v4 {
                        SocketAddr::new(IpAddr::V4(v4), port)
                    } else {
                        return Err(anyhow!(
                            "Failed to create ipv4 socket address for STUN server, is ipv4 enabled?"
                        ));
                    }
                }
                IpVersion::V6 => {
                    if let Some(v6) = resolved.v6 {
                        SocketAddr::new(IpAddr::V6(v6), port)
                    } else {
                        return Err(anyhow!(
                            "Failed to create ipv6 socket address for STUN server, is ipv6 enabled?"
                        ));
                    }
                }
            };
            if let Err(e) = sock.connect(stun_ip).await {
                return Err(anyhow!(e).context("Failed to connect to STUN server"));
            }
            let mut msg = Message::new();
            msg.build(&[Box::<TransactionId>::default(), Box::new(BINDING_REQUEST)])?;
            let bytes = msg.marshal_binary()?;
            sock.send(&bytes).await?;
            let mut res_buff = [0; 1024];
            if sock.recv(&mut res_buff).await.is_ok() {
                let mut res_msg = Message::new();
                res_msg.unmarshal_binary(&res_buff)?;
                let mut xor_addr = XorMappedAddress {
                    ip: match version {
                        IpVersion::V4 => IpAddr::V4(Ipv4Addr::from(0)),
                        IpVersion::V6 => IpAddr::V6(Ipv6Addr::from(0)),
                    },
                    port: 0,
                };
                xor_addr.get_from(&res_msg)?;
                return Ok(xor_addr.ip);
            }
        }
        Err(anyhow!("Failed to fetch IP address via STUN"))
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
                                IpSource::Stun => self.fetch_ip_stun(version).await.context("Failed to fetch IP via STUN"),
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

/// Host response from DNS resolution
#[derive(Debug)]
struct HostResponse {
    v4: Option<Ipv4Addr>,
    v6: Option<Ipv6Addr>,
}

/// Resolve a host to an IP address
async fn resolve_host(resolver: &TokioResolver, host: &str) -> Result<HostResponse> {
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
    Err(anyhow!(
        "Failed to find network interface: {}",
        interface.name
    ))
}
