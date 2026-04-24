use anyhow::Result;

use crate::credentials;

pub enum YankOp {
    Yank,
    Unyank,
}

fn resolve_registry(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("WAFER_REGISTRY").ok())
        .unwrap_or_else(|| "https://wafer.run".to_string())
}

pub async fn run(
    target: String,
    reason: Option<String>,
    registry: Option<String>,
    op: YankOp,
) -> Result<()> {
    let (org, rest) = target
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("target must be org/block@version"))?;
    let (block, version) = rest
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("target must be org/block@version"))?;

    let url = resolve_registry(registry);
    let cf = credentials::load().unwrap_or_default();
    let entry = credentials::resolve(&cf, &url)
        .ok_or_else(|| anyhow::anyhow!("No token for {url}. Run `wafer login` first."))?;

    let action = match op {
        YankOp::Yank => "yank",
        YankOp::Unyank => "unyank",
    };
    let endpoint = format!(
        "{}/registry/api/packages/{}/{}/{}/{}",
        url.trim_end_matches('/'),
        org,
        block,
        version,
        action
    );

    let mut req = crate::registry_client::client()
        .post(&endpoint)
        .bearer_auth(&entry.token);
    if let (YankOp::Yank, Some(r)) = (&op, &reason) {
        req = req.json(&serde_json::json!({ "reason": r }));
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("{action} failed: {status}");
    }
    let past_tense = match op {
        YankOp::Yank => "Yanked",
        YankOp::Unyank => "Unyanked",
    };
    println!("\u{2714} {past_tense} {org}/{block}@{version}");
    Ok(())
}
