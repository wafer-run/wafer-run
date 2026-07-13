# with-admin-block

Demonstrates the production pattern for typed-resource grants:

1. An **admin block** declares the typed grant in `BlockInfo::grants`.
2. The application calls `wafer.set_admin_block("example/admin")`.
3. The runtime accepts typed grants declared on that block.
4. A separate **feature block** consumes the resource at runtime.

This is the pattern a consuming application uses in its runtime builder
— the admin block `my-org/admin` owns the typed Storage grant
that the `my-org/files` feature block needs.

## Why `set_admin_block` is load-bearing

Wave 13 PR B ([#166](https://github.com/wafer-run/wafer-run/pull/166))
made `set_admin_block` the security boundary for typed grants. A typed
grant declared on a non-admin block is rejected at `seal()` time with
`RuntimeError::GrantsRejected`. Try it — remove the
`wafer.set_admin_block("example/admin");` line in `src/main.rs` and
rerun; `start()` will fail.

The integration test
[`admin_block_example_pattern.rs`](../../crates/wafer-run/tests/admin_block_example_pattern.rs)
pins both the positive case (this example's setup succeeds) and the
negative case (a non-admin block declaring the same grant fails seal)
as machine-checkable behavior.

## Run

```bash
cargo run -p with-admin-block
# In another terminal:
curl http://localhost:8080/
```

The response is `{"folders":[...]}` — the local-storage service's
folder listing, fetched via the typed Storage grant.
