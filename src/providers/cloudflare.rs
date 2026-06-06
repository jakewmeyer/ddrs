use std::net::IpAddr;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use compact_str::CompactString;
use reqwest::Response;
use reqwest_middleware::ClientWithMiddleware as HttpClient;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use smallvec::SmallVec;

use crate::client::{IpUpdate, IpVersion, Provider};

/// Cloudflare DNS update provider
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Cloudflare {
    zone: CompactString,
    api_token: SecretString,
    domains: SmallVec<[Domain; 2]>,
    #[serde(default = "default_api_url")]
    api_url: String,
}

fn default_api_url() -> String {
    "https://api.cloudflare.com/client/v4".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct Domain {
    name: CompactString,
    #[serde(default = "default_ttl")]
    ttl: u32,
    #[serde(default)]
    proxied: bool,
    #[serde(default = "default_comment")]
    comment: CompactString,
}

// TTL of 1 is Cloudflare's auto setting
fn default_ttl() -> u32 {
    1
}

fn default_comment() -> CompactString {
    let now: DateTime<Utc> = Utc::now();
    format!("Updated by DDRS @ {}", now.format("%Y-%m-%d %H:%M:%S UTC")).into()
}

#[derive(Debug, Deserialize)]
struct CloudflareResponse<T> {
    success: bool,
    result: Option<T>,
    #[serde(default)]
    errors: Vec<CloudflareError>,
}

impl<T> CloudflareResponse<T> {
    fn error_summary(&self) -> Option<String> {
        let messages = self
            .errors
            .iter()
            .filter_map(|error| error.message.as_deref())
            .filter(|message| !message.is_empty())
            .collect::<Vec<_>>();

        if messages.is_empty() {
            None
        } else {
            Some(messages.join(", "))
        }
    }
}

#[derive(Debug, Deserialize)]
struct CloudflareError {
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZoneResult {
    id: String,
}

#[derive(Debug, Deserialize)]
struct RecordResult {
    id: String,
}

impl Cloudflare {
    async fn fetch_zone_id(&self, request: &HttpClient) -> Result<String> {
        let response = request
            .get(format!("{}/zones", self.api_url))
            .query(&[("name", &self.zone)])
            .bearer_auth(self.api_token.expose_secret())
            .send()
            .await?;
        let zones =
            parse_cloudflare_response::<Vec<ZoneResult>>(response, "list Cloudflare zones").await?;
        let zone_result = zones
            .result
            .ok_or(anyhow!("failed to list Cloudflare zones: missing result"))?;
        Ok(zone_result
            .first()
            .ok_or(anyhow!("failed to find a matching Cloudflare zone"))?
            .id
            .clone())
    }

    async fn fetch_dns_records(
        &self,
        request: &HttpClient,
        zone_id: &str,
        record_type: &str,
        domain: &Domain,
    ) -> Result<Vec<RecordResult>> {
        let response = request
            .get(format!("{}/zones/{}/dns_records", self.api_url, zone_id))
            .query(&[("name", &domain.name)])
            .query(&[("type", record_type)])
            .bearer_auth(self.api_token.expose_secret())
            .send()
            .await?;
        let records = parse_cloudflare_response::<Vec<RecordResult>>(
            response,
            &format!("list Cloudflare DNS records for {}", domain.name),
        )
        .await?;
        records.result.ok_or(anyhow!(
            "failed to list Cloudflare DNS records for {}: missing result",
            domain.name
        ))
    }

    async fn update_dns_record(
        &self,
        request: &HttpClient,
        zone_id: &str,
        record_id: &str,
        record_type: &str,
        domain: &Domain,
        address: &IpAddr,
    ) -> Result<()> {
        let response = request
            .put(format!(
                "{}/zones/{}/dns_records/{}",
                self.api_url, zone_id, record_id,
            ))
            .json(&json!({
                "type": record_type,
                "name": domain.name,
                "content": address,
                "ttl": domain.ttl,
                "proxied": domain.proxied,
                "comment": domain.comment,
            }))
            .bearer_auth(self.api_token.expose_secret())
            .send()
            .await?;
        parse_cloudflare_response::<Value>(
            response,
            &format!("update Cloudflare domain ({}) record", domain.name),
        )
        .await?;
        Ok(())
    }

    async fn create_dns_record(
        &self,
        request: &HttpClient,
        zone_id: &str,
        record_type: &str,
        domain: &Domain,
        address: &IpAddr,
    ) -> Result<()> {
        let response = request
            .post(format!("{}/zones/{}/dns_records", self.api_url, zone_id))
            .json(&json!({
                "type": record_type,
                "name": domain.name,
                "content": address,
                "ttl": domain.ttl,
                "proxied": domain.proxied,
                "comment": domain.comment,
            }))
            .bearer_auth(self.api_token.expose_secret())
            .send()
            .await?;
        parse_cloudflare_response::<Value>(
            response,
            &format!("create Cloudflare domain ({}) record", domain.name),
        )
        .await?;
        Ok(())
    }
}

async fn parse_cloudflare_response<T>(
    response: Response,
    action: &str,
) -> Result<CloudflareResponse<T>>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let body = response.text().await?;
    let parsed = serde_json::from_str::<CloudflareResponse<T>>(&body);

    if !status.is_success() {
        let detail = parsed
            .as_ref()
            .ok()
            .and_then(CloudflareResponse::error_summary)
            .or_else(|| body_snippet(&body));
        let detail = detail.map_or_else(String::new, |detail| format!(": {detail}"));

        return Err(anyhow!("failed to {action}: HTTP {status}{detail}"));
    }

    let parsed = parsed
        .map_err(|error| anyhow!("failed to parse Cloudflare response for {action}: {error}"))?;
    if !parsed.success {
        let detail = parsed
            .error_summary()
            .unwrap_or_else(|| "Cloudflare API returned success=false".to_string());
        return Err(anyhow!("failed to {action}: {detail}"));
    }

    Ok(parsed)
}

fn body_snippet(body: &str) -> Option<String> {
    const MAX_BODY_CHARS: usize = 200;

    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let mut end = body.len();
    for (count, (index, _)) in body.char_indices().enumerate() {
        if count == MAX_BODY_CHARS {
            end = index;
            break;
        }
    }

    let mut snippet = body[..end].to_string();
    if end < body.len() {
        snippet.push_str("...");
    }
    Some(snippet)
}

#[async_trait]
#[typetag::deserialize(name = "cloudflare")]
impl Provider for Cloudflare {
    fn validate_config(&self) -> Result<()> {
        if self.domains.is_empty() {
            return Err(anyhow!("no domains configured for Cloudflare provider"));
        }
        Ok(())
    }

    async fn update(&self, update: IpUpdate, request: HttpClient) -> Result<bool> {
        let zone_id = self.fetch_zone_id(&request).await?;
        for domain in &self.domains {
            for (version, addr) in update.iter() {
                let record_type = match version {
                    IpVersion::V4 => "A",
                    IpVersion::V6 => "AAAA",
                };
                if let Some(record) = self
                    .fetch_dns_records(&request, &zone_id, record_type, domain)
                    .await?
                    .first()
                {
                    self.update_dns_record(
                        &request,
                        &zone_id,
                        &record.id,
                        record_type,
                        domain,
                        &addr,
                    )
                    .await?;
                } else {
                    self.create_dns_record(&request, &zone_id, record_type, domain, &addr)
                        .await?;
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use reqwest::Client as InnerHttpClient;
    use reqwest_middleware::ClientBuilder;
    use smallvec::smallvec;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{bearer_token, method, path, query_param},
    };

    use super::*;

    const UPDATE_BOTH: IpUpdate = IpUpdate {
        v4: Some(Ipv4Addr::LOCALHOST),
        v6: Some(Ipv6Addr::LOCALHOST),
    };
    const UPDATE_V4: IpUpdate = IpUpdate {
        v4: Some(Ipv4Addr::LOCALHOST),
        v6: None,
    };
    const UPDATE_V6: IpUpdate = IpUpdate {
        v4: None,
        v6: Some(Ipv6Addr::LOCALHOST),
    };

    #[tokio::test]
    async fn test_cloudflare_bad_token() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "bad_token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
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

        let error = provider.update(UPDATE_BOTH, http).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "failed to list Cloudflare zones: HTTP 403 Forbidden: Invalid access token"
        );
    }

    #[tokio::test]
    async fn test_cloudflare_rejects_non_success_http_status() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "success": true,
                "errors": [],
                "messages": [],
                "result": [
                    {
                        "id": "023e105f4ecef8ad9ca31a8372d0c353",
                        "name": "example.com",
                    }
                ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let error = provider.update(UPDATE_BOTH, http).await.unwrap_err();
        let error = error.to_string();
        assert!(error.contains("failed to list Cloudflare zones: HTTP 500 Internal Server Error"));
        assert!(error.contains("\"success\":true"));
    }

    #[tokio::test]
    async fn test_cloudflare_rejects_api_failure_on_success_http_status() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": false,
                "errors": [
                    {
                        "code": 10000,
                        "message": "Authentication error"
                    }
                ],
                "messages": [],
                "result": [
                    {
                        "id": "023e105f4ecef8ad9ca31a8372d0c353",
                        "name": "example.com",
                    }
                ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let error = provider.update(UPDATE_BOTH, http).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "failed to list Cloudflare zones: Authentication error"
        );
    }

    #[tokio::test]
    async fn test_cloudflare_no_matching_zones() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "errors": [],
                "messages": [],
                "result": []
            })))
            .mount(&mock)
            .await;

        let error = provider.update(UPDATE_BOTH, http).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "failed to find a matching Cloudflare zone"
        );
    }

    #[tokio::test]
    async fn test_cloudflare_no_domains() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
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

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn test_cloudflare_update_both() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";
        let v4_record_id = "89c0cbe7d4554cd29120ed30d8e6ef17";
        let v6_record_id = "25f1b0da807484b9668f812480f5c734";

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.domains[0].name))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.domains[0].name))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .mount(&mock)
            .await;

        Mock::given(method("PUT"))
            .and(path(format!("/zones/{zone_id}/dns_records/{v6_record_id}")))
            .and(bearer_token(provider.api_token.expose_secret()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_update_ipv4() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";
        let v4_record_id = "89c0cbe7d4554cd29120ed30d8e6ef17";

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.domains[0].name))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_V4, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_update_v6() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";
        let v6_record_id = "25f1b0da807484b9668f812480f5c734";

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.domains[0].name))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .expect(1)
            .named("Update DNS Record")
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_V6, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_create_both() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.domains[0].name))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.domains[0].name))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/zones/{zone_id}/dns_records")))
            .and(bearer_token(provider.api_token.expose_secret()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_cloudflare_create_v4_update_v6() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let zone_id = "023e105f4ecef8ad9ca31a8372d0c353";
        let v4_record_id = "89c0cbe7d4554cd29120ed30d8e6ef17";

        let provider = Cloudflare {
            zone: "example.com".into(),
            api_token: "token".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                ttl: 1,
                proxied: true,
                comment: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.zone))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.domains[0].name))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .and(query_param("name", &*provider.domains[0].name))
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
            .and(bearer_token(provider.api_token.expose_secret()))
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
            .and(bearer_token(provider.api_token.expose_secret()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [],
                "messages": [],
                "success": true,
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }
}
