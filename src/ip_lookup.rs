use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{Context, Result, anyhow};
use local_ip_address::list_afinet_netifas;
use reqwest::Response;
use reqwest_middleware::ClientWithMiddleware as HttpClient;
use tokio::task::JoinSet;
use tracing::debug;
use url::Url;

use crate::ip::IpVersion;

const MAX_IP_LOOKUP_BODY_BYTES: usize = 256;

/// Fetches the IP address via HTTP lookup URLs.
pub async fn fetch_ip_http(
    request: &HttpClient,
    urls: &[Url],
    threshold: usize,
    version: IpVersion,
) -> Result<IpAddr> {
    let url_count = urls.len();
    let mut set = JoinSet::new();
    for url in urls {
        let request = request.clone();
        let url = url.clone();
        set.spawn(async move {
            let response = request
                .get(url.as_str())
                .send()
                .await
                .with_context(|| format!("HTTP request failed for {url}"))?;
            parse_ip_lookup_response(response, version, url.as_str()).await
        });
    }

    let mut votes = BTreeMap::new();
    let mut failures = 0;
    let mut failure_details = Vec::new();
    while let Some(result) = set.join_next().await {
        match result {
            Ok(Ok(ip)) => {
                let vote_count = {
                    let count = votes.entry(ip).or_insert(0);
                    *count += 1;
                    *count
                };
                if vote_count >= threshold {
                    let checked = votes.values().sum::<usize>() + failures;
                    debug!(
                        "IP lookup quorum reached for {version}: {ip} ({vote_count}/{checked} responses, {threshold}/{url_count} required)"
                    );
                    set.abort_all();
                    return Ok(ip);
                }
            }
            Ok(Err(error)) => {
                debug!("IP lookup failed for {version}: {error}");
                failure_details.push(error.to_string());
                failures += 1;
            }
            Err(error) => {
                debug!("IP lookup task failed for {version}: {error}");
                failure_details.push(error.to_string());
                failures += 1;
            }
        }
    }

    match resolve_ip_quorum(votes, failures, threshold) {
        IpQuorumResult::Reached { ip, votes, checked } => {
            debug!("IP lookup quorum reached for {version}: {ip} ({votes}/{checked})");
            Ok(ip)
        }
        IpQuorumResult::NotReached { votes, failures } => Err(anyhow!(
            "no HTTP IP lookup quorum reached for {version}: required {threshold} matching responses from {url_count} URLs, votes [{}], failures {failures}{}",
            format_ip_votes(&votes),
            format_failure_details(&failure_details)
        )),
    }
}

/// Fetches the IP address of a specific network interface.
pub fn fetch_ip_interface(interface_name: &str, version: IpVersion) -> Result<IpAddr> {
    let interfaces = list_afinet_netifas()?;
    for iface in interfaces {
        if iface.0 == interface_name {
            match version {
                IpVersion::V4 => {
                    if iface.1.is_ipv4() {
                        return Ok(iface.1);
                    }
                }
                IpVersion::V6 => {
                    if iface.1.is_ipv6() {
                        return Ok(iface.1);
                    }
                }
            }
        }
    }
    Err(anyhow!(
        "failed to find network interface: {interface_name}"
    ))
}

fn parse_ip_for_version(version: IpVersion, body: &str) -> Result<IpAddr> {
    let body = body.trim();
    match version {
        IpVersion::V4 => body
            .parse::<Ipv4Addr>()
            .map(IpAddr::V4)
            .context("expected IPv4 address"),
        IpVersion::V6 => body
            .parse::<Ipv6Addr>()
            .map(IpAddr::V6)
            .context("expected IPv6 address"),
    }
}

async fn parse_ip_lookup_response(
    response: Response,
    version: IpVersion,
    url: &str,
) -> Result<IpAddr> {
    let status = response.status();
    let body = read_ip_lookup_body(response).await;

    if !status.is_success() {
        let detail = match body {
            Ok(body) => response_body_detail(&body),
            Err(error) => format!(": failed to read response body: {error}"),
        };
        return Err(anyhow!("IP lookup {url} returned HTTP {status}{detail}"));
    }

    let body = body?;
    parse_ip_for_version(version, &body)
        .with_context(|| format!("failed to parse {version:?} IP lookup response from {url}"))
}

async fn read_ip_lookup_body(mut response: Response) -> Result<String> {
    if let Some(length) = response.content_length() {
        let max_length = u64::try_from(MAX_IP_LOOKUP_BODY_BYTES)?;
        if length > max_length {
            return Err(anyhow!(
                "IP lookup response body exceeded {MAX_IP_LOOKUP_BODY_BYTES} bytes"
            ));
        }
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_IP_LOOKUP_BODY_BYTES.saturating_sub(body.len()) {
            return Err(anyhow!(
                "IP lookup response body exceeded {MAX_IP_LOOKUP_BODY_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }

    String::from_utf8(body).context("IP lookup response body was not valid UTF-8")
}

fn response_body_detail(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        String::new()
    } else {
        format!(": {body}")
    }
}

#[derive(Debug, PartialEq, Eq)]
enum IpQuorumResult {
    Reached {
        ip: IpAddr,
        votes: usize,
        checked: usize,
    },
    NotReached {
        votes: BTreeMap<IpAddr, usize>,
        failures: usize,
    },
}

fn resolve_ip_quorum(
    votes: BTreeMap<IpAddr, usize>,
    failures: usize,
    threshold: usize,
) -> IpQuorumResult {
    let checked = votes.values().sum::<usize>() + failures;
    let Some((ip, vote_count)) = votes.iter().max_by_key(|(_ip, count)| *count) else {
        return IpQuorumResult::NotReached { votes, failures };
    };
    let ip = *ip;
    let vote_count = *vote_count;

    let tied = votes.values().filter(|&&count| count == vote_count).count() > 1;

    if vote_count >= threshold && !tied {
        IpQuorumResult::Reached {
            ip,
            votes: vote_count,
            checked,
        }
    } else {
        IpQuorumResult::NotReached { votes, failures }
    }
}

fn format_ip_votes(votes: &BTreeMap<IpAddr, usize>) -> String {
    if votes.is_empty() {
        return "none".to_string();
    }

    votes
        .iter()
        .map(|(ip, count)| format!("{ip}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_failure_details(failures: &[String]) -> String {
    if failures.is_empty() {
        String::new()
    } else {
        format!(", failure details [{}]", failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use reqwest::Client as InnerHttpClient;
    use reqwest_middleware::{ClientBuilder, ClientWithMiddleware as HttpClient};
    use url::Url;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    use crate::ip::IpVersion;

    use super::{IpQuorumResult, MAX_IP_LOOKUP_BODY_BYTES, fetch_ip_http, resolve_ip_quorum};

    fn request_client() -> HttpClient {
        ClientBuilder::new(InnerHttpClient::new()).build()
    }

    fn urls(urls: &[String]) -> Vec<Url> {
        urls.iter().map(|url| Url::parse(url).unwrap()).collect()
    }

    async fn ip_lookup_server(status: u16, body: String) -> MockServer {
        ip_lookup_server_with_template(ResponseTemplate::new(status).set_body_string(body)).await
    }

    async fn delayed_ip_lookup_server(status: u16, body: String, delay: Duration) -> MockServer {
        ip_lookup_server_with_template(
            ResponseTemplate::new(status)
                .set_body_string(body)
                .set_delay(delay),
        )
        .await
    }

    async fn ip_lookup_server_with_template(response: ResponseTemplate) -> MockServer {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(response)
            .mount(&mock)
            .await;
        mock
    }

    #[tokio::test]
    async fn fetch_ip_http_rejects_non_success_status() {
        let mock = ip_lookup_server(500, "192.0.2.1".to_string()).await;
        let url = mock.uri();
        let urls = urls(std::slice::from_ref(&url));
        let request = request_client();

        let error = fetch_ip_http(&request, &urls, 1, IpVersion::V4)
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("no HTTP IP lookup quorum reached for IPv4"),
            "error should identify the failed quorum: {message}"
        );
        assert!(
            message.contains(&url),
            "error should include URL: {message}"
        );
        assert!(
            message.contains("HTTP 500 Internal Server Error"),
            "error should include status: {message}"
        );
        assert!(
            message.contains("192.0.2.1"),
            "error should include bounded response detail: {message}"
        );
    }

    #[tokio::test]
    async fn fetch_ip_http_reaches_quorum_after_non_success_status() {
        let bad = ip_lookup_server(500, "192.0.2.1".to_string()).await;
        let first_good = ip_lookup_server(200, "192.0.2.2\n".to_string()).await;
        let second_good = ip_lookup_server(200, "192.0.2.2\n".to_string()).await;
        let urls = urls(&[bad.uri(), first_good.uri(), second_good.uri()]);
        let request = request_client();

        let ip = fetch_ip_http(&request, &urls, 2, IpVersion::V4)
            .await
            .unwrap();

        assert_eq!(ip, Ipv4Addr::new(192, 0, 2, 2));
    }

    #[tokio::test]
    async fn fetch_ip_http_rejects_oversized_body() {
        let mock = ip_lookup_server(200, "x".repeat(MAX_IP_LOOKUP_BODY_BYTES + 1)).await;
        let url = mock.uri();
        let urls = urls(std::slice::from_ref(&url));
        let request = request_client();

        let error = fetch_ip_http(&request, &urls, 1, IpVersion::V4)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("IP lookup response body exceeded 256 bytes")
        );
    }

    #[tokio::test]
    async fn fetch_ip_http_requires_matching_quorum() {
        let first = ip_lookup_server(200, "192.0.2.10\n".to_string()).await;
        let second = ip_lookup_server(200, "192.0.2.10\n".to_string()).await;
        let third = ip_lookup_server(200, "192.0.2.11\n".to_string()).await;
        let urls = urls(&[first.uri(), second.uri(), third.uri()]);
        let request = request_client();

        let ip = fetch_ip_http(&request, &urls, 2, IpVersion::V4)
            .await
            .unwrap();

        assert_eq!(ip, Ipv4Addr::new(192, 0, 2, 10));
    }

    #[tokio::test]
    async fn fetch_ip_http_returns_when_quorum_is_reached() {
        let first = ip_lookup_server(200, "192.0.2.10\n".to_string()).await;
        let second = ip_lookup_server(200, "192.0.2.10\n".to_string()).await;
        let slow =
            delayed_ip_lookup_server(200, "192.0.2.11\n".to_string(), Duration::from_secs(5)).await;
        let urls = urls(&[first.uri(), second.uri(), slow.uri()]);
        let request = request_client();

        let ip = tokio::time::timeout(
            Duration::from_millis(500),
            fetch_ip_http(&request, &urls, 2, IpVersion::V4),
        )
        .await
        .expect("HTTP lookup should return before the delayed response")
        .unwrap();

        assert_eq!(ip, Ipv4Addr::new(192, 0, 2, 10));
    }

    #[tokio::test]
    async fn fetch_ip_http_rejects_split_without_majority() {
        let first = ip_lookup_server(200, "192.0.2.10\n".to_string()).await;
        let second = ip_lookup_server(200, "192.0.2.10\n".to_string()).await;
        let third = ip_lookup_server(200, "192.0.2.11\n".to_string()).await;
        let fourth = ip_lookup_server(200, "192.0.2.11\n".to_string()).await;
        let urls = urls(&[first.uri(), second.uri(), third.uri(), fourth.uri()]);
        let request = request_client();

        let error = fetch_ip_http(&request, &urls, 3, IpVersion::V4)
            .await
            .unwrap_err();
        let message = error.to_string();

        assert!(
            message.contains("no HTTP IP lookup quorum reached for IPv4"),
            "{message}"
        );
        assert!(message.contains("192.0.2.10=2"), "{message}");
        assert!(message.contains("192.0.2.11=2"), "{message}");
    }

    #[test]
    fn ip_quorum_rejects_tied_leaders() {
        let votes = BTreeMap::from([
            (IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 2),
            (IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11)), 2),
        ]);

        assert_eq!(
            resolve_ip_quorum(votes.clone(), 0, 2),
            IpQuorumResult::NotReached { votes, failures: 0 }
        );
    }
}
