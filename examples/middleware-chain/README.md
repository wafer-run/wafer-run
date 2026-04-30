# middleware-chain

## What it demonstrates

A custom HTTP flow with a middleware chain in front of the handler:

```
security-headers → cors → request-logger → api-key-check → router → handler
```

Shows how middleware blocks short-circuit the chain: requests without `X-Api-Key` are blocked by `api-key-check` before reaching the router.

## Run

```
cargo run -p middleware-chain
```

Then:

```
# Allowed (has API key):
curl http://localhost:8080/api/echo \
  -H "X-Api-Key: secret123" \
  -H "Content-Type: application/json" \
  -d '{"hello":"world"}'

# Blocked by api-key-check:
curl http://localhost:8080/api/echo
curl http://localhost:8080/stats

# Inspector:
curl http://localhost:8080/_inspector/ui
```

## Key files

- `src/main.rs` — inline `RequestLoggerBlock` and `ApiKeyCheckBlock` impls, plus the custom flow definition that wires the chain in order.
- `Cargo.toml` — path deps on `wafer-run` + `wafer-flow-http-server`.

## Related docs

- [wafer.run/docs/core-concepts](https://wafer.run/docs/core-concepts) — block ordering and short-circuit semantics (`OutputStream::continue_with` vs `OutputStream::respond`).
- [wafer.run/docs/built-in-blocks](https://wafer.run/docs/built-in-blocks) — the first-party `cors` and `security-headers` blocks used here.
