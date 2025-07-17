use std::time::Duration;

use serde::{Deserialize, Serialize};
use smallvec::{SmallVec, smallvec};

use crate::client::{IpSource, IpVersion, Provider};

/// Client configuration
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
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
    /// STUN servers with port
    pub stun_urls: SmallVec<[String; 3]>,
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
            interval: Duration::from_secs(30),
            source: IpSource::Stun,
            versions: smallvec![IpVersion::V4],
            dry_run: false,
            stun_urls: smallvec![
                String::from("stun://stun.l.google.com:19302"),
                String::from("stun://stun.cloudflare.com:3478"),
                String::from("stun://global.stun.twilio.com:3478"),
            ],
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
