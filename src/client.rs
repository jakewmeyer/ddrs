use hickory_resolver::TokioAsyncResolver;
use hickory_resolver::{name_server::TokioConnectionProvider, AsyncResolver};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

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

const STUN_SERVER: &str = "stun.l.google.com";
const STUN_PORT: u16 = 19302;

pub enum Version {
    V4,
    V6,
}

#[derive(Debug)]
pub struct Client {
    config: Config,
    resolver: AsyncResolver<TokioConnectionProvider>,
}

impl Client {
    pub fn new(config: Config) -> Self {
        let mut opts = ResolverOpts::default();
        opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
        let resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), opts);
        Self { config, resolver }
    }

    pub async fn fetch_ip_stun(&self, version: Version) -> Result<IpAddr, Error> {
        let (v4, v6) = match &self.resolver.lookup_ip(STUN_SERVER).await {
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
            Version::V4 => {
                if let Some(v4) = v4 {
                    SocketAddr::new(IpAddr::V4(v4), STUN_PORT)
                } else {
                    error!("Failed to create ipv4 socket address for STUN server");
                    return Err(Error::Unknown);
                }
            }
            Version::V6 => {
                if let Some(v6) = v6 {
                    SocketAddr::new(IpAddr::V6(v6), STUN_PORT)
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
}
