use anyhow::{Result, anyhow};
use async_trait::async_trait;
use compact_str::CompactString;
use reqwest::Response;
use reqwest_middleware::ClientWithMiddleware as HttpClient;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use smallvec::SmallVec;

use crate::client::{IpUpdate, Provider};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DuckDns {
    token: SecretString,
    domains: SmallVec<[CompactString; 2]>,
    #[serde(default = "default_api_url")]
    api_url: String,
}

fn default_api_url() -> String {
    "https://www.duckdns.org".to_string()
}

impl DuckDns {
    async fn update_domains(&self, update: &IpUpdate, request: &HttpClient) -> Result<bool> {
        if update.v4.is_none() && update.v6.is_none() {
            return Err(anyhow!("no IP addresses supplied for Duck DNS update"));
        }

        let mut params = vec![
            ("domains", self.domains_param()),
            ("token", self.token.expose_secret().to_owned()),
            ("verbose", "true".to_string()),
        ];
        if let Some(ip) = update.v4 {
            params.push(("ip", ip.to_string()));
        }
        if let Some(ip) = update.v6 {
            params.push(("ipv6", ip.to_string()));
        }

        let response = request.get(self.update_url()).query(&params).send().await?;

        parse_update_response(response).await
    }

    fn update_url(&self) -> String {
        format!("{}/update", self.api_url.trim_end_matches('/'))
    }

    fn domains_param(&self) -> String {
        let mut domains = String::new();
        for domain in &self.domains {
            if !domains.is_empty() {
                domains.push(',');
            }
            domains.push_str(domain.as_str());
        }
        domains
    }
}

#[async_trait]
#[typetag::deserialize(name = "duckdns")]
impl Provider for DuckDns {
    fn validate_config(&self) -> Result<()> {
        if self.token.expose_secret().trim().is_empty() {
            return Err(anyhow!("Duck DNS token must not be empty"));
        }
        if self.domains.is_empty() {
            return Err(anyhow!("no domains configured for Duck DNS provider"));
        }
        for domain in &self.domains {
            if domain.trim().is_empty() {
                return Err(anyhow!("Duck DNS domains must not be empty"));
            }
            if domain.contains(',') {
                return Err(anyhow!("Duck DNS domains must not contain commas"));
            }
        }
        Ok(())
    }

    async fn update(&self, update: IpUpdate, request: HttpClient) -> Result<bool> {
        self.validate_config()?;
        self.update_domains(&update, &request).await
    }
}

async fn parse_update_response(response: Response) -> Result<bool> {
    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        let detail = body_snippet(&body).map_or_else(String::new, |body| format!(": {body}"));
        return Err(anyhow!(
            "failed to update Duck DNS domains: HTTP {status}{detail}"
        ));
    }

    parse_update_response_body(&body)
}

fn parse_update_response_body(body: &str) -> Result<bool> {
    let mut lines = body.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(first) = lines.next() else {
        return Err(anyhow!("empty Duck DNS response"));
    };

    match first {
        "OK" => {}
        "KO" => return Err(anyhow!("Duck DNS update rejected request")),
        status => return Err(anyhow!("unexpected Duck DNS response: {status}")),
    }

    for line in lines {
        match line {
            "UPDATED" => return Ok(true),
            "NOCHANGE" => return Ok(false),
            _ => {}
        }
    }

    Ok(true)
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

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use reqwest::Client as InnerHttpClient;
    use reqwest_middleware::ClientBuilder;
    use smallvec::smallvec;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
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
    const EMPTY_UPDATE: IpUpdate = IpUpdate { v4: None, v6: None };

    fn provider(api_url: String) -> DuckDns {
        DuckDns {
            token: "token".into(),
            domains: smallvec!["example".into(), "second".into()],
            api_url,
        }
    }

    #[test]
    fn test_duckdns_deserializes_from_config() {
        let config = toml::from_str::<crate::config::Config>(
            r#"
[[providers]]
type = "duckdns"
token = "token"
domains = ["example"]
"#,
        )
        .unwrap();

        assert_eq!(config.providers.len(), 1);
        config.providers[0].validate_config().unwrap();
    }

    #[tokio::test]
    async fn test_duckdns_bad_token() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let provider = provider(mock.uri());

        Mock::given(method("GET"))
            .and(path("/update"))
            .and(query_param("domains", "example,second"))
            .and(query_param("token", provider.token.expose_secret()))
            .and(query_param("verbose", "true"))
            .and(query_param("ip", Ipv4Addr::LOCALHOST.to_string()))
            .and(query_param("ipv6", Ipv6Addr::LOCALHOST.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_string("KO"))
            .expect(1)
            .mount(&mock)
            .await;

        let error = provider.update(UPDATE_BOTH, http).await.unwrap_err();
        assert_eq!(error.to_string(), "Duck DNS update rejected request");
    }

    #[tokio::test]
    async fn test_duckdns_rejects_non_success_http_status() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let provider = provider(mock.uri());

        Mock::given(method("GET"))
            .and(path("/update"))
            .respond_with(ResponseTemplate::new(500).set_body_string(format!(
                "OK\n{}\n{}\nUPDATED",
                Ipv4Addr::LOCALHOST,
                Ipv6Addr::LOCALHOST
            )))
            .expect(1)
            .mount(&mock)
            .await;

        let error = provider.update(UPDATE_BOTH, http).await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("failed to update Duck DNS domains: HTTP 500 Internal Server Error"),
            "{message}"
        );
        assert!(message.contains("OK"), "{message}");
    }

    #[tokio::test]
    async fn test_duckdns_update_both() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let provider = provider(mock.uri());

        Mock::given(method("GET"))
            .and(path("/update"))
            .and(query_param("domains", "example,second"))
            .and(query_param("token", provider.token.expose_secret()))
            .and(query_param("verbose", "true"))
            .and(query_param("ip", Ipv4Addr::LOCALHOST.to_string()))
            .and(query_param("ipv6", Ipv6Addr::LOCALHOST.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "OK\n{}\n{}\nUPDATED",
                Ipv4Addr::LOCALHOST,
                Ipv6Addr::LOCALHOST
            )))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_duckdns_update_ipv4() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let provider = provider(mock.uri());

        Mock::given(method("GET"))
            .and(path("/update"))
            .and(query_param("domains", "example,second"))
            .and(query_param("token", provider.token.expose_secret()))
            .and(query_param("verbose", "true"))
            .and(query_param("ip", Ipv4Addr::LOCALHOST.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("OK\n{}\n\nUPDATED", Ipv4Addr::LOCALHOST)),
            )
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_V4, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_duckdns_update_ipv6() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let provider = provider(mock.uri());

        Mock::given(method("GET"))
            .and(path("/update"))
            .and(query_param("domains", "example,second"))
            .and(query_param("token", provider.token.expose_secret()))
            .and(query_param("verbose", "true"))
            .and(query_param("ipv6", Ipv6Addr::LOCALHOST.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!("OK\n\n{}\nUPDATED", Ipv6Addr::LOCALHOST)),
            )
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_V6, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_duckdns_no_change() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let provider = provider(mock.uri());

        Mock::given(method("GET"))
            .and(path("/update"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "OK\n{}\n{}\nNOCHANGE",
                Ipv4Addr::LOCALHOST,
                Ipv6Addr::LOCALHOST
            )))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_duckdns_simple_ok_response() {
        let mock = MockServer::start().await;
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let provider = provider(mock.uri());

        Mock::given(method("GET"))
            .and(path("/update"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .expect(1)
            .mount(&mock)
            .await;

        let result = provider.update(UPDATE_BOTH, http).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_duckdns_rejects_empty_update() {
        let http: HttpClient = ClientBuilder::new(InnerHttpClient::new()).build();
        let provider = provider(default_api_url());

        let error = provider.update(EMPTY_UPDATE, http).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "no IP addresses supplied for Duck DNS update"
        );
    }

    #[test]
    fn test_duckdns_validates_config() {
        let mut provider = provider(default_api_url());

        provider.token = "".into();
        assert_eq!(
            provider.validate_config().unwrap_err().to_string(),
            "Duck DNS token must not be empty"
        );

        provider.token = "token".into();
        provider.domains = smallvec![];
        assert_eq!(
            provider.validate_config().unwrap_err().to_string(),
            "no domains configured for Duck DNS provider"
        );

        provider.domains = smallvec!["".into()];
        assert_eq!(
            provider.validate_config().unwrap_err().to_string(),
            "Duck DNS domains must not be empty"
        );

        provider.domains = smallvec!["example,second".into()];
        assert_eq!(
            provider.validate_config().unwrap_err().to_string(),
            "Duck DNS domains must not contain commas"
        );
    }
}
