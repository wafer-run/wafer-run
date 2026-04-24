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
pub async fn ensure_ok(resp: reqwest::Response, op: &'static str) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(crate::registry_error::RegistryError::new(op, status, body).into())
}
