# static-site

## What it demonstrates

Wafer serving static files via the first-party `wafer-run/web` block. Shows how `wafer-flow-http-server` routes every path to `wafer-run/web`, which reads from a `web_root` directory configured at runtime.

On first run the example auto-creates `./public/index.html` with a sample page so there's something to serve.

## Run

```
cargo run -p static-site
```

Then visit `http://localhost:8080/` in a browser, or:

```
curl -v http://localhost:8080/
```

Drop your own files into `./public/` to serve them.

## Key files

- `src/main.rs` — registers the HTTP server with a single `/**` route into `wafer-run/web`, configures `web_root`, and (on first run) seeds `public/index.html`.
- `Cargo.toml` — minimal: just `wafer-run` + `wafer-flow-http-server`. The `wafer-run/web` block is auto-registered via inventory.

## Related docs

- [wafer.run/docs/built-in-blocks](https://wafer.run/docs/built-in-blocks) — `wafer-run/web` block reference.
- [wafer.run/docs/deployment](https://wafer.run/docs/deployment) — shipping a static-site wafer to a server.
