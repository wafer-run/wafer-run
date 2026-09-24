use anyhow::Result;

use crate::{credentials, registry_client};

pub async fn run(registry: Option<String>) -> Result<()> {
    let url = registry_client::resolve_registry(registry);
    let mut cf = credentials::load()?;

    if cf.remove(&url) {
        credentials::save(&cf)?;
        println!("\u{2714} Logged out of {url}");
    } else {
        println!("No credentials for {url}");
    }
    Ok(())
}
