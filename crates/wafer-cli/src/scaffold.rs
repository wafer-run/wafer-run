use std::path::Path;

use anyhow::{bail, Context};

use crate::{detect::Lang, manifest::Manifest};

/// Create a new block project directory for the given `name` and `lang`.
///
/// `name` must be in `{org}/{block}` format. The directory created is named
/// after the block segment (the part after "/").
pub fn scaffold(name: &str, lang: Lang) -> anyhow::Result<()> {
    // Validate name format.
    let parts: Vec<&str> = name.splitn(3, '/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        bail!(
            "Invalid block name {name:?}: must be in {{org}}/{{block}} format (exactly one \"/\")"
        );
    }
    let block_name = parts[1];
    let dir = Path::new(block_name);

    if dir.exists() {
        bail!("Directory {dir:?} already exists");
    }

    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create directory {}", dir.display()))?;

    // Write manifest.json.
    Manifest::write_template(dir, name)?;

    // Write the sample test fixture.
    write_test_fixture(dir, block_name)?;

    match lang {
        Lang::Rust => scaffold_rust(dir, name, block_name)?,
        Lang::Go => scaffold_go(dir, name, block_name)?,
        Lang::TypeScript => {
            bail!("TypeScript blocks are no longer supported. Please use Rust or Go.")
        }
    }

    println!("Created block project in ./{block_name}/");
    println!("  manifest.json");
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
        Lang::TypeScript => unreachable!("rejected above"),
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

require github.com/suppers-ai/wafer-sdk-go v0.1.0
"#
    );
    write_file(dir, "go.mod", &go_mod)?;

    let main_go = format!(
        r#"// {block_name} — WAFER block.
package main

import (
	"encoding/json"

	wafer "github.com/suppers-ai/wafer-sdk-go"
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
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}
