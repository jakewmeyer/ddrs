use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{
    client::{IpSource, IpVersion},
    providers::Provider,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    pub versions: Vec<IpVersion>,
    pub ip_source: IpSource,
    pub stun_server: String,
    pub stun_port: u16,
    pub http_ipv4: Vec<String>,
    pub http_ipv6: Vec<String>,
    pub domains: Vec<Box<dyn Provider>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            versions: vec![IpVersion::V4, IpVersion::V6],
            ip_source: IpSource::Stun,
            stun_server: "stun.l.google.com".to_string(),
            stun_port: 19302,
            http_ipv4: vec![
                String::from("https://api.ipify.org"),
                String::from("https://ipv4.icanhazip.com"),
                String::from("https://ipv4.seeip.org"),
            ],
            http_ipv6: vec![
                String::from("https://api6.ipify.org"),
                String::from("https://ipv6.icanhazip.com"),
                String::from("https://ipv6.seeip.org"),
            ],
            domains: vec![],
        }
    }
}
