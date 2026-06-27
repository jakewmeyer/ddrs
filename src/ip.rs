use std::fmt::{self, Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// IP version without associated address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpVersion {
    V4,
    V6,
}

impl Display for IpVersion {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::V4 => f.write_str("IPv4"),
            Self::V6 => f.write_str("IPv6"),
        }
    }
}

/// Update sent to each provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpUpdate {
    pub v4: Option<Ipv4Addr>,
    pub v6: Option<Ipv6Addr>,
}

impl IpUpdate {
    pub fn iter(&self) -> impl Iterator<Item = (IpVersion, IpAddr)> + '_ {
        let v4 = self.v4.map(|addr| (IpVersion::V4, IpAddr::V4(addr)));
        let v6 = self.v6.map(|addr| (IpVersion::V6, IpAddr::V6(addr)));
        [v4, v6].into_iter().flatten()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.v4.is_none() && self.v6.is_none()
    }

    pub(crate) fn changed_since(&self, cached: Option<&Self>) -> Self {
        Self {
            v4: self
                .v4
                .filter(|observed| cached.and_then(|cached| cached.v4) != Some(*observed)),
            v6: self
                .v6
                .filter(|observed| cached.and_then(|cached| cached.v6) != Some(*observed)),
        }
    }

    pub(crate) fn merge_into_cache(self, cached: Option<Self>) -> Self {
        let mut merged = cached.unwrap_or(Self { v4: None, v6: None });

        if self.v4.is_some() {
            merged.v4 = self.v4;
        }
        if self.v6.is_some() {
            merged.v6 = self.v6;
        }

        merged
    }
}

impl Display for IpUpdate {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(
            f,
            "v4: {}, v6: {}",
            self.v4.map_or("None".to_string(), |ip| ip.to_string()),
            self.v6.map_or("None".to_string(), |ip| ip.to_string())
        )
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::IpUpdate;

    const OLD_V4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
    const NEW_V4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
    const OLD_V6: Ipv6Addr = Ipv6Addr::LOCALHOST;
    const NEW_V6: Ipv6Addr = Ipv6Addr::UNSPECIFIED;

    #[test]
    fn ip_update_changed_since_cache_only_keeps_observed_changes() {
        let cached = IpUpdate {
            v4: Some(OLD_V4),
            v6: Some(OLD_V6),
        };
        let observed = IpUpdate {
            v4: Some(NEW_V4),
            v6: None,
        };

        assert_eq!(
            observed.changed_since(Some(&cached)),
            IpUpdate {
                v4: Some(NEW_V4),
                v6: None,
            }
        );
    }

    #[test]
    fn ip_update_changed_since_cache_skips_unchanged_values() {
        let cached = IpUpdate {
            v4: Some(OLD_V4),
            v6: Some(OLD_V6),
        };
        let observed = IpUpdate {
            v4: Some(OLD_V4),
            v6: Some(NEW_V6),
        };

        assert_eq!(
            observed.changed_since(Some(&cached)),
            IpUpdate {
                v4: None,
                v6: Some(NEW_V6),
            }
        );
    }

    #[test]
    fn ip_update_merge_into_cache_preserves_unobserved_cached_values() {
        let cached = IpUpdate {
            v4: Some(OLD_V4),
            v6: Some(OLD_V6),
        };
        let observed = IpUpdate {
            v4: Some(NEW_V4),
            v6: None,
        };

        assert_eq!(
            observed.merge_into_cache(Some(cached)),
            IpUpdate {
                v4: Some(NEW_V4),
                v6: Some(OLD_V6),
            }
        );
    }

    #[test]
    fn ip_update_merge_into_cache_uses_observed_values_without_cache() {
        let observed = IpUpdate {
            v4: None,
            v6: Some(NEW_V6),
        };

        assert_eq!(
            observed.merge_into_cache(None),
            IpUpdate {
                v4: None,
                v6: Some(NEW_V6),
            }
        );
    }
}
