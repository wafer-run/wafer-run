# wafer-run examples

Each subdirectory is a self-contained, runnable example. New to wafer? Start with [**hello-world**](./hello-world).

| Example | What it demonstrates | Run |
|---|---|---|
| [`hello-world`](./hello-world)         | Smallest possible wafer + HTTP server | `cargo run -p hello-world`         |
| [`api-server`](./api-server)           | Wafer behind a JSON REST API          | `cargo run -p api-server`          |
| [`static-site`](./static-site)         | Wafer serving static files            | `cargo run -p static-site`         |
| [`multi-flow`](./multi-flow)           | Multiple flows in one binary          | `cargo run -p multi-flow`          |
| [`middleware-chain`](./middleware-chain) | Composed middleware blocks         | `cargo run -p middleware-chain`    |
| [`with-admin-block`](./with-admin-block) | Typed grants via admin block (production pattern) | `cargo run -p with-admin-block` |
| [`wasmi-block`](./wasmi-block)         | Authoring a guest WASM block          | (see its README — built with `cargo build --target wasm32-wasip1`) |

Each example's README explains what to read in `src/` and which [wafer.run/docs](https://wafer.run/docs/quick-start) pages go deeper.
