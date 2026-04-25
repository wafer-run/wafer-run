use anyhow::{bail, Result};

use crate::{credentials, registry_client};

pub async fn run(registry: Option<String>) -> Result<()> {
    let url = registry_client::resolve_registry(registry);
    let cf = credentials::load()?;
    let Some(entry) = credentials::resolve(&cf, &url) else {
        bail!("no token for {url} — run `wafer login`");
    };

    let me = registry_client::me(&url, &entry.token).await?;
    let suffix = if me.is_admin { "admin" } else { "not-admin" };
    println!("{} <{}>", me.email, suffix);
    Ok(())
}
