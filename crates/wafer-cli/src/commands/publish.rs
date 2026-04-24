use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::credentials;

#[derive(serde::Deserialize)]
struct CargoPackageRelease {
    org: String,
    name: String,
    version: String,
}

#[derive(serde::Deserialize)]
struct WaferTomlMin {
    package: CargoPackageRelease,
}

fn resolve_registry(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("WAFER_REGISTRY").ok())
        .unwrap_or_else(|| "https://wafer.run".to_string())
}

pub async fn run(file: Option<PathBuf>, registry: Option<String>, dry_run: bool) -> Result<()> {
    let cwd_toml =
        std::fs::read_to_string("wafer.toml").context("wafer.toml not found in current dir")?;
    let parsed: WaferTomlMin = toml::from_str(&cwd_toml).context("parse wafer.toml")?;

    let tarball_path = file.unwrap_or_else(|| {
        PathBuf::from(format!(
            "target/wafer/{}-{}.wafer",
            parsed.package.name, parsed.package.version
        ))
    });
    if !tarball_path.exists() {
        anyhow::bail!(
            "tarball not found at {}. Run `wafer build` first.",
            tarball_path.display()
        );
    }

    // TODO: stream via reqwest::Body::wrap_stream once the registry cap grows past a few hundred MiB.
    let bytes =
        std::fs::read(&tarball_path).with_context(|| format!("read {}", tarball_path.display()))?;

    if dry_run {
        println!(
            "\u{2714} Dry-run OK: {}/{}@{} ({} bytes)",
            parsed.package.org,
            parsed.package.name,
            parsed.package.version,
            bytes.len()
        );
        return Ok(());
    }

    let url = resolve_registry(registry);
    let cf = credentials::load().unwrap_or_default();
    let entry = credentials::resolve(&cf, &url)
        .ok_or_else(|| anyhow::anyhow!("No token for {url}. Run `wafer login` first."))?;

    let file_name = tarball_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "package.wafer".to_string());

    let form = reqwest::multipart::Form::new().part(
        "tarball",
        reqwest::multipart::Part::bytes(bytes).file_name(file_name),
    );

    let endpoint = format!("{}/registry/api/publish", url.trim_end_matches('/'));
    let resp = crate::registry_client::client_with_timeout(120)
        .post(&endpoint)
        .bearer_auth(&entry.token)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("POST {endpoint}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("publish failed: {status} {body}");
    }

    let json: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("decode publish response: {body}"))?;
    let package = json["package"].as_str().unwrap_or_default();
    let version = json["version"].as_str().unwrap_or_default();
    let download_url = json["download_url"].as_str().unwrap_or_default();
    let sha256 = json["sha256"].as_str().unwrap_or_default();

    println!("\u{2714} Published {package}@{version}");
    println!("  download: {}{}", url.trim_end_matches('/'), download_url);
    println!("  sha256:   {sha256}");
    Ok(())
}
