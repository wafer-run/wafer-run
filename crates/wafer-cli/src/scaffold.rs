use std::path::Path;

use anyhow::{bail, Context};

use crate::{block_name::parse_org_block, detect::Lang};

/// Create a new block project directory for the given `name` and `lang`.
///
/// `name` must be in `{org}/{block}` format. The directory created is named
/// after the block segment (the part after "/").
pub fn scaffold(name: &str, lang: Lang) -> anyhow::Result<()> {
    // Validate name format.
    let (org, block_name) = parse_org_block(name)?;
    let block_name = block_name.as_str();
    let dir = Path::new(block_name);

    if dir.exists() {
        bail!("Directory {dir:?} already exists");
    }

    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create directory {}", dir.display()))?;

    // Write wafer.toml — the single source of package metadata for
    // build/package/publish/install.
    write_wafer_toml(dir, &org, block_name)?;

    // Write the sample test fixture.
    write_test_fixture(dir, block_name)?;

    match lang {
        Lang::Rust => scaffold_rust(dir, name, block_name)?,
        Lang::Go => scaffold_go(dir, name, block_name)?,
    }

    println!("Created block project in ./{block_name}/");
    println!("  wafer.toml");
    println!("  tests/echo.json");
    match lang {
        Lang::Rust => {
            println!("  Cargo.toml");
            println!("  src/lib.rs");
        }
        Lang::Go => {
            println!("  go.mod");
            println!("  main.go");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Rust scaffold
// ---------------------------------------------------------------------------

fn scaffold_rust(dir: &Path, full_name: &str, block_name: &str) -> anyhow::Result<()> {
    // Standalone workspace + cdylib Cargo.toml.
    let cargo_toml = format!(
        r#"[workspace]
resolver = "2"
members = ["."]

[package]
name = "{block_name}"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
wafer-sdk = {{ git = "https://github.com/wafer-run/wafer-run.git", package = "wafer-sdk" }}
wafer-block = {{ git = "https://github.com/wafer-run/wafer-run.git", package = "wafer-block" }}
"#
    );
    write_file(dir, "Cargo.toml", &cargo_toml)?;

    // src/lib.rs skeleton.
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir)
        .with_context(|| format!("Failed to create {}", src_dir.display()))?;

    let lib_rs = format!(
        r#"use wafer_sdk::*;

struct {struct_name};

#[wafer_block(
    name = "{full_name}",
    interface = "handler@v1",
    summary = "A WAFER block"
)]
impl {struct_name} {{
    fn handle(msg: Message, _body: Vec<u8>) -> GuestResult {{
        GuestResult::respond(vec![])
    }}
}}
"#,
        struct_name = to_struct_name(block_name),
        full_name = full_name
    );
    write_file(&src_dir, "lib.rs", &lib_rs)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Go scaffold
// ---------------------------------------------------------------------------

fn scaffold_go(dir: &Path, full_name: &str, block_name: &str) -> anyhow::Result<()> {
    let struct_name = to_struct_name(block_name);

    let go_mod = format!(
        r#"module {block_name}

go 1.22

require github.com/wafer-run/wafer-sdk-go v0.1.0
"#
    );
    write_file(dir, "go.mod", &go_mod)?;

    let main_go = format!(
        r#"// {block_name} — WAFER block.
package main

import (
	"encoding/json"

	wafer "github.com/wafer-run/wafer-sdk-go"
)

// {struct_name} implements the wafer.Block interface.
type {struct_name} struct{{}}

// Info returns the block's identity metadata.
func (b *{struct_name}) Info() wafer.BlockInfo {{
	return wafer.BlockInfo{{
		Name:         "{full_name}",
		Version:      "0.1.0",
		Interface:    "handler@v1",
		Summary:      "A WAFER block",
		InstanceMode: wafer.InstanceModePerNode,
	}}
}}

// Handle processes an incoming message.
func (b *{struct_name}) Handle(msg wafer.Message) wafer.BlockResult {{
	// Echo the message kind back as a JSON response.
	data, _ := json.Marshal(map[string]any{{
		"kind": msg.Kind,
	}})
	return wafer.RespondBytes(data)
}}

func main() {{
	wafer.Register(&{struct_name}{{}})
}}
"#,
    );
    write_file(dir, "main.go", &main_go)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Write the `wafer.toml` template: `[package]` identity plus a one-line
/// summary. ABI is pinned at 1 (the only ABI major the runtime speaks).
fn write_wafer_toml(dir: &Path, org: &str, block_name: &str) -> anyhow::Result<()> {
    let body = format!(
        r#"[package]
org = "{org}"
name = "{block_name}"
version = "0.1.0"
abi = 1
summary = "A WAFER block: {org}/{block_name}"
"#
    );
    write_file(dir, "wafer.toml", &body)
}

fn write_test_fixture(dir: &Path, _block_name: &str) -> anyhow::Result<()> {
    let tests_dir = dir.join("tests");
    std::fs::create_dir_all(&tests_dir)
        .with_context(|| format!("Failed to create {}", tests_dir.display()))?;

    // The test runner parses fixtures as wafer_block::Message JSON.
    // `data` is Vec<u8> which serde serializes as an array of byte values.
    // "hello" in UTF-8 bytes: [104, 101, 108, 108, 111]
    let fixture = serde_json::json!({
        "kind": "run",
        "data": [104, 101, 108, 108, 111],
        "meta": []
    });
    let json = serde_json::to_string_pretty(&fixture).context("Failed to serialize fixture")?;
    write_file(&tests_dir, "echo.json", &(json + "\n"))?;

    Ok(())
}

fn write_file(dir: &Path, name: &str, content: &str) -> anyhow::Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, content).with_context(|| format!("Failed to write {}", path.display()))
}

/// Convert a kebab-case block name to PascalCase for a Rust struct.
///
/// Examples: "my-block" → "MyBlock", "auth" → "Auth"
fn to_struct_name(s: &str) -> String {
    s.split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect()
}
