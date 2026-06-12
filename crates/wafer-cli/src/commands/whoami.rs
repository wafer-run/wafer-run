use anyhow::Result;

use crate::{credentials, registry_client};

pub async fn run(registry: Option<String>) -> Result<()> {
    let url = registry_client::resolve_registry(registry);
    let entry = credentials::require(&url)?;

    let me = registry_client::me(&url, &entry.token).await?;
    let suffix = if me.is_admin { "admin" } else { "not-admin" };
    println!("{} <{}>", me.email, suffix);
    Ok(())
}
