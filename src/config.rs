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
    /// Source for address checks
    pub source: IpSource,
    /// IP versions to check/update
    pub versions: SmallVec<[IpVersion; 2]>,
    /// Toggle dry run mode
    pub dry_run: bool,
    /// STUN server address
    pub stun_url: String,
    /// STUN server port
    pub stun_port: u16,
    /// HTTP servers for IPv4 address checks
    pub http_ipv4: SmallVec<[String; 3]>,
    /// HTTP servers for IPv6 address checks
    pub http_ipv6: SmallVec<[String; 3]>,
    /// DNS update providers
    pub providers: SmallVec<[Box<dyn Provider>; 1]>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            source: IpSource::Stun,
            versions: smallvec![IpVersion::V4],
            dry_run: false,
            stun_url: String::from("stun.l.google.com"),
            stun_port: 19302,
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
