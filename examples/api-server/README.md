# api-server

## What it demonstrates

A small JSON REST API hosted by wafer, backed by a SQLite database. Shows how to register the `wafer-run/inspector` and `wafer-run/web` blocks alongside a custom `example/api-handler` block, wire up the database service, and route `/api/**` through to the handler.

The example's data lives in `data/notes.db` (auto-created on first run).

## Run

```
cargo run -p api-server
```

Then:

```
curl -X POST http://localhost:8080/api/notes \
  -H 'Content-Type: application/json' \
  -d '{"title":"Hello","body":"World"}'

curl http://localhost:8080/api/notes
```

Visit `http://localhost:8080/_inspector/ui` to inspect the running flow.

## Key files

- `src/main.rs` — block registration (HTTP server, database, logger, inspector), inline `api-handler` block.
- `Cargo.toml` — adds `wafer-block-sqlite`, `wafer-block-logger`, `wafer-core` on top of the runtime + HTTP server.

## Related docs

- [wafer.run/docs/flow-configuration](https://wafer.run/docs/flow-configuration) — the flow config schema used by `wafer-flow-http-server`.
- [wafer.run/docs/built-in-blocks](https://wafer.run/docs/built-in-blocks) — `wafer-run/web`, `wafer-run/inspector`, sqlite, logger.
