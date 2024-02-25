use async_trait::async_trait;
use local_ip_address::list_afinet_netifas;
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use stun::agent::TransactionId;
use stun::client::ClientBuilder;
use stun::message::{Getter, Message, BINDING_REQUEST};
use stun::xoraddr::XorMappedAddress;
use tokio::net::{lookup_host, UdpSocket};
use tracing::error;

use crate::config::Config;
use crate::error::Error;

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
#[derive(Debug, PartialEq, Eq)]
pub struct IpUpdate {
    v4: Option<IpAddr>,
    v6: Option<IpAddr>,
}

/// Provider trait for updating DNS records or DDNS services
#[async_trait]
#[typetag::serde(tag = "type")]
pub trait Provider: Debug + Send + Sync {
    async fn update(&self, update: &IpUpdate, request: &HttpClient) -> Result<bool, Error>;
}

/// DDRS client
#[derive(Debug)]
pub struct Client {
    config: Config,
    cache: RwLock<IpUpdate>,
    request: HttpClient,
    shutdown: CancellationToken,
    pub tracker: TaskTracker,
}

impl Client {
    pub fn new(config: Config) -> Client {
        Client {
            config,
            cache: RwLock::new(IpUpdate { v4: None, v6: None }),
            request: HttpClient::new(),
            shutdown: CancellationToken::new(),
            tracker: TaskTracker::new(),
        }
    }

    /// Fetches the IP address via a STUN request to a public server
    async fn fetch_ip_stun(&self, version: &IpVersion) -> Result<IpAddr, Error> {
        let resolved = resolve_host(&self.config.stun_addr).await?;
        let (handler_tx, mut handler_rx) = tokio::sync::mpsc::unbounded_channel();
        let conn = UdpSocket::bind("0:0").await?;
        let stun_ip = match version {
            IpVersion::V4 => {
                if let Some(v4) = resolved.v4 {
                    SocketAddr::new(IpAddr::V4(v4), resolved.port)
                } else {
                    error!(
                        "Failed to create ipv4 socket address for STUN server, is ipv4 enabled?"
                    );
                    return Err(Error::Unknown);
                }
            }
            IpVersion::V6 => {
                if let Some(v6) = resolved.v6 {
                    SocketAddr::new(IpAddr::V6(v6), resolved.port)
                } else {
                    error!(
                        "Failed to create ipv6 socket address for STUN server, is ipv6 enabled?"
                    );
                    return Err(Error::Unknown);
                }
            }
        };
        if let Err(e) = conn.connect(stun_ip).await {
            error!("Failed to connect to STUN server: {}", e);
            return Err(Error::Io(e));
        }
        let mut client = ClientBuilder::new().with_conn(Arc::new(conn)).build()?;
        let mut msg = Message::new();
        msg.build(&[Box::<TransactionId>::default(), Box::new(BINDING_REQUEST)])?;
        let handler = Arc::new(handler_tx);
        client.send(&msg, Some(handler.clone())).await?;
        if let Some(event) = handler_rx.recv().await {
            let msg = event.event_body?;
            let mut xor_addr = XorMappedAddress::default();
            xor_addr.get_from(&msg)?;
            client.close().await?;
            Ok(xor_addr.ip)
        } else {
            client.close().await?;
            Err(Error::Unknown)
        }
    }

    /// Fetches the IP address via a HTTP request
    async fn fetch_ip_http(&self, version: &IpVersion) -> Result<IpAddr, Error> {
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
        Err(Error::Unknown)
    }

    /// Starts the client
    pub async fn run(&self) -> Result<(), Error> {
        let mut interval = time::interval(self.config.interval);
        loop {
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
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
                    if update == *self.cache.read().await {
                        continue;
                    }
                    for provider in &self.config.providers {
                        provider.update(&update, &self.request).await?;
                    }
                    let mut cache = self.cache.write().await;
                    *cache = update;
                },
            }
        }
        Ok(())
    }

    /// Trigger a graceful shutdown of the client
    /// This will wait for all in progress updates to finish
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tracker.close();
        self.tracker.wait().await;
    }
}

/// Host response from DNS resolution
#[derive(Debug)]
struct HostResponse {
    v4: Option<Ipv4Addr>,
    v6: Option<Ipv6Addr>,
    port: u16,
}

/// Resolve a host to an IP address using `ToSocketAddrs`
async fn resolve_host(host: &str) -> Result<HostResponse, Error> {
    let mut ipv4 = None;
    let mut ipv6 = None;
    let mut port = 0;
    let addresses = lookup_host(host).await?;
    for addr in addresses {
        port = addr.port();
        match addr.ip() {
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
    Ok(HostResponse {
        v4: ipv4,
        v6: ipv6,
        port,
    })
}

/// Fetches the IP address of a specific network interface
fn fetch_ip_interface(interface: &IpSourceInterface, version: &IpVersion) -> Result<IpAddr, Error> {
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
    Err(Error::Unknown)
}
