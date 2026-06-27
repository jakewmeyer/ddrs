use std::net::IpAddr;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use compact_str::CompactString;
use reqwest::Response;
use reqwest_middleware::ClientWithMiddleware as HttpClient;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use smallvec::SmallVec;

use crate::ip::{IpUpdate, IpVersion};
use crate::providers::Provider;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Porkbun {
    api_key: SecretString,
    secret_api_key: SecretString,
    #[serde(default = "default_api_url")]
    api_url: String,
    domains: SmallVec<[Domain; 2]>,
}

fn default_api_url() -> String {
    "https://api.porkbun.com/api/json/v3".to_string()
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct Domain {
    name: CompactString,
    subdomain: Option<CompactString>,
    #[serde(default = "default_ttl")]
    ttl: u32,
    #[serde(default = "default_comment")]
    notes: CompactString,
}

fn default_ttl() -> u32 {
    600
}

fn default_comment() -> CompactString {
    let now: DateTime<Utc> = Utc::now();
    format!("Updated by DDRS @ {}", now.format("%Y-%m-%d %H:%M:%S UTC")).into()
}

#[derive(Serialize)]
struct CreateRecordBody<'a> {
    secretapikey: &'a str,
    apikey: &'a str,
    #[serde(rename = "type")]
    record_type: &'a str,
    content: &'a IpAddr,
    ttl: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a CompactString>,
    notes: &'a str,
}

#[derive(Debug, Serialize)]
struct UpdateRecordBody<'a> {
    secretapikey: &'a str,
    apikey: &'a str,
    #[serde(rename = "type")]
    record_type: &'a str,
    content: &'a IpAddr,
    ttl: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a CompactString>,
    notes: &'a str,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
enum ResponseStatus {
    Success,
    Error,
}

#[derive(Debug, Deserialize)]
struct PorkbunResponse<T> {
    status: ResponseStatus,
    message: Option<String>,
    #[serde(flatten)]
    data: T,
}

#[derive(Debug, Deserialize)]
struct EmptyResult {}

#[derive(Debug, Deserialize)]
struct RecordResult {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ListRecordsResult {
    records: Option<Vec<RecordResult>>,
}

impl Porkbun {
    async fn fetch_dns_records(
        &self,
        request: &HttpClient,
        record_type: &str,
        domain: &Domain,
    ) -> Result<Vec<RecordResult>> {
        let mut url = format!(
            "{}/dns/retrieveByNameType/{}/{}",
            self.api_url, domain.name, record_type
        );
        if let Some(subdomain) = domain.subdomain.as_ref() {
            url.push('/');
            url.push_str(subdomain);
        }
        let response = request
            .post(url)
            .json(&json!({
                "secretapikey": self.secret_api_key.expose_secret(),
                "apikey": self.api_key.expose_secret(),
            }))
            .send()
            .await?;

        let response = parse_porkbun_response::<ListRecordsResult>(
            response,
            &format!("list Porkbun domain ({}) records", domain.name),
        )
        .await?;

        match response.data.records {
            Some(records) => Ok(records),
            None => Err(anyhow!(
                "no Porkbun records found for domain ({})",
                domain.name
            )),
        }
    }

    async fn update_dns_record(
        &self,
        request: &HttpClient,
        id: &str,
        record_type: &str,
        domain: &Domain,
        address: &IpAddr,
    ) -> Result<()> {
        let response = request
            .post(format!("{}/dns/edit/{}/{}", self.api_url, domain.name, id))
            .json(&UpdateRecordBody {
                secretapikey: self.secret_api_key.expose_secret(),
                apikey: self.api_key.expose_secret(),
                record_type,
                content: address,
                ttl: domain.ttl,
                name: domain.subdomain.as_ref(),
                notes: &domain.notes,
            })
            .send()
            .await?;
        parse_porkbun_response::<EmptyResult>(
            response,
            &format!("update Porkbun domain ({}) record", domain.name),
        )
        .await?;
        Ok(())
    }

    async fn create_dns_record(
        &self,
        request: &HttpClient,
        record_type: &str,
        domain: &Domain,
        address: &IpAddr,
    ) -> Result<()> {
        let response = request
            .post(format!("{}/dns/create/{}", self.api_url, domain.name))
            .json(&CreateRecordBody {
                secretapikey: self.secret_api_key.expose_secret(),
                apikey: self.api_key.expose_secret(),
                record_type,
                content: address,
                ttl: domain.ttl,
                name: domain.subdomain.as_ref(),
                notes: &domain.notes,
            })
            .send()
            .await?;
        parse_porkbun_response::<EmptyResult>(
            response,
            &format!("create Porkbun domain ({}) record", domain.name),
        )
        .await?;
        Ok(())
    }
}

async fn parse_porkbun_response<T>(response: Response, action: &str) -> Result<PorkbunResponse<T>>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let body = response.text().await?;
    let parsed = serde_json::from_str::<PorkbunResponse<T>>(&body);

    if !status.is_success() {
        let detail = parsed
            .as_ref()
            .ok()
            .and_then(|response| response.message.as_deref())
            .filter(|message| !message.is_empty())
            .map(str::to_owned)
            .or_else(|| body_snippet(&body));
        let detail = detail.map_or_else(String::new, |detail| format!(": {detail}"));

        return Err(anyhow!("failed to {action}: HTTP {status}{detail}"));
    }

    let parsed = parsed
        .map_err(|error| anyhow!("failed to parse Porkbun response for {action}: {error}"))?;
    if parsed.status == ResponseStatus::Error {
        let detail = parsed
            .message
            .as_deref()
            .filter(|message| !message.is_empty())
            .unwrap_or("Porkbun API returned status=ERROR");
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
#[typetag::deserialize(name = "porkbun")]
impl Provider for Porkbun {
    fn validate_config(&self) -> Result<()> {
        if self.domains.is_empty() {
            return Err(anyhow!("no domains configured for Porkbun provider"));
        }
        Ok(())
    }

    async fn update(&self, update: IpUpdate, request: HttpClient) -> Result<bool> {
        for domain in &self.domains {
            for (version, addr) in update.iter() {
                let record_type = match version {
                    IpVersion::V4 => "A",
                    IpVersion::V6 => "AAAA",
                };
                if let Some(record) = self
                    .fetch_dns_records(&request, record_type, domain)
                    .await?
                    .first()
                {
                    self.update_dns_record(&request, &record.id, record_type, domain, &addr)
                        .await?;
                } else {
                    self.create_dns_record(&request, record_type, domain, &addr)
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
        matchers::{body_json, body_partial_json, method, path},
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
    async fn test_porkbun_bad_api_key() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Porkbun {
            api_key: "api_key".into(),
            secret_api_key: "secret_key".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                subdomain: None,
                ttl: 1,
                notes: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/A"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "status": "ERROR",
                "message": "Invalid API key. (001)"
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let error = provider.update(UPDATE_BOTH, http).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "failed to list Porkbun domain (example.com) records: HTTP 400 Bad Request: Invalid API key. (001)"
        );
    }

    #[tokio::test]
    async fn test_porkbun_rejects_non_success_http_status() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Porkbun {
            api_key: "api_key".into(),
            secret_api_key: "secret_key".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                subdomain: None,
                ttl: 600,
                notes: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/A"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "status": "SUCCESS",
                "records": [
                    {
                        "id": "106926659",
                        "name": "www.example.com",
                        "type": "A",
                        "content": "1.1.1.1",
                        "ttl": "600",
                        "prio": "0",
                        "notes": ""
                    }
                ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let error = provider.update(UPDATE_BOTH, http).await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(
                "failed to list Porkbun domain (example.com) records: HTTP 500 Internal Server Error"
            ),
            "{message}"
        );
        assert!(message.contains("SUCCESS"), "{message}");
    }

    #[tokio::test]
    async fn test_porkbun_no_domains() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Porkbun {
            api_key: "api_key".into(),
            secret_api_key: "secret_key".into(),
            domains: smallvec![],
            api_url: mock.uri(),
        };

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/A"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
                "records": [
                    {
                        "id": "106926659",
                        "name": "www.example.com",
                        "type": "A",
                        "content": "1.1.1.1",
                        "ttl": "600",
                        "prio": "0",
                        "notes": ""
                    }
                ]
            })))
            .expect(0)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_porkbun_update_both() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let v4_record_id = "89c0cbe7d4554cd29120ed30d8e6ef17";
        let v6_record_id = "25f1b0da807484b9668f812480f5c734";

        let provider = Porkbun {
            api_key: "api_key".into(),
            secret_api_key: "secret_key".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                subdomain: None,
                ttl: 600,
                notes: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/A"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
                "records": [
                    {
                        "id": v4_record_id,
                        "name": "www.example.com",
                        "type": "A",
                        "content": "192.168.1.1",
                        "ttl": "600",
                        "prio": "0",
                        "notes": ""
                    }
                ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/AAAA"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
                "records": [
                    {
                        "id": v6_record_id,
                        "name": "www.example.com",
                        "type": "AAAA",
                        "content": "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
                        "ttl": "600",
                        "prio": "0",
                        "notes": ""
                    }
                ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/dns/edit/example.com/{v4_record_id}")))
            .and(body_partial_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
                "type": "A",
                "content": UPDATE_BOTH.v4.unwrap().to_string(),
                "ttl": 600,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/dns/edit/example.com/{v6_record_id}")))
            .and(body_partial_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
                "type": "AAAA",
                "content": UPDATE_BOTH.v6.unwrap().to_string(),
                "ttl": 600,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_porkbun_update_ipv4() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let v4_record_id = "89c0cbe7d4554cd29120ed30d8e6ef17";

        let provider = Porkbun {
            api_key: "api_key".into(),
            secret_api_key: "secret_key".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                subdomain: None,
                ttl: 600,
                notes: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/A"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
                "records": [
                    {
                        "id": v4_record_id,
                        "name": "www.example.com",
                        "type": "A",
                        "content": "192.168.1.1",
                        "ttl": "600",
                        "prio": "0",
                        "notes": ""
                    }
                ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/dns/edit/example.com/{v4_record_id}")))
            .and(body_partial_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
                "type": "A",
                "content": UPDATE_V4.v4.unwrap().to_string(),
                "ttl": 600,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_V4, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_porkbun_update_v6() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let v6_record_id = "25f1b0da807484b9668f812480f5c734";

        let provider = Porkbun {
            api_key: "api_key".into(),
            secret_api_key: "secret_key".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                subdomain: None,
                ttl: 600,
                notes: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/AAAA"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
                "records": [
                    {
                        "id": v6_record_id,
                        "name": "www.example.com",
                        "type": "AAAA",
                        "content": "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
                        "ttl": "600",
                        "prio": "0",
                        "notes": ""
                    }
                ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/dns/edit/example.com/{v6_record_id}")))
            .and(body_partial_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
                "type": "AAAA",
                "content": UPDATE_V6.v6.unwrap().to_string(),
                "ttl": 600,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_V6, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_porkbun_create_both() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();

        let provider = Porkbun {
            api_key: "api_key".into(),
            secret_api_key: "secret_key".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                subdomain: None,
                ttl: 600,
                notes: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/A"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
                "records": []
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/AAAA"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
                "records": []
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/dns/create/example.com"))
            .and(body_partial_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
                "type": "A",
                "content": UPDATE_BOTH.v4.unwrap().to_string(),
                "ttl": 600,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/dns/create/example.com"))
            .and(body_partial_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
                "type": "AAAA",
                "content": UPDATE_BOTH.v6.unwrap().to_string(),
                "ttl": 600,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_porkbun_create_v4_update_v6() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let v6_record_id = "25f1b0da807484b9668f812480f5c734";

        let provider = Porkbun {
            api_key: "api_key".into(),
            secret_api_key: "secret_key".into(),
            domains: smallvec![Domain {
                name: "example.com".into(),
                subdomain: None,
                ttl: 600,
                notes: "Created by DDRS".into(),
            }],
            api_url: mock.uri(),
        };

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/A"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "SUCCESS",
            "records": []
                    })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/dns/retrieveByNameType/example.com/AAAA"))
            .and(body_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
                "records": [
                    {
                        "id": v6_record_id,
                        "name": "www.example.com",
                        "type": "AAAA",
                        "content": "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
                        "ttl": "600",
                        "prio": "0",
                        "notes": ""
                    }
                ]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/dns/create/example.com"))
            .and(body_partial_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
                "type": "A",
                "content": UPDATE_BOTH.v4.unwrap().to_string(),
                "ttl": 600,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
            })))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path(format!("/dns/edit/example.com/{v6_record_id}")))
            .and(body_partial_json(json!({
                "secretapikey": provider.secret_api_key.expose_secret(),
                "apikey": provider.api_key.expose_secret(),
                "type": "AAAA",
                "content": UPDATE_BOTH.v6.unwrap().to_string(),
                "ttl": 600,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "SUCCESS",
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }
}
