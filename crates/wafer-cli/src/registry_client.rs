use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct ExchangeUser {
    pub email: String,
}

#[derive(Deserialize, Debug)]
pub struct ExchangeResponse {
    pub token: String,
    pub user: ExchangeUser,
}

#[derive(Deserialize, Debug)]
pub struct MeResponse {
    pub email: String,
    pub is_admin: bool,
}

// ---- Public read-only DTOs (no auth required) ----------------------------

/// One row in the `/registry/search` response envelope and (future) the
/// paginated listing endpoint. Mirrors `site::blocks::registry::models::PackageSummary`.
#[derive(Deserialize, Debug, Clone, serde::Serialize)]
pub struct PackageSummary {
    pub org: String,
    pub name: String,
    pub summary: Option<String>,
    pub latest: Option<String>,
}

/// Envelope returned by `GET /registry/search?q=…&page=…`.
#[derive(Deserialize, Debug)]
pub struct SearchResponse {
    pub packages: Vec<PackageSummary>,
    // Accepted but currently unused by the CLI. Present so downstream
    // pagination work doesn't need to re-shape the struct.
    #[allow(dead_code)]
    pub total: i64,
    #[allow(dead_code)]
    pub query: String,
    #[allow(dead_code)]
    pub page: i64,
    #[allow(dead_code)]
    pub page_size: i64,
}

/// Per-version summary inside `PackageDetail.versions`.
/// Mirrors `site::blocks::registry::models::VersionSummary`.
/// `published_at` is a unix epoch (seconds).
#[derive(Deserialize, Debug, Clone, serde::Serialize)]
pub struct VersionSummary {
    pub version: String,
    pub abi: i64,
    pub sha256: String,
    pub size_bytes: i64,
    pub license: Option<String>,
    pub yanked: i64,
    pub published_at: i64,
}

/// Response body of `GET /registry/api/packages/{org}/{block}`.
#[derive(Deserialize, Debug, serde::Serialize)]
pub struct PackageDetail {
    pub org: String,
    pub name: String,
    pub summary: Option<String>,
    pub versions: Vec<VersionSummary>,
}

/// Response body of `GET /registry/api/packages/{org}/{block}/{version}`.
/// `org_name`/`pkg_name` match the server's column names; `published_at`
/// is a unix epoch (seconds).
#[derive(Deserialize, Debug, serde::Serialize)]
pub struct VersionDetail {
    pub org_name: String,
    pub pkg_name: String,
    pub version: String,
    pub abi: i64,
    pub sha256: String,
    pub storage_key: String,
    pub size_bytes: i64,
    pub license: Option<String>,
    pub readme_md: Option<String>,
    pub dependencies: Option<String>,
    pub capabilities: Option<String>,
    pub yanked: i64,
    pub yanked_reason: Option<String>,
    pub published_at: i64,
}

// ---- Fetchers ------------------------------------------------------------

/// `GET /registry/search?q={query}`. Callers are responsible for rejecting
/// empty queries — this function does not inspect `query`.
pub async fn search(registry: &str, query: &str) -> Result<SearchResponse> {
    let url = format!(
        "{}/registry/search?q={}",
        registry.trim_end_matches('/'),
        urlencoding_encode(query),
    );
    let resp = client()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let resp = ensure_ok(resp, "search").await?;
    resp.json()
        .await
        .with_context(|| format!("decode search response from {url}"))
}

/// `GET /registry/api/packages/{org}/{block}`.
pub async fn get_package(registry: &str, org: &str, block: &str) -> Result<PackageDetail> {
    let url = format!(
        "{}/registry/api/packages/{}/{}",
        registry.trim_end_matches('/'),
        urlencoding_encode(org),
        urlencoding_encode(block),
    );
    let resp = client()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let resp = ensure_ok(resp, "info").await?;
    resp.json()
        .await
        .with_context(|| format!("decode package detail from {url}"))
}

/// `GET /registry/api/packages/{org}/{block}/{version}`.
pub async fn get_version(
    registry: &str,
    org: &str,
    block: &str,
    version: &str,
) -> Result<VersionDetail> {
    let url = format!(
        "{}/registry/api/packages/{}/{}/{}",
        registry.trim_end_matches('/'),
        urlencoding_encode(org),
        urlencoding_encode(block),
        urlencoding_encode(version),
    );
    let resp = client()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let resp = ensure_ok(resp, "info").await?;
    resp.json()
        .await
        .with_context(|| format!("decode version detail from {url}"))
}

/// Minimal percent-encoder for the subset we care about: path segments and
/// query values. Encodes everything outside unreserved + `-`, `.`, `_`, `~`.
/// Keeps the wafer-cli crate free of an extra `url`/`percent-encoding` dep.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Build a reqwest client with a default 60-second timeout and user-agent.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("wafer-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("build reqwest client")
}

/// Build a reqwest client with a custom timeout and user-agent.
pub fn client_with_timeout(secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(secs))
        .user_agent(concat!("wafer-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("build reqwest client")
}

pub async fn exchange_code(registry: &str, code: &str) -> Result<ExchangeResponse> {
    let url = format!(
        "{}/registry/api/cli-login/exchange",
        registry.trim_end_matches('/')
    );
    let resp = client()
        .post(&url)
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let resp = ensure_ok(resp, "login").await?;
    let parsed: ExchangeResponse = resp
        .json()
        .await
        .with_context(|| format!("decode exchange response from {url}"))?;
    Ok(parsed)
}

pub async fn me(registry: &str, token: &str) -> Result<MeResponse> {
    let url = format!("{}/registry/api/me", registry.trim_end_matches('/'));
    let resp = client()
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let resp = ensure_ok(resp, "whoami").await?;
    let parsed: MeResponse = resp
        .json()
        .await
        .with_context(|| format!("decode me response from {url}"))?;
    Ok(parsed)
}

/// Convert a reqwest `Response` into an error if the status is not 2xx.
/// The body is consumed so callers should only call this before reading JSON.
pub(crate) async fn ensure_ok(
    resp: reqwest::Response,
    op: &'static str,
) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(crate::registry_error::RegistryError::new(op, status, body).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_summary_deserialises_from_wire() {
        let wire = r#"{
            "org": "acme",
            "name": "widget",
            "summary": "A widget.",
            "latest": "0.3.1"
        }"#;
        let got: PackageSummary = serde_json::from_str(wire).unwrap();
        assert_eq!(got.org, "acme");
        assert_eq!(got.name, "widget");
        assert_eq!(got.summary.as_deref(), Some("A widget."));
        assert_eq!(got.latest.as_deref(), Some("0.3.1"));
    }

    #[test]
    fn package_summary_null_summary_latest_ok() {
        let wire = r#"{"org":"a","name":"b","summary":null,"latest":null}"#;
        let got: PackageSummary = serde_json::from_str(wire).unwrap();
        assert!(got.summary.is_none() && got.latest.is_none());
    }

    #[test]
    fn search_response_envelope_deserialises() {
        let wire = r#"{
            "packages": [
                {"org":"a","name":"b","summary":null,"latest":"1.0.0"}
            ],
            "total": 1,
            "query": "b",
            "page": 1,
            "page_size": 20
        }"#;
        let got: SearchResponse = serde_json::from_str(wire).unwrap();
        assert_eq!(got.packages.len(), 1);
        assert_eq!(got.packages[0].name, "b");
    }

    #[test]
    fn version_summary_deserialises_with_epoch_published_at() {
        let wire = r#"{
            "version": "0.3.1",
            "abi": 1,
            "sha256": "deadbeef",
            "size_bytes": 123456,
            "license": "Apache-2.0",
            "yanked": 0,
            "published_at": 1744416000
        }"#;
        let got: VersionSummary = serde_json::from_str(wire).unwrap();
        assert_eq!(got.version, "0.3.1");
        assert_eq!(got.abi, 1);
        assert_eq!(got.size_bytes, 123456);
        assert_eq!(got.yanked, 0);
        assert_eq!(got.published_at, 1_744_416_000);
    }

    #[test]
    fn package_detail_carries_versions() {
        let wire = r#"{
            "org": "acme",
            "name": "widget",
            "summary": "A widget.",
            "versions": [
                {"version":"0.2.0","abi":1,"sha256":"a","size_bytes":1,"license":null,"yanked":1,"published_at":1},
                {"version":"0.3.1","abi":1,"sha256":"b","size_bytes":2,"license":null,"yanked":0,"published_at":2}
            ]
        }"#;
        let got: PackageDetail = serde_json::from_str(wire).unwrap();
        assert_eq!(got.versions.len(), 2);
        assert_eq!(got.versions[1].yanked, 0);
    }

    #[test]
    fn version_detail_carries_storage_key_and_yanked_reason() {
        let wire = r#"{
            "org_name": "acme",
            "pkg_name": "widget",
            "version": "0.3.1",
            "abi": 1,
            "sha256": "abc",
            "storage_key": "storage/abc.wafer",
            "size_bytes": 120300,
            "license": "Apache-2.0",
            "readme_md": null,
            "dependencies": null,
            "capabilities": null,
            "yanked": 1,
            "yanked_reason": "security issue",
            "published_at": 1744416000
        }"#;
        let got: VersionDetail = serde_json::from_str(wire).unwrap();
        assert_eq!(got.org_name, "acme");
        assert_eq!(got.pkg_name, "widget");
        assert_eq!(got.yanked, 1);
        assert_eq!(got.yanked_reason.as_deref(), Some("security issue"));
    }

    #[test]
    fn urlencoding_encode_handles_space_and_slash() {
        assert_eq!(super::urlencoding_encode("a b/c"), "a%20b%2Fc");
        assert_eq!(super::urlencoding_encode("acme/widget"), "acme%2Fwidget");
        assert_eq!(super::urlencoding_encode("plain"), "plain");
    }
}
