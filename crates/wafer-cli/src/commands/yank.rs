use anyhow::Result;

use crate::{commands::info::parse_target, credentials, registry_client};

pub enum YankOp {
    Yank,
    Unyank,
}

pub async fn run(
    target: String,
    reason: Option<String>,
    registry: Option<String>,
    op: YankOp,
) -> Result<()> {
    let (org, block, version) = parse_target(&target)?;
    let version = version.ok_or_else(|| anyhow::anyhow!("target must be org/block@version"))?;

    let url = registry_client::resolve_registry(registry);
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
    // `action` is a &'static str ("yank" or "unyank") so it satisfies the
    // ensure_ok op parameter.
    let _resp = crate::registry_client::ensure_ok(resp, action).await?;
    let past_tense = match op {
        YankOp::Yank => "Yanked",
        YankOp::Unyank => "Unyanked",
    };
    println!("\u{2714} {past_tense} {org}/{block}@{version}");
    Ok(())
}
