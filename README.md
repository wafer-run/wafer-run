# wafer.run

**WAFER** — *WebAssembly Architecture for Flow Execution & Routing*. A wafer-thin runtime for tools, apps, and services. One binary, composable WASM blocks, declarative flows.

[**→ Documentation**](https://wafer.run/docs/quick-start) · [**→ Run an example**](./examples/hello-world) · [**→ Contributing**](./CONTRIBUTING.md)

---

## Why wafer

- **Single binary.** Drop `wafer-run` (or your wafer-built binary) on a host; no runtime to install.
- **WASM blocks.** Sandboxed, language-agnostic guest code via `wasmi` with resumable async host calls. See [creating a block](https://wafer.run/docs/creating-a-block).
- **Composable flows.** Declarative flow files describe how blocks chain into request pipelines. See [waferflow](https://wafer.run/docs/waferflow).
- **Package registry.** `wafer search`, `wafer install`, `wafer publish` — see [the registry](https://wafer.run/docs/registry).
- **Secure by default.** Per-block capabilities enforced at runtime (WRAP). See [block capabilities](https://wafer.run/docs/block-capabilities).

## Quick taste

```rust
use std::sync::Arc;
use wafer_run::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut wafer = Wafer::new()?;

    // Front the runtime with an HTTP server that routes to your blocks
    wafer_flow_http_server::register(&mut wafer, serde_json::json!({
        "listen": "0.0.0.0:8080",
        "routes": [{ "path": "/**", "block": "example/hello" }]
    }))?;

    // Register a block (Block impl elided — see examples/hello-world)
    wafer.register_block("example/hello", Arc::new(HelloBlock))?;

    let wafer = wafer.start().await?;
    tokio::signal::ctrl_c().await.ok();
    wafer.shutdown().await;
    Ok(())
}
```

The full runnable version lives at [`examples/hello-world/`](./examples/hello-world).

## Get started

| If you want to … | Go to |
| --- | --- |
| Read the docs            | [wafer.run/docs/quick-start](https://wafer.run/docs/quick-start) |
| Clone and run an example | [`examples/hello-world`](./examples/hello-world) |
| Hack on wafer-run itself | [`CONTRIBUTING.md`](./CONTRIBUTING.md) |

## Repo layout

```
crates/        Rust crates (runtime, blocks, CLI, SDK)
examples/      Runnable demos — start with hello-world
sdks/          Guest SDKs (Rust today)
packages/      JS/TS client (wafer-client-js)
registry/      Block manifests for the wafer-run registry
docs/          Specs (docs/specs/) and plans (docs/plans/)
common/        Shared resources (see common/README.md)
go/            Go bindings
```

## License

MIT — see [`LICENSE`](./LICENSE).

## Status

Pre-1.0. APIs and schemas are still moving; breaking changes land without deprecation cycles. Registry is currently private (flips public when wafer-run crates publish to crates.io).
