use std::net::IpAddr;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use compact_str::CompactString;
use reqwest_middleware::ClientWithMiddleware as HttpClient;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use smallvec::SmallVec;

use crate::client::{IpUpdate, IpVersion, Provider};

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
struct CreateRecordResult {
    status: ResponseStatus,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateRecordResult {
    status: ResponseStatus,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecordResult {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ListRecordsResult {
    status: ResponseStatus,
    message: Option<String>,
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
        let record_result = request
            .post(url)
            .json(&json!({
                "secretapikey": self.secret_api_key.expose_secret(),
                "apikey": self.api_key.expose_secret(),
            }))
            .send()
            .await?
            .json::<ListRecordsResult>()
            .await?;
        match record_result.status {
            ResponseStatus::Error => Err(anyhow!(
                "Failed to list Porkbun domain ({}) records, Error: {:?}",
                domain.name,
                record_result.message,
            )),
            ResponseStatus::Success => match record_result.records {
                Some(records) => Ok(records),
                None => Err(anyhow!(
                    "No Porkbun records found for domain ({})",
                    domain.name
                )),
            },
        }
    }

    async fn update_dns_record(
        &self,
        request: &HttpClient,
        id: &str,
        record_type: &str,
        domain: &Domain,
        address: &IpAddr,
    ) -> Result<UpdateRecordResult> {
        let updated = request
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
            .await?
            .json::<UpdateRecordResult>()
            .await?;
        match updated.status {
            ResponseStatus::Error => Err(anyhow!(
                "Failed to update Porkbun domain ({}) record, Error: {:?}",
                domain.name,
                updated.message,
            )),
            ResponseStatus::Success => Ok(updated),
        }
    }

    async fn create_dns_record(
        &self,
        request: &HttpClient,
        record_type: &str,
        domain: &Domain,
        address: &IpAddr,
    ) -> Result<CreateRecordResult> {
        let created = request
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
            .await?
            .json::<CreateRecordResult>()
            .await?;
        match created.status {
            ResponseStatus::Error => Err(anyhow!(
                "Failed to create Porkbun domain ({}) record, Error: {:?}",
                domain.name,
                created.message,
            )),
            ResponseStatus::Success => Ok(created),
        }
    }
}

#[async_trait]
#[typetag::deserialize(name = "porkbun")]
impl Provider for Porkbun {
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
            "Failed to list Porkbun domain (example.com) records, Error: Some(\"Invalid API key. (001)\")"
        );
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
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
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
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
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
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
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
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
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
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
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
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
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
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
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
