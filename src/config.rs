use std::{path::PathBuf, time::Duration};

use anyhow::{Result, anyhow};
use serde::{Deserialize, de};
use smallvec::{SmallVec, smallvec};
use url::Url;

use crate::client::{IpSource, IpVersion, Provider};

const MAX_RETRIES: u32 = 10;

/// Client configuration
#[derive(Debug, Deserialize)]
#[serde(try_from = "RawConfig")]
pub struct Config {
    /// Interval between IP address checks
    pub interval: NonZeroDuration,
    /// Source for address checks
    pub source: IpSource,
    /// IP versions to check/update
    pub versions: IpVersions,
    /// Toggle dry run mode
    pub dry_run: bool,
    // Total request timeout
    pub timeout: NonZeroDuration,
    /// Request connect timeout
    pub connect_timeout: NonZeroDuration,
    /// File path to cache file
    pub cache_path: PathBuf,
    /// HTTP request max retries
    pub retries: RetryCount,
    /// HTTP servers for IPv4 address checks
    pub http_ipv4: SmallVec<[Url; 3]>,
    /// HTTP servers for IPv6 address checks
    pub http_ipv6: SmallVec<[Url; 3]>,
    /// DNS update providers
    pub providers: SmallVec<[Box<dyn Provider>; 1]>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawConfig {
    #[serde(with = "humantime_serde")]
    interval: Duration,
    source: IpSource,
    versions: SmallVec<[IpVersion; 2]>,
    dry_run: bool,
    #[serde(with = "humantime_serde")]
    timeout: Duration,
    #[serde(with = "humantime_serde")]
    connect_timeout: Duration,
    cache_path: PathBuf,
    retries: u32,
    http_ipv4: SmallVec<[Url; 3]>,
    http_ipv6: SmallVec<[Url; 3]>,
    providers: SmallVec<[Box<dyn Provider>; 1]>,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            source: IpSource::Http,
            versions: smallvec![IpVersion::V4],
            dry_run: false,
            timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            cache_path: "/var/cache/ddrs".into(),
            retries: 1,
            http_ipv4: smallvec![
                parse_default_url("https://api.ipify.org"),
                parse_default_url("https://ipv4.seeip.org"),
                parse_default_url("https://ipv4.icanhazip.com"),
                parse_default_url("https://4.ident.me"),
            ],
            http_ipv6: smallvec![
                parse_default_url("https://api6.ipify.org"),
                parse_default_url("https://ipv6.seeip.org"),
                parse_default_url("https://ipv6.icanhazip.com"),
                parse_default_url("https://6.ident.me"),
            ],
            providers: smallvec![],
        }
    }
}

impl TryFrom<RawConfig> for Config {
    type Error = anyhow::Error;

    fn try_from(raw: RawConfig) -> Result<Self> {
        let interval = NonZeroDuration::new(raw.interval, "interval")?;
        let timeout = NonZeroDuration::new(raw.timeout, "timeout")?;
        let connect_timeout = NonZeroDuration::new(raw.connect_timeout, "connect_timeout")?;

        if connect_timeout.get() > timeout.get() {
            return Err(anyhow!("connect_timeout must not be greater than timeout"));
        }

        let versions = IpVersions::new(raw.versions)?;
        let retries = RetryCount::new(raw.retries)?;

        ensure_http_urls("http_ipv4", &raw.http_ipv4)?;
        ensure_http_urls("http_ipv6", &raw.http_ipv6)?;

        if matches!(raw.source, IpSource::Http) {
            if versions.contains(IpVersion::V4) && raw.http_ipv4.is_empty() {
                return Err(anyhow!(
                    "http_ipv4 must not be empty when source is http and versions includes v4"
                ));
            }
            if versions.contains(IpVersion::V6) && raw.http_ipv6.is_empty() {
                return Err(anyhow!(
                    "http_ipv6 must not be empty when source is http and versions includes v6"
                ));
            }
        }

        if raw.providers.is_empty() {
            return Err(anyhow!("no providers configured"));
        }

        Ok(Self {
            interval,
            source: raw.source,
            versions,
            dry_run: raw.dry_run,
            timeout,
            connect_timeout,
            cache_path: raw.cache_path,
            retries,
            http_ipv4: raw.http_ipv4,
            http_ipv6: raw.http_ipv6,
            providers: raw.providers,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonZeroDuration(Duration);

impl NonZeroDuration {
    fn new(duration: Duration, field: &str) -> Result<Self> {
        if duration.is_zero() {
            return Err(anyhow!("{field} must be greater than 0s"));
        }
        Ok(Self(duration))
    }

    pub fn get(self) -> Duration {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NonEmptyString(String);

impl NonEmptyString {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for NonEmptyString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() {
            return Err(de::Error::custom("must not be empty"));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpVersions {
    v4: bool,
    v6: bool,
}

impl IpVersions {
    fn new(versions: impl IntoIterator<Item = IpVersion>) -> Result<Self> {
        let mut parsed = Self {
            v4: false,
            v6: false,
        };

        for version in versions {
            match version {
                IpVersion::V4 if parsed.v4 => {
                    return Err(anyhow!("versions must not contain duplicates"));
                }
                IpVersion::V4 => parsed.v4 = true,
                IpVersion::V6 if parsed.v6 => {
                    return Err(anyhow!("versions must not contain duplicates"));
                }
                IpVersion::V6 => parsed.v6 = true,
            }
        }

        if !parsed.v4 && !parsed.v6 {
            return Err(anyhow!("versions must not be empty"));
        }

        Ok(parsed)
    }

    pub fn contains(self, version: IpVersion) -> bool {
        match version {
            IpVersion::V4 => self.v4,
            IpVersion::V6 => self.v6,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = IpVersion> {
        [
            self.v4.then_some(IpVersion::V4),
            self.v6.then_some(IpVersion::V6),
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryCount(u32);

impl RetryCount {
    fn new(retries: u32) -> Result<Self> {
        if retries > MAX_RETRIES {
            return Err(anyhow!("retries must not be greater than {MAX_RETRIES}"));
        }
        Ok(Self(retries))
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

fn ensure_http_urls(field: &str, urls: &[Url]) -> Result<()> {
    for url in urls {
        match url.scheme() {
            "http" | "https" => {}
            scheme => return Err(anyhow!("{field} URL must use http or https: {scheme}")),
        }
    }
    Ok(())
}

fn parse_default_url(url: &str) -> Url {
    url.parse()
        .expect("default HTTP lookup URL should be a valid URL")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDER_CONFIG: &str = r#"
[[providers]]
type = "cloudflare"
zone = "example.com"
api_token = "token"

[[providers.domains]]
name = "example.com"
"#;

    fn parse_config(config: &str) -> std::result::Result<Config, toml::de::Error> {
        toml::from_str(&format!("{config}\n{PROVIDER_CONFIG}"))
    }

    fn parse_error(config: &str) -> String {
        parse_config(config).unwrap_err().to_string()
    }

    #[test]
    fn parses_minimal_config_with_defaults() {
        let config = parse_config("").unwrap();

        assert_eq!(config.interval.get(), Duration::from_secs(30));
        assert_eq!(config.timeout.get(), Duration::from_secs(10));
        assert_eq!(config.connect_timeout.get(), Duration::from_secs(5));
        assert_eq!(config.cache_path, PathBuf::from("/var/cache/ddrs"));
        assert_eq!(config.retries.get(), 1);
        assert!(config.versions.contains(IpVersion::V4));
        assert!(!config.versions.contains(IpVersion::V6));
        assert_eq!(config.http_ipv4.len(), 4);
        assert_eq!(config.http_ipv6.len(), 4);
        assert_eq!(config.providers.len(), 1);
    }

    #[test]
    fn rejects_missing_providers() {
        let error = toml::from_str::<Config>("").unwrap_err().to_string();

        assert!(error.contains("no providers configured"));
    }

    #[test]
    fn rejects_zero_durations() {
        assert!(parse_error(r#"interval = "0s""#).contains("interval must be greater than 0s"));
        assert!(parse_error(r#"timeout = "0s""#).contains("timeout must be greater than 0s"));
        assert!(
            parse_error(r#"connect_timeout = "0s""#)
                .contains("connect_timeout must be greater than 0s")
        );
    }

    #[test]
    fn rejects_connect_timeout_greater_than_timeout() {
        let error = parse_error(
            r#"
timeout = "5s"
connect_timeout = "6s"
"#,
        );

        assert!(error.contains("connect_timeout must not be greater than timeout"));
    }

    #[test]
    fn rejects_empty_versions() {
        let error = parse_error("versions = []");

        assert!(error.contains("versions must not be empty"));
    }

    #[test]
    fn rejects_duplicate_versions() {
        let error = parse_error(r#"versions = ["v4", "v4"]"#);

        assert!(error.contains("versions must not contain duplicates"));
    }

    #[test]
    fn parses_versions_as_a_set() {
        let config = parse_config(r#"versions = ["v6", "v4"]"#).unwrap();

        assert!(config.versions.contains(IpVersion::V4));
        assert!(config.versions.contains(IpVersion::V6));
        assert_eq!(
            config.versions.iter().collect::<Vec<_>>(),
            [IpVersion::V4, IpVersion::V6]
        );
    }

    #[test]
    fn rejects_missing_http_lookup_urls_for_selected_versions() {
        let v4_error = parse_error("http_ipv4 = []");
        let v6_error = parse_error(
            r#"
versions = ["v6"]
http_ipv6 = []
"#,
        );

        assert!(
            v4_error.contains(
                "http_ipv4 must not be empty when source is http and versions includes v4"
            )
        );
        assert!(
            v6_error.contains(
                "http_ipv6 must not be empty when source is http and versions includes v6"
            )
        );
    }

    #[test]
    fn rejects_non_http_lookup_urls() {
        let error = parse_error(r#"http_ipv4 = ["ftp://example.com"]"#);

        assert!(error.contains("http_ipv4 URL must use http or https: ftp"));
    }

    #[test]
    fn allows_empty_http_urls_for_interface_source() {
        let config = parse_config(
            r#"
versions = ["v4", "v6"]
http_ipv4 = []
http_ipv6 = []

[source]
type = "interface"
name = "en0"
"#,
        )
        .unwrap();

        assert!(matches!(config.source, IpSource::Interface(_)));
        assert!(config.http_ipv4.is_empty());
        assert!(config.http_ipv6.is_empty());
    }

    #[test]
    fn rejects_empty_interface_name() {
        let error = parse_error(
            r#"
[source]
type = "interface"
name = ""
"#,
        );

        assert!(error.contains("must not be empty"));
    }

    #[test]
    fn rejects_retry_count_above_limit() {
        let error = parse_error("retries = 11");

        assert!(error.contains("retries must not be greater than 10"));
    }
}
