use std::time::Duration;

use serde::{Deserialize, Serialize};
use smallvec::{smallvec, SmallVec};

use crate::client::{IpSource, IpVersion, Provider};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    pub versions: SmallVec<[IpVersion; 2]>,
    pub source: IpSource,
    pub stun_addr: String,
    pub http_ipv4: SmallVec<[String; 3]>,
    pub http_ipv6: SmallVec<[String; 3]>,
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
