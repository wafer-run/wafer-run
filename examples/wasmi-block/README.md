# wasmi-block

## What it demonstrates

Authoring a guest WASM block with the `#[wafer_block]` macro from `wafer-sdk`. The example is an "echo" block (`example/echo`) that returns its input back to the caller as JSON. Compiles to a `wasm32-wasip1` `cdylib` that another wafer host loads at runtime via `wasmi`.

This example is structured differently from the others:

- It's a `cdylib` (not a bin), compiled to a `.wasm` artifact.
- It's a **standalone Cargo workspace** (`[workspace]` block in its `Cargo.toml`) so it doesn't pollute the main workspace's target dir or feature unification.
- It builds via `cargo build --target wasm32-wasip1`, not `cargo run`.

## Build

From inside the example's directory:

```
cd examples/wasmi-block
rustup target add wasm32-wasip1                            # one-time
cargo build --release --target wasm32-wasip1
```

The artifact lands at:

```
examples/wasmi-block/target/wasm32-wasip1/release/wafer_example_wasmi_echo.wasm
```

## Use the artifact

The produced `.wasm` is the input to wafer's WASM block loader. It's also what `crates/wafer-run/testdata/echo_block.wasm` is regenerated from when running `wafer-run`'s integration tests — see [`CONTRIBUTING.md`](../../CONTRIBUTING.md) → "Build & test" → testdata gotcha for the copy command.

## Key files

- `src/lib.rs` — the block implementation: a single `EchoBlock` annotated with `#[wafer_block(name = "example/echo", ...)]`.
- `Cargo.toml` — note `[lib] crate-type = ["cdylib"]` and the `[workspace]` block.
- `wafer.toml` — the package manifest (`wafer package`/`wafer publish` metadata).
- `tests/` — host-side tests that load the built `.wasm`.

## Related docs

- [wafer.run/docs/creating-a-block](https://wafer.run/docs/creating-a-block) — block author's guide.
- [wafer.run/docs/wasm-blocks](https://wafer.run/docs/wasm-blocks) — runtime details for WASM blocks.
