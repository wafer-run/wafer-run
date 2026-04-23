use anyhow::{bail, Context, Result};
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

pub async fn exchange_code(registry: &str, code: &str) -> Result<ExchangeResponse> {
    let url = format!(
        "{}/registry/api/cli-login/exchange",
        registry.trim_end_matches('/')
    );
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("exchange failed: {status} {body}");
    }
    let parsed: ExchangeResponse = resp
        .json()
        .await
        .with_context(|| format!("decode exchange response from {url}"))?;
    Ok(parsed)
}

pub async fn me(registry: &str, token: &str) -> Result<MeResponse> {
    let url = format!("{}/registry/api/me", registry.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("me failed: {status} {body}");
    }
    let parsed: MeResponse = resp
        .json()
        .await
        .with_context(|| format!("decode me response from {url}"))?;
    Ok(parsed)
}
