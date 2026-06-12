use anyhow::Result;

use crate::{credentials, registry_client};

pub async fn run(registry: Option<String>) -> Result<()> {
    let url = registry_client::resolve_registry(registry);
    let mut cf = credentials::load()?;

    let mut removed = false;

    if let Some(entry) = &cf.default {
        if entry.registry == url.as_str() {
            cf.default = None;
            removed = true;
        }
    }

    let before = cf.registries.len();
    cf.registries.retain(|_, e| e.registry != url.as_str());
    if cf.registries.len() != before {
        removed = true;
    }

    if removed {
        credentials::save(&cf)?;
        println!("\u{2714} Logged out of {url}");
    } else {
        println!("No credentials for {url}");
    }
    Ok(())
}
