use std::time::Duration;

use serde::{Deserialize, Serialize};
use smallvec::{smallvec, SmallVec};

use crate::client::{IpSource, IpVersion, Provider};

/// Client configuration
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    /// Interval between IP address checks
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    /// IP address versions to use in updates
    pub versions: SmallVec<[IpVersion; 2]>,
    /// Source of the IP address
    pub source: IpSource,
    /// STUN server address with port
    pub stun_addr: String,
    /// HTTP servers for IPv4 address checks
    pub http_ipv4: SmallVec<[String; 3]>,
    /// HTTP servers for IPv6 address checks
    pub http_ipv6: SmallVec<[String; 3]>,
    /// DNS update providers
    pub providers: SmallVec<[Box<dyn Provider>; 3]>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            versions: smallvec![IpVersion::V4, IpVersion::V6],
            source: IpSource::Stun,
            stun_addr: String::from("stun.l.google.com:19302"),
            http_ipv4: smallvec![
                String::from("https://api.ipify.org"),
                String::from("https://ipv4.icanhazip.com"),
                String::from("https://ipv4.seeip.org"),
            ],
            http_ipv6: smallvec![
                String::from("https://api6.ipify.org"),
                String::from("https://ipv6.icanhazip.com"),
                String::from("https://ipv6.seeip.org"),
            ],
            providers: smallvec![],
        }
    }
}
