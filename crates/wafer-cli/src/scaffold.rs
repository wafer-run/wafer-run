use crate::detect::Lang;
use crate::manifest::Manifest;
use anyhow::{bail, Context};
use std::path::Path;

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
        Lang::TypeScript => scaffold_typescript(dir, name, block_name)?,
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
        Lang::TypeScript => {
            println!("  package.json");
            println!("  tsconfig.json");
            println!("  src/index.ts");
            println!("  runtime/Cargo.toml");
            println!("  runtime/src/lib.rs");
            println!("  runtime/bundle.js  (placeholder)");
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
// TypeScript scaffold
// ---------------------------------------------------------------------------

fn scaffold_typescript(dir: &Path, full_name: &str, block_name: &str) -> anyhow::Result<()> {
    let struct_name = to_struct_name(block_name);

    let package_json = format!(
        r#"{{
  "name": "{block_name}",
  "version": "0.1.0",
  "type": "module",
  "scripts": {{
    "build": "wafer build",
    "test": "wafer test"
  }},
  "dependencies": {{
    "@wafer-run/sdk": "^0.1.0"
  }},
  "devDependencies": {{
    "esbuild": "^0.25.0",
    "typescript": "^5.0.0"
  }}
}}
"#
    );
    write_file(dir, "package.json", &package_json)?;

    let tsconfig_json = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "declaration": true,
    "outDir": "dist"
  },
  "include": ["src"]
}
"#;
    write_file(dir, "tsconfig.json", tsconfig_json)?;

    // --- src/index.ts ---
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir)
        .with_context(|| format!("Failed to create {}", src_dir.display()))?;

    // Use struct_name as a comment-only label; the block variable name is camelCase.
    let var_name = {
        let mut s = struct_name;
        if let Some(first) = s.get_mut(0..1) {
            first.make_ascii_lowercase();
        }
        s
    };

    let index_ts = format!(
        r#"// {block_name} — WAFER block.
import {{ block, respond }} from "@wafer-run/sdk";
import type {{ Message, BlockResult }} from "@wafer-run/sdk";

const {var_name} = block({{
  name: "{full_name}",
  version: "0.1.0",
  interface: "handler@v1",
  summary: "A WAFER block",

  handle(msg: Message): BlockResult {{
    // Echo the message kind back as a JSON response.
    return respond({{ echo: true, kind: msg.kind }});
  }},
}});

export default {var_name};
"#
    );
    write_file(&src_dir, "index.ts", &index_ts)?;

    // --- runtime/Cargo.toml ---
    let runtime_dir = dir.join("runtime");
    std::fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("Failed to create {}", runtime_dir.display()))?;

    let runtime_cargo_toml = r#"[package]
name = "block-runtime"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
boa_engine = { version = "0.21", default-features = false }
serde_json = "1"

[workspace]
"#;
    write_file(&runtime_dir, "Cargo.toml", runtime_cargo_toml)?;

    // --- runtime/src/lib.rs ---
    let runtime_src_dir = runtime_dir.join("src");
    std::fs::create_dir_all(&runtime_src_dir)
        .with_context(|| format!("Failed to create {}", runtime_src_dir.display()))?;

    let runtime_lib_rs = RUNTIME_LIB_RS;
    write_file(&runtime_src_dir, "lib.rs", runtime_lib_rs)?;

    // --- runtime/bundle.js placeholder ---
    write_file(
        &runtime_dir,
        "bundle.js",
        "// Placeholder — replaced by esbuild during `wafer build`.\n",
    )?;

    Ok(())
}

/// The Rust source for the boa_engine-based WASM runtime.
///
/// This file is embedded as a string constant and written to
/// `runtime/src/lib.rs` during scaffolding.
const RUNTIME_LIB_RS: &str = r#"//! WAFER JS/TS block runtime — embeds boa_engine to run bundled JavaScript.
//!
//! The user's TypeScript is bundled by esbuild into `bundle.js` (IIFE format),
//! then `include_str!`-ed here. On first call, boa evaluates the bundle which
//! sets `__wafer_block_info` and `__wafer_handle_message` on globalThis.

use boa_engine::{Context, Source};
use std::cell::RefCell;

const BUNDLE_JS: &str = include_str!("../bundle.js");

/// Polyfill for TextEncoder that the SDK's `respond()` helper needs.
/// Boa does not provide Web APIs, so we shim the minimal subset used by
/// the SDK: `new TextEncoder().encode(string)` returning an array of bytes.
const TEXT_ENCODER_POLYFILL: &str = r"
if (typeof TextEncoder === 'undefined') {
    globalThis.TextEncoder = class TextEncoder {
        encode(str) {
            const arr = [];
            for (let i = 0; i < str.length; i++) {
                let c = str.charCodeAt(i);
                if (c < 0x80) {
                    arr.push(c);
                } else if (c < 0x800) {
                    arr.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
                } else if (c >= 0xd800 && c <= 0xdbff) {
                    const hi = c;
                    const lo = str.charCodeAt(++i);
                    const cp = ((hi - 0xd800) << 10) + (lo - 0xdc00) + 0x10000;
                    arr.push(
                        0xf0 | (cp >> 18),
                        0x80 | ((cp >> 12) & 0x3f),
                        0x80 | ((cp >> 6) & 0x3f),
                        0x80 | (cp & 0x3f)
                    );
                } else {
                    arr.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
                }
            }
            return new Uint8Array(arr);
        }
    };
}
";

thread_local! {
    static CTX: RefCell<Option<Context>> = RefCell::new(None);
}

fn with_context<F, R>(f: F) -> R
where
    F: FnOnce(&mut Context) -> R,
{
    CTX.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            let mut ctx = Context::default();
            // Install TextEncoder polyfill before evaluating user code.
            ctx.eval(Source::from_bytes(TEXT_ENCODER_POLYFILL))
                .expect("TextEncoder polyfill failed");
            // Evaluate the bundled JS (sets __wafer_block_info and __wafer_handle_message).
            ctx.eval(Source::from_bytes(BUNDLE_JS))
                .expect("JS bundle evaluation failed");
            *borrow = Some(ctx);
        }
        f(borrow.as_mut().unwrap())
    })
}

fn pack_ptr_len(ptr: u32, len: u32) -> i64 {
    ((ptr as i64) << 32) | (len as i64)
}

fn string_to_packed(s: String) -> i64 {
    let bytes = s.into_bytes();
    let ptr = bytes.as_ptr() as u32;
    let len = bytes.len() as u32;
    std::mem::forget(bytes);
    pack_ptr_len(ptr, len)
}

/// Allocate `size` bytes in WASM linear memory and return the pointer.
/// The runtime uses this to write message data before calling `__wafer_handle`.
#[no_mangle]
pub extern "C" fn __wafer_alloc(size: i32) -> i32 {
    let mut buf = Vec::<u8>::with_capacity(size as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr as i32
}

/// Return the block's metadata as a packed (ptr, len) i64 pointing to a JSON string.
#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    with_context(|ctx| {
        let result = ctx
            .eval(Source::from_bytes("JSON.stringify(__wafer_block_info)"))
            .expect("__wafer_block_info not set — did you call block()?");
        let json = result
            .as_string()
            .expect("__wafer_block_info did not produce a string")
            .to_std_string_escaped();
        string_to_packed(json)
    })
}

/// Handle an incoming message. The runtime writes JSON bytes at `msg_ptr..msg_ptr+msg_len`,
/// then calls this. Returns a packed (ptr, len) i64 pointing to the JSON result.
#[no_mangle]
pub extern "C" fn __wafer_handle(msg_ptr: i32, msg_len: i32) -> i64 {
    let msg_bytes =
        unsafe { std::slice::from_raw_parts(msg_ptr as *const u8, msg_len as usize) };
    let msg_json = std::str::from_utf8(msg_bytes).expect("invalid UTF-8 in message");

    with_context(|ctx| {
        // serde_json::to_string on a &str produces a properly escaped JSON string literal
        // (e.g. `"hello \"world\""` becomes `"\"hello \\\"world\\\"\""` in Rust, which
        // in JS evaluates to the original string). This avoids manual escaping.
        let js_string_literal =
            serde_json::to_string(msg_json).expect("failed to re-serialize message");
        let script = format!("__wafer_handle_message({})", js_string_literal);
        let result = ctx
            .eval(Source::from_bytes(&script))
            .expect("__wafer_handle_message failed");
        let json = result
            .as_string()
            .expect("__wafer_handle_message did not return a string")
            .to_std_string_escaped();
        string_to_packed(json)
    })
}

/// Handle a lifecycle event. Currently a no-op for JS blocks that don't define
/// a lifecycle handler — returns `"null"`.
#[no_mangle]
pub extern "C" fn __wafer_lifecycle(evt_ptr: i32, evt_len: i32) -> i64 {
    let evt_bytes =
        unsafe { std::slice::from_raw_parts(evt_ptr as *const u8, evt_len as usize) };
    let evt_json = std::str::from_utf8(evt_bytes).expect("invalid UTF-8 in lifecycle event");

    with_context(|ctx| {
        // Check if the lifecycle handler was registered.
        let check = ctx
            .eval(Source::from_bytes("typeof __wafer_lifecycle_handler"))
            .expect("typeof check failed");
        let type_str = check
            .as_string()
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default();

        if type_str == "function" {
            let js_string_literal =
                serde_json::to_string(evt_json).expect("failed to re-serialize event");
            let script = format!("__wafer_lifecycle_handler({})", js_string_literal);
            let result = ctx
                .eval(Source::from_bytes(&script))
                .expect("__wafer_lifecycle_handler failed");
            let json = result
                .as_string()
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_else(|| "null".to_string());
            string_to_packed(json)
        } else {
            string_to_packed("null".to_string())
        }
    })
}
"#;

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
