use async_trait::async_trait;
use hickory_resolver::TokioAsyncResolver;
use hickory_resolver::{name_server::TokioConnectionProvider, AsyncResolver};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use hickory_resolver::config::LookupIpStrategy;
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use stun::agent::TransactionId;
use stun::client::ClientBuilder;
use stun::message::{Getter, Message, BINDING_REQUEST};
use stun::xoraddr::XorMappedAddress;
use tokio::net::UdpSocket;
use tracing::error;

use crate::config::Config;
use crate::error::Error;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpVersion {
    V4,
    V6,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpSourceInterface {
    interface: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpSourceStatic {
    address: IpAddr,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum IpSource {
    Stun,
    Http,
    Interface(IpSourceInterface),
    Static(IpSourceStatic),
}

#[derive(Debug)]
pub struct IpUpdate {
    v4: Option<Ipv4Addr>,
    v6: Option<Ipv6Addr>,
}

#[async_trait]
#[typetag::serde(tag = "type")]
pub trait Provider: Debug + Send + Sync {
    async fn update(&self, update: IpUpdate) -> Result<bool, Error>;
}

#[derive(Debug)]
pub struct Client {
    config: Config,
    resolver: AsyncResolver<TokioConnectionProvider>,
    cache: Option<IpAddr>,
    shutdown: CancellationToken,
    pub tracker: TaskTracker,
}

impl Client {
    pub fn new(config: Config) -> Client {
        let mut opts = ResolverOpts::default();
        opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
        let resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), opts);
        Client {
            config,
            resolver,
            cache: None,
            shutdown: CancellationToken::new(),
            tracker: TaskTracker::new(),
        }
    }

    async fn fetch_ip_stun(&self, version: IpVersion) -> Result<IpAddr, Error> {
        let (v4, v6) = match &self
            .resolver
            .lookup_ip(self.config.stun_server.as_str())
            .await
        {
            Ok(response) => {
                let v4 = response.iter().find_map(|ip| match ip {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                });
                let v6 = response.iter().find_map(|ip| match ip {
                    IpAddr::V6(v6) => Some(v6),
                    IpAddr::V4(_) => None,
                });
                (v4, v6)
            }
            Err(e) => {
                error!("Failed to resolve STUN server: {}", e);
                (None, None)
            }
        };
        let (handler_tx, mut handler_rx) = tokio::sync::mpsc::unbounded_channel();
        let conn = UdpSocket::bind("0:0").await?;
        let stun_ip = match version {
            IpVersion::V4 => {
                if let Some(v4) = v4 {
                    SocketAddr::new(IpAddr::V4(v4), self.config.stun_port)
                } else {
                    error!("Failed to create ipv4 socket address for STUN server");
                    return Err(Error::Unknown);
                }
            }
            IpVersion::V6 => {
                if let Some(v6) = v6 {
                    SocketAddr::new(IpAddr::V6(v6), self.config.stun_port)
                } else {
                    error!("Failed to create ipv6 socket address for STUN server");
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

    async fn fetch_ip_http(&self, version: IpVersion) -> Result<IpAddr, Error> {
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

    pub async fn run(&self) -> Result<(), Error> {
        let mut interval = time::interval(self.config.interval);
        loop {
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    // If they are different, update DNS
                    for provider in &self.config.domains {
                        let update = IpUpdate {
                            v4: None,
                            v6: None,
                        };
                        provider.update(update).await?;
                    }
                },
            }
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tracker.close();
        self.tracker.wait().await;
    }
}
