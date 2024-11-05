use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use smallvec::SmallVec;

use crate::client::{IpUpdate, IpVersion, Provider};

/// Cloudflare DNS update provider
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Cloudflare {
    zone: String,
    api_token: String,
    domains: SmallVec<[Domains; 2]>,
    #[serde(default = "default_api_url")]
    api_url: String,
}

fn default_api_url() -> String {
    "https://api.cloudflare.com/client/v4".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    result: Option<Vec<ZoneResult>>,
    #[serde(rename = "errors")]
    _errors: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct ZoneResult {
    id: String,
}

/// Records lookup response
#[derive(Debug, Deserialize)]
struct RecordsList {
    result: Option<Vec<RecordResult>>,
    #[serde(rename = "errors")]
    _errors: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct RecordResult {
    id: String,
}

#[derive(Debug, Deserialize)]
struct UpdatedResult {
    #[serde(rename = "errors")]
    _errors: Vec<Value>,
    #[serde(rename = "messages")]
    _messages: Vec<Value>,
    success: bool,
}

#[derive(Debug, Deserialize)]
struct CreatedResult {
    #[serde(rename = "errors")]
    _errors: Vec<Value>,
    #[serde(rename = "messages")]
    _messages: Vec<Value>,
    success: bool,
}

#[async_trait]
#[typetag::serde(name = "cloudflare")]
impl Provider for Cloudflare {
    async fn update(&self, update: IpUpdate, request: Client) -> Result<bool> {
        let zones = request
            .get(format!("{}/zones", self.api_url))
            .query(&[("name", &self.zone)])
            .bearer_auth(&self.api_token)
            .send()
            .await?
            .json::<ZoneList>()
            .await?;
        let zone_result = zones
            .result
            .ok_or(anyhow!("Failed to list Cloudflare zones"))?;
        let zone_id = &zone_result
            .first()
            .ok_or(anyhow!("Failed to find a matching Cloudflare zone"))?
            .id;
        for domain in &self.domains {
            for (version, address) in update.as_array() {
                if address.is_none() {
                    continue;
                }
                let record_type = match version {
                    IpVersion::V4 => "A",
                    IpVersion::V6 => "AAAA",
                };
                let records = request
                    .get(format!("{}/zones/{zone_id}/dns_records", self.api_url))
                    .query(&[("name", &domain.name)])
                    .query(&[("type", record_type)])
                    .bearer_auth(&self.api_token)
                    .send()
                    .await?
                    .json::<RecordsList>()
                    .await?;
                if let Some(record) = records.result.and_then(|vec| vec.into_iter().next()) {
                    let updated = request
                        .put(format!(
                            "{}/zones/{zone_id}/dns_records/{}",
                            self.api_url, record.id,
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
                    if !updated.success {
                        return Err(anyhow!(
                            "Failed to update Cloudflare domain ({}) record",
                            domain.name
                        ));
                    }
                    continue;
                }
                let created = request
                    .post(format!("{}/zones/{zone_id}/dns_records", self.api_url))
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
                if !created.success {
                    return Err(anyhow!(
                        "Failed to create Cloudflare domain ({}) record",
                        domain.name
                    ));
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use smallvec::smallvec;
    use wiremock::{
        matchers::{bearer_token, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    const UPDATE_BOTH: IpUpdate = IpUpdate {
        v4: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        v6: Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
    };
    const UPDATE_V4: IpUpdate = IpUpdate {
        v4: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        v6: None,
    };
    const UPDATE_V6: IpUpdate = IpUpdate {
        v4: None,
        v6: Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
    };

    #[tokio::test]
    async fn test_cloudflare_bad_token() {
        let mock = MockServer::start().await;

        let provider = Cloudflare {
            zone: "example.com".to_string(),
            api_token: "bad_token".to_string(),
            domains: smallvec![Domains {
                name: "example.com".to_string(),
                ttl: 1,
                proxied: true,
                comment: None,
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.zone))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "success": false,
                "errors": [
                    {
                        "code": 9109,
                        "message": "Invalid access token"
                    }
                ],
                "messages": [],
                "result": null
            })))
            .mount(&mock)
            .await;

        let error = provider
            .update(UPDATE_BOTH, Client::new())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Failed to list Cloudflare zones");
    }

    #[tokio::test]
    async fn test_cloudflare_no_matching_zones() {
        let mock = MockServer::start().await;

        let provider = Cloudflare {
            zone: "example.com".to_string(),
            api_token: "token".to_string(),
            domains: smallvec![Domains {
                name: "example.com".to_string(),
                ttl: 1,
                proxied: true,
                comment: None,
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "errors": [],
                "messages": [],
                "result": []
            })))
            .mount(&mock)
            .await;

        let error = provider
            .update(UPDATE_BOTH, Client::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Failed to find a matching Cloudflare zone"
        );
    }

    #[tokio::test]
    async fn test_cloudflare_no_domains() {
        let mock = MockServer::start().await;

        let provider = Cloudflare {
            zone: "example.com".to_string(),
            api_token: "token".to_string(),
            domains: smallvec![],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "id": "023e105f4ecef8ad9ca31a8372d0c353",
                  "name": "example.com",
                }
              ]
            })))
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, Client::new()).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_update_both() {
        let mock = MockServer::start().await;
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";
        let v4_record_id = "89c0cbe7d4554cd29120ed30d8e6ef17";
        let v6_record_id = "25f1b0da807484b9668f812480f5c734";

        let provider = Cloudflare {
            zone: "example.com".to_string(),
            api_token: "token".to_string(),
            domains: smallvec![Domains {
                name: "example.com".to_string(),
                ttl: 1,
                proxied: true,
                comment: Some("Created by DDRS".to_string()),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "id": zone_id,
                  "name": "example.com",
                }
              ]
            })))
            .mount(&mock)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.domains[0].name))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "comment": "Created by DDRS",
                  "name": "example.com",
                  "proxied": true,
                  "ttl": 3600,
                  "content": "192.168.1.1",
                  "type": "A",
                  "id": v4_record_id,
                },
              ]
            })))
            .mount(&mock)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.domains[0].name))
            .and(query_param("type", "AAAA"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "comment": "Created by DDRS",
                  "name": "example.com",
                  "proxied": true,
                  "ttl": 3600,
                  "content": "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
                  "type": "AAAA",
                  "id": v6_record_id,
                }
              ]
            })))
            .mount(&mock)
            .await;

        Mock::given(method("PUT"))
            .and(path(format!("/zones/{zone_id}/dns_records/{v4_record_id}")))
            .and(bearer_token(&provider.api_token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .mount(&mock)
            .await;

        Mock::given(method("PUT"))
            .and(path(format!("/zones/{zone_id}/dns_records/{v6_record_id}")))
            .and(bearer_token(&provider.api_token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, Client::new()).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_update_ipv4() {
        let mock = MockServer::start().await;
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";
        let v4_record_id = "89c0cbe7d4554cd29120ed30d8e6ef17";

        let provider = Cloudflare {
            zone: "example.com".to_string(),
            api_token: "token".to_string(),
            domains: smallvec![Domains {
                name: "example.com".to_string(),
                ttl: 1,
                proxied: true,
                comment: Some("Created by DDRS".to_string()),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "id": zone_id,
                  "name": "example.com",
                }
              ]
            })))
            .mount(&mock)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.domains[0].name))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "comment": "Created by DDRS",
                  "name": "example.com",
                  "proxied": true,
                  "ttl": 3600,
                  "content": "192.168.1.1",
                  "type": "A",
                  "id": v4_record_id,
                },
              ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("PUT"))
            .and(path(format!("/zones/{zone_id}/dns_records/{v4_record_id}")))
            .and(bearer_token(&provider.api_token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_V4, Client::new()).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_update_v6() {
        let mock = MockServer::start().await;
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";
        let v6_record_id = "25f1b0da807484b9668f812480f5c734";

        let provider = Cloudflare {
            zone: "example.com".to_string(),
            api_token: "token".to_string(),
            domains: smallvec![Domains {
                name: "example.com".to_string(),
                ttl: 1,
                proxied: true,
                comment: Some("Created by DDRS".to_string()),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "id": zone_id,
                  "name": "example.com",
                }
              ]
            })))
            .expect(1)
            .named("List Zones")
            .mount(&mock)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.domains[0].name))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "comment": "Created by DDRS",
                  "name": "example.com",
                  "proxied": true,
                  "ttl": 3600,
                  "content": "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
                  "type": "AAAA",
                  "id": v6_record_id,
                }
              ]
            })))
            .expect(1)
            .named("List DNS Records")
            .mount(&mock)
            .await;

        Mock::given(method("PUT"))
            .and(path(format!("/zones/{zone_id}/dns_records/{v6_record_id}")))
            .and(bearer_token(&provider.api_token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .expect(1)
            .named("Update DNS Record")
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_V6, Client::new()).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_create_both() {
        let mock = MockServer::start().await;
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";

        let provider = Cloudflare {
            zone: "example.com".to_string(),
            api_token: "token".to_string(),
            domains: smallvec![Domains {
                name: "example.com".to_string(),
                ttl: 1,
                proxied: true,
                comment: Some("Created by DDRS".to_string()),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "id": zone_id,
                  "name": "example.com",
                }
              ]
            })))
            .mount(&mock)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.domains[0].name))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": []
            })))
            .mount(&mock)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.domains[0].name))
            .and(query_param("type", "AAAA"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": []
            })))
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, Client::new()).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_create_v4_update_v6() {
        let mock = MockServer::start().await;
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";
        let v4_record_id = "89c0cbe7d4554cd29120ed30d8e6ef17";

        let provider = Cloudflare {
            zone: "example.com".to_string(),
            api_token: "token".to_string(),
            domains: smallvec![Domains {
                name: "example.com".to_string(),
                ttl: 1,
                proxied: true,
                comment: Some("Created by DDRS".to_string()),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "id": zone_id,
                  "name": "example.com",
                }
              ]
            })))
            .mount(&mock)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.domains[0].name))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": [
                {
                  "comment": "Created by DDRS",
                  "name": "example.com",
                  "proxied": true,
                  "ttl": 3600,
                  "content": "192.168.1.1",
                  "type": "A",
                  "id": v4_record_id,
                },
              ]
            })))
            .mount(&mock)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .and(query_param("name", &provider.domains[0].name))
            .and(query_param("type", "AAAA"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
              "errors": [],
              "messages": [],
              "success": true,
              "result": []
            })))
            .mount(&mock)
            .await;

        Mock::given(method("PUT"))
            .and(path(format!("/zones/{zone_id}/dns_records/{v4_record_id}")))
            .and(bearer_token(&provider.api_token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(&provider.api_token))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, Client::new()).await.unwrap();
        assert!(result);
    }
}
