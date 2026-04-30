# hello-world

## What it demonstrates

The smallest end-to-end wafer setup: define an inline `Block` that responds with JSON, register it under `example/hello`, front the runtime with `wafer-flow-http-server`, and serve it on `http://localhost:8080`. Use this as your first read after cloning.

## Run

```
cargo run -p hello-world
```

Then in another terminal:

```
curl http://localhost:8080
```

Response:

```json
{"message":"Hello, World!","path":"/"}
```

## Key files

- `src/main.rs` — the entire example: `HelloBlock` impl, HTTP-server registration (`/**` → `example/hello`), `wafer.start()`, ctrl-C shutdown.
- `Cargo.toml` — uses path deps into `../../crates/wafer-run` and `../../crates/wafer-flow-http-server`. In your own project you'd use crates.io versions.

## Related docs

- [wafer.run/docs/quick-start](https://wafer.run/docs/quick-start) — the recommended next read; same shape, more annotation.
- [wafer.run/docs/core-concepts](https://wafer.run/docs/core-concepts) — Block, Message, OutputStream, and the runtime mental model.
