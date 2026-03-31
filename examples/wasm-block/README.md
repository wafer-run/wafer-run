# Example: WASM Block (network-request)

A minimal WASM block that demonstrates how to build a third-party plugin for wafer-run.

This block uses the **network** block to fetch external URLs and returns processed results.

## Routes

- `GET /b/network-example/fetch?url=<url>` - Fetch a URL, return status/headers/body preview
- `GET /b/network-example/json?url=<url>` - Fetch a JSON API, return parsed data

## Build

```sh
cargo build --target wasm32-wasip1 --release
```

## How it works

WASM blocks use two crates from wafer-run:

- **`wafer-block`** — SDK with types, helpers (`json_respond`, `err_not_found`, ...), `MessageExt` trait, `#[wafer_block]` macro
- **`wafer-core`** — Service clients (`network::do_request`, `db::list`, etc.) with `wasm-component` feature for sync WASM calls

The key pattern is `wit_bindgen::generate!` with type remapping — this generates the WASM
export stubs locally while reusing `wafer_block`'s types, giving full SDK access:

```rust
wit_bindgen::generate!({
    world: "wafer-block",
    path: "../../wit/wit",
    with: {
        "wafer:block-world/types@0.2.0": wafer_block::wafer::block_world::types,
        "wafer:block-world/runtime@0.2.0": wafer_block::wafer::block_world::runtime,
    },
    export_macro_name: "export_block",
});
```

Then `#[wafer_block(..., export_macro = "export_block")]` generates the `Guest` impl and
export stubs using the local macro.

## Project structure

```
src/lib.rs     - Block implementation (WIT setup, routes, handlers)
Cargo.toml     - cdylib crate with wasm-component feature
```
