use anyhow::{bail, Result};

use crate::credentials;
use crate::registry_client;

fn resolve_registry(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("WAFER_REGISTRY").ok())
        .unwrap_or_else(|| "https://wafer.run".to_string())
}

pub async fn run(registry: Option<String>) -> Result<()> {
    let url = resolve_registry(registry);
    let cf = credentials::load()?;
    let Some(entry) = credentials::resolve(&cf, &url) else {
        bail!("no token for {url} — run `wafer login`");
    };

    let me = registry_client::me(&url, &entry.token).await?;
    let suffix = if me.is_admin { "admin" } else { "not-admin" };
    println!("{} <{}>", me.email, suffix);
    Ok(())
}
