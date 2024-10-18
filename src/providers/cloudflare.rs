use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use smallvec::SmallVec;
use tracing::{debug, error};

use crate::client::{IpUpdate, IpVersion, Provider};

const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";

/// Cloudflare DNS update provider
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Cloudflare {
    zone: String,
    api_token: String,
    domains: SmallVec<[Domains; 2]>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
struct Domains {
    name: String,
    ttl: u32,
    proxied: bool,
    comment: Option<String>,
}

/// Zone lookup response
#[derive(Debug, Deserialize)]
struct ZoneList {
    result: Vec<ZoneResult>,
}

#[derive(Debug, Deserialize)]
struct ZoneResult {
    id: String,
}

/// Records lookup response
#[derive(Debug, Deserialize)]
struct RecordsList {
    result: Vec<RecordResult>,
}

#[derive(Debug, Deserialize)]
struct RecordResult {
    id: String,
}

#[derive(Debug, Deserialize)]
struct UpdatedResult {
    #[serde(rename = "errors")]
    _errors: Vec<Option<serde_json::Value>>,
    #[serde(rename = "messages")]
    _messages: Vec<Option<serde_json::Value>>,
    success: bool,
}

#[derive(Debug, Deserialize)]
struct CreatedResult {
    #[serde(rename = "errors")]
    _errors: Vec<Option<serde_json::Value>>,
    #[serde(rename = "messages")]
    _messages: Vec<Option<serde_json::Value>>,
    success: bool,
}

#[async_trait]
#[typetag::serde(name = "cloudflare")]
impl Provider for Cloudflare {
    async fn update(&self, update: &IpUpdate, request: &Client) -> Result<bool> {
        let zones = request
            .get(format!("{CLOUDFLARE_API}/zones"))
            .query(&[("name", &self.zone)])
            .bearer_auth(&self.api_token)
            .send()
            .await?
            .json::<ZoneList>()
            .await?;
        let zone_id = &zones.result.first().ok_or(anyhow!("No zone found"))?.id;
        debug!("Found zone ID: {}", zone_id);
        for domain in &self.domains {
            for (version, address) in update.as_array() {
                if let Some(address) = address {
                    let record_type = match version {
                        IpVersion::V4 => "A",
                        IpVersion::V6 => "AAAA",
                    };
                    let records = request
                        .get(format!("{CLOUDFLARE_API}/zones/{zone_id}/dns_records"))
                        .query(&[("name", &domain.name)])
                        .query(&[("type", record_type)])
                        .bearer_auth(&self.api_token)
                        .send()
                        .await?
                        .json::<RecordsList>()
                        .await?;
                    if let Some(record) = records.result.first() {
                        debug!(
                            "Updating {:?} record for {} to {}",
                            version, domain.name, address
                        );
                        let updated = request
                            .put(format!(
                                "{CLOUDFLARE_API}/zones/{zone_id}/dns_records/{0}",
                                record.id
                            ))
                            .json(&json!({
                                "type": record_type,
                                "name": domain.name,
                                "content": address,
                                "ttl": domain.ttl,
                                "proxied": domain.proxied,
                                "comment": domain.comment,
                            }))
                            .bearer_auth(&self.api_token)
                            .send()
                            .await?
                            .json::<UpdatedResult>()
                            .await?;
                        if updated.success {
                            debug!("Record updated: {:#?}", updated);
                        } else {
                            error!("Failed to update domain ({}) record: {:#?}", domain.name, updated);
                            return Err(anyhow!("Failed to update domain ({}) record", domain.name));
                        }
                    } else {
                        debug!(
                            "Creating {:?} record for {} to {}",
                            version, domain.name, address
                        );
                        let created = request
                            .post(format!("{CLOUDFLARE_API}/zones/{zone_id}/dns_records"))
                            .json(&json!({
                                "type": record_type,
                                "name": domain.name,
                                "content": address,
                                "ttl": domain.ttl,
                                "proxied": domain.proxied,
                                "comment": domain.comment,
                            }))
                            .bearer_auth(&self.api_token)
                            .send()
                            .await?
                            .json::<CreatedResult>()
                            .await?;
                        if created.success {
                            debug!("Record created: {:#?}", created);
                        } else {
                            error!("Failed to create domain ({}) record: {:#?}", domain.name, created);
                            return Err(anyhow!("Failed to create domain ({}) record", domain.name));
                        }
                    };
                }
            }
        }
        Ok(true)
    }
}
