# multi-flow

## What it demonstrates

Multiple flows in a single wafer binary, visualized through the inspector. Registers the standard `wafer-run/http-server` flow plus an `onboarding` flow (for illustration); both are listed by `/_inspector/flows`. Includes inline blocks for `greeter`, `health`, and a `not-found` fallback.

## Run

```
cargo run -p multi-flow
```

Then:

```
curl http://localhost:8080/greet?name=Alice
curl http://localhost:8080/_inspector/flows | python3 -m json.tool
curl http://localhost:8080/_inspector/flows/onboarding | python3 -m json.tool
```

Visit `http://localhost:8080/_inspector/ui` for the visual flow inspector.

## Key files

- `src/main.rs` — `GreeterBlock`, `HealthBlock`, `NotFoundBlock` impls, plus registration of the `onboarding` data-pipeline flow alongside the HTTP server flow.
- `Cargo.toml` — path deps on `wafer-run` + `wafer-flow-http-server`.

## Related docs

- [wafer.run/docs/waferflow](https://wafer.run/docs/waferflow) — flow composition concepts.
- [wafer.run/docs/flow-configuration](https://wafer.run/docs/flow-configuration) — flow config schema.
