use anyhow::Result;

use crate::credentials;

fn resolve_registry(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("WAFER_REGISTRY").ok())
        .unwrap_or_else(|| "https://wafer.run".to_string())
}

pub async fn run(registry: Option<String>) -> Result<()> {
    let url = resolve_registry(registry);
    let mut cf = credentials::load()?;

    let mut removed = false;

    if let Some(entry) = &cf.default {
        if entry.registry == url {
            cf.default = None;
            removed = true;
        }
    }

    let before = cf.registries.len();
    cf.registries.retain(|_, e| e.registry != url);
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
