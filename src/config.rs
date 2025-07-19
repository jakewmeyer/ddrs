use std::time::Duration;

use compact_str::CompactString;
use serde::Deserialize;
use smallvec::{SmallVec, smallvec};

use crate::client::{IpSource, IpVersion, Provider};

/// Client configuration
#[derive(Debug, Deserialize)]
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
    // Total request timeout
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    /// Request connect timeout
    #[serde(with = "humantime_serde")]
    pub connect_timeout: Duration,
    /// File path to cache file
    pub cache_path: CompactString,
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
            source: IpSource::Http,
            versions: smallvec![IpVersion::V4],
            dry_run: false,
            timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            cache_path: "/var/cache/ddrs/ddrs.bin".into(),
            http_ipv4: smallvec![
                String::from("https://api.ipify.org"),
                String::from("https://ipv4.seeip.org"),
                String::from("https://ipv4.icanhazip.com"),
            ],
            http_ipv6: smallvec![
                String::from("https://api6.ipify.org"),
                String::from("https://ipv6.seeip.org"),
                String::from("https://ipv6.icanhazip.com"),
            ],
            providers: smallvec![],
        }
    }
}
