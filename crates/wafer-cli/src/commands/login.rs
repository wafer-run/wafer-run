use anyhow::{Context, Result};

use crate::{
    credentials::{self, Entry},
    registry_client,
};

fn read_code() -> Result<String> {
    // rpassword falls back to stdin on non-TTY. If that fails (some builds),
    // drop to a plain stdin read so piped input still works.
    match rpassword::prompt_password("Paste code: ") {
        Ok(s) => Ok(s.trim().to_string()),
        Err(_) => {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            let mut line = String::new();
            stdin
                .lock()
                .read_line(&mut line)
                .context("read code from stdin")?;
            Ok(line.trim().to_string())
        }
    }
}

pub async fn run(registry: Option<String>) -> Result<()> {
    let url = registry_client::resolve_registry(registry);
    let login_page = format!("{}/registry/cli-login", url.trim_end_matches('/'));

    // Best-effort browser open; ignore errors (e.g. headless / CI).
    let _ = webbrowser::open(&login_page);
    println!("Opened {login_page} — complete the flow, then paste the code below.");

    let code = read_code()?;
    if code.is_empty() {
        anyhow::bail!("no code provided");
    }

    let resp = registry_client::exchange_code(&url, &code).await?;

    let mut cf = credentials::load()?;
    credentials::upsert(
        &mut cf,
        None,
        Entry {
            registry: url.clone(),
            token: resp.token,
        },
    );
    credentials::save(&cf)?;

    println!("\u{2714} Logged in as {}", resp.user.email);
    Ok(())
}
