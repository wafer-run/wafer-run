use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{credentials, registry_client, wafer_toml::WaferToml};

/// Body of a successful `POST /registry/api/publish` response.
///
/// All fields are required: a 2xx with a missing/invalid field is a malformed
/// registry response and is surfaced as a decode error rather than silently
/// rendered as empty strings.
#[derive(serde::Deserialize)]
struct PublishResponse {
    package: String,
    version: String,
    download_url: String,
    sha256: String,
}

pub async fn run(file: Option<PathBuf>, registry: Option<String>, dry_run: bool) -> Result<()> {
    let pkg = WaferToml::read(Path::new("wafer.toml"))
        .context("wafer.toml not found in current dir")?
        .package()?;

    let tarball_path = file
        .unwrap_or_else(|| crate::package::tarball_path(Path::new("."), &pkg.name, &pkg.version));
    if !tarball_path.exists() {
        anyhow::bail!(
            "tarball not found at {}. Run `wafer package` first.",
            tarball_path.display()
        );
    }

    // TODO(#104): stream via reqwest::Body::wrap_stream once the registry cap grows past a few hundred MiB.
    let bytes =
        std::fs::read(&tarball_path).with_context(|| format!("read {}", tarball_path.display()))?;

    if dry_run {
        println!(
            "\u{2714} Dry-run OK: {}/{}@{} ({} bytes)",
            pkg.org,
            pkg.name,
            pkg.version,
            bytes.len()
        );
        return Ok(());
    }

    let url = registry_client::resolve_registry(registry);
    let entry = credentials::require(&url)?;

    let file_name = tarball_path.file_name().map_or_else(
        || "package.wafer".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );

    let form = reqwest::multipart::Form::new().part(
        "tarball",
        reqwest::multipart::Part::bytes(bytes).file_name(file_name),
    );

    let endpoint = url.join("/registry/api/publish");
    let resp = crate::registry_client::client_with_timeout(120)
        .post(&endpoint)
        .bearer_auth(&entry.token)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("POST {endpoint}"))?;

    let resp = crate::registry_client::ensure_ok(resp, "publish").await?;
    let body = resp
        .text()
        .await
        .with_context(|| format!("read publish response from {endpoint}"))?;

    let parsed: PublishResponse =
        serde_json::from_str(&body).with_context(|| format!("decode publish response: {body}"))?;

    println!("\u{2714} Published {}@{}", parsed.package, parsed.version);
    println!("  download: {}{}", url, parsed.download_url);
    println!("  sha256:   {}", parsed.sha256);
    Ok(())
}
