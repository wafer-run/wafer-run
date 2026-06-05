# wafer-run — Rust Best-Practices Audit

**Date:** 2026-06-05  ·  **Standard:** Apollo GraphQL *Rust Best Practices* handbook (the `rust-best-practices` skill, 9 chapters)  ·  **Branch:** `main`

**Method:** Mechanical ground truth (`cargo +nightly fmt --check`, `cargo clippy` default + workspace lints, `cargo clippy -W pedantic -W nursery`) followed by a 20-agent fan-out review (one agent per crate-group, full 9-chapter checklist) and a 20-agent adversarial verification pass that re-opened every cited line. Scope: all ~70k LoC of Rust across the 35-crate workspace.

## Headline

The codebase is **already in very good shape against this standard.** Objective tooling is clean and the human-judgment review found **no systemic anti-patterns** — issues are localized.

| Check | Result |
|---|---|
| `cargo +nightly fmt --check` | ✅ clean (zero drift) |
| `cargo clippy` (default + 6 workspace lints) | ✅ clean (zero warnings) |
| `cargo clippy -W pedantic -W nursery` | ⚠️ ~1,900 advisory hits (tiers deliberately not enabled) |
| Judgment review (verified) | **3 high · 31 medium · 76 low** (7 false positives rejected) |

### What's already excellent (verified, not flagged)

- **Error handling:** `thiserror` enums throughout the library crates (`RuntimeError`, `DatabaseError`, `StorageError`, `AuthError`, `CryptoError`, `LlmError`, …); `anyhow` correctly confined to the `wafer-cli` binary; `?` over match chains; context preserved with `map_err`/`with_context`.
- **Panics:** no `unwrap`/`expect`/`panic!` on reachable external-input paths in the core runtime, wasm host, storage/db backends, SDK, or examples — the ones that exist are in tests or documented provable invariants.
- **Concurrency (ch9):** no `std`/`parking_lot` guard held across an `.await` anywhere (the wasm resume loop explicitly drops `&mut store` before each await; sqlite scopes its `Mutex` in an inner block; monitoring snapshots-then-drops). The one async lock held across `.await` is a `tokio::sync::Mutex` that is *designed* for it and documented.
- **`unsafe`:** the wasm FFI blocks carry `// SAFETY:` rationale and RAII guards (`CloseGuard`, `ContextGuard`).
- **Docs:** `#![warn(missing_docs)]` is enforced and honored across all crates.
- **Exemplary modules called out by reviewers:** `wasm/stream.rs`, `wire/*` DTOs, `clients/*` in the SDK, `monitoring`, `wafer-sql-utils` (parameterized + sanitized identifiers), `wafer-test-support`.

---

## HIGH severity (3) — fix first

### H1. wasm32 Instant stub breaks deadline cancellation + contradicts doc
`crates/wafer-run/src/platform.rs:15-33` — *ch8-comments*

**Problem.** On wasm32 `Instant` is a unit struct deriving `PartialOrd`/`Ord`, so any two values compare `Equal`. In `context.rs:484` the deadline check `if Instant::now() >= deadline` is therefore ALWAYS true on wasm32. Result: any `RuntimeContext` constructed with `deadline: Some(_)` is reported as cancelled on the very first `is_cancelled()` call (which also latches `cancelled = true`), so the block never runs. The platform.rs and context.rs doc comments both claim Instant uses `Performance.now()` on wasm32 ("zero-cost on native, Performance.now() on wasm32"), but the wasm32 arm is a no-op stub that returns `Duration::ZERO` and a constant unit value — the comment is stale/misleading and hides a real correctness bug for the solobase-web SW host.

**Fix.** Either back the wasm32 `Instant` with `web_time::Instant` (it supports wasm32 via Performance.now(), which is presumably why `web-time` is already a dep) and delete the hand-rolled stub, or — if deadlines are intentionally unsupported on wasm32 — make `deadline` unconstructable on wasm32 / make `is_cancelled` ignore the deadline arm under `cfg(target_arch="wasm32")`, and fix both doc comments to say the wasm32 clock is a no-op stub, not Performance.now().

### H2. pagination_params multiply can overflow on user input
`crates/wafer-block/src/types.rs:1265-1274` — *ch4-errors*

**Problem.** `page` is parsed straight from the `page` query string with only `.unwrap_or(1).max(1)` — there is no upper bound. `offset = (page - 1) * page_size` then multiplies an unbounded user-controlled `usize` by `page_size` (capped at 100). A request with `?page=184467440737095517` (or any value near usize::MAX/100) overflows: in debug builds this panics, and in release it silently wraps to a bogus offset. Query parameters are external/network input, so this is a reachable panic/incorrect-result path. `page_size` is correctly capped via `.min(100)` but `page` is not capped at all.

**Fix.** Cap `page` (e.g. `.min(SOME_MAX_PAGE)`) and/or compute the offset with `page.saturating_sub(1).saturating_mul(page_size)` so a malicious page number yields a clamped offset instead of overflowing.

### H3. requires attribute parsed but silently discarded
`crates/wafer-block-macro/src/lib.rs:607` — *ch4-errors*

**Problem.** The `requires` optional attribute is documented (line 500-501: `requires` — list of block names this block may call) and parsed into `_requires` via `let _requires = args.get_str_list("requires");`, but the result is bound to an underscore-prefixed variable and never used. The generated `block_info()` (lines 757-766) builds `BlockInfo` with name/version/interface/summary, `instance_mode`, capabilities, and skill — but never calls `.requires(...)`, even though `BlockInfo::requires(Vec<String>)` exists (wafer-block/src/types.rs:379). This is not cosmetic: `BlockInfo.requires` is an access-control gate actively enforced by the runtime — context.rs:175-182 denies `call_block` with "block '{name}' not in requires list — call_block denied", and context.rs:327-330 treats an EMPTY requires list as "no restriction" (called_requires = None). So a WASM block author who declares `requires = ["wafer-run/database"]` through the documented macro attribute gets an empty list, the restriction silently never applies, and call_block is left unrestricted. A documented, security-relevant attribute that does nothing is worse than no attribute. Fix: thread `_requires` into the generated `block_info()` (emit `.requires(vec![#(#requires.to_string()),*])` when non-empty), and add a test asserting `block_info().requires` matches the declared list.

**Fix.** Rename `_requires` to `requires`, build a `requires_expr` token stream the same way capabilities lists are built, and apply it in the generated `block_info()` body (e.g. `if !requires.is_empty() { info = info.requires(vec![...]); }`). Add a test in tests/ asserting the declared requires list reaches BlockInfo.requires.

---

## MEDIUM severity (31)

### Swallowed errors / stringly-typed errors / reachable panics (ch4-errors)

- **M1. instantiate matches WASI shutdown on error-message substring** — `crates/wafer-run/src/wasm/wasmi_loader/mod.rs:146-161`  
  After calling the guest `_start`, success vs. expected-shutdown is decided by `if !msg.contains("proc_exit")` on the stringified trap error. The proc_exit stub itself produces that string (`format!("guest called proc_exit({code})")` in imports.rs:481), so the contract is internal and currently consistent — but keying control flow off a substring of a human-readable error message is brittle: any reword of the stub message, or a guest trap whose message coincidentally contains 'proc_exit', silently changes whether startup is treated as success or failure. This runs on guest-controlled (external WASM) input.  
  *Fix:* Make the proc_exit stub trap a typed `HostError` marker (like the existing StreamFinishTrap/LoadAssetTrap markers in abi.rs) and downcast the wasmi error to that type instead of substring-matching its Display string; ignore proc_exit(0), surface non-zero/other traps as errors.
- **M2. SSRF ipv4 allowlist misses CGNAT/reserved ranges** — `crates/wafer-core/src/security.rs:50-77`  
  is_blocked_ipv4 enumerates 0/8, 127/8, 10/8, 172.16/12, 192.168/16, 169.254/16 but omits several internal/reserved ranges that real SSRF guards block: 100.64.0.0/10 (carrier-grade NAT — routable internal infra on many cloud providers and the path used by some metadata proxies), 192.0.0.0/24 (IETF), 198.18.0.0/15 (benchmarking), 240.0.0.0/4 (reserved) and the 255.255.255.255 broadcast. The module's own docstring at lines 47-49 advertises blocking 'private/loopback/link-local/etc' addresses, so the gap is a behavior-vs-doc mismatch on a security boundary, not just a missing-feature. Because is_blocked_ip is the post-DNS-resolution rebinding check (SEC-019), a hostname resolving into 100.64/10 would pass.  
  *Fix:* Add the missing ranges (at minimum 100.64.0.0/10, plus 240.0.0.0/4 and 255.255.255.255) to is_blocked_ipv4, mirroring std::net::Ipv4Addr::is_shared / is_reserved / is_broadcast where available, and add regression tests alongside the existing ones (e.g. assert is_blocked_url("http://100.64.0.1")).
- **M3. MetaAccess::set on slice panics** — `crates/wafer-block/src/types.rs:1309-1321`  
  `impl MetaAccess for [crate::MetaEntry]` implements `set` as an unconditional `panic!("cannot insert into a slice; use Vec<MetaEntry> instead")`. The trait is public (`pub trait MetaAccess`) and `set` is part of its contract, so any caller holding `&mut [MetaEntry]` (e.g. a sub-slice of a meta vec) and calling `.set(...)` through the trait hits a guaranteed panic with no compile-time prevention. This is an illegal state encoded as a runtime panic rather than a type-level distinction — the slice impl provides read methods that work fine but a mutating method that always aborts.  
  *Fix:* Either split the trait into a read-only `MetaGet` (impl for slices) and a mutating `MetaSet` (impl only for `Vec`), so a slice never exposes a method that can only panic; or change `set` to return `Result`/`bool` so the failure is in the type system, not a panic.
- **M4. BlockInfo::validate returns stringly-typed error in library crate** — `crates/wafer-block/src/types.rs:360-370`  
  `wafer-block` is a shared library crate (consumed by wafer-core and wafer-sdk). `BlockInfo::validate` returns `Result<(), String>`, a stringly-typed library error. The handbook flags `Result<_, String>` in library crates: callers cannot match on the failure kind, and the message format is the only contract. The crate already uses `thiserror` elsewhere (see streams/output.rs `SinkClosed`/`SinkSendError`), so the pattern is available.  
  *Fix:* Introduce a `#[derive(thiserror::Error)]` enum (e.g. `BlockInfoError::ReservedConfigKey { block: String, key: String }`) and return that, so consumers can match on the specific validation failure rather than parsing the message.
- **M5. publish response body read error swallowed by unwrap_or_default** — `crates/wafer-cli/src/commands/publish.rs:89-93`  
  After `ensure_ok`, `resp.text().await.unwrap_or_default()` discards any I/O error reading the success-response body, turning a truncated/aborted transfer into an empty string. The subsequent `serde_json::from_str("")` then fails with a confusing 'decode publish response: ' (empty body) message that hides the real cause (network read failure). Every other body read in this crate uses `.with_context(...)?` (see registry_client.rs search/get_package). This one path is inconsistent and loses the underlying error.  
  *Fix:* Propagate the read error: `let body = resp.text().await.with_context(|| format!("read publish response from {endpoint}"))?;` so a failed body read surfaces as a transport error rather than a misleading decode error.
- **M6. Like/In filters silently dropped or corrupted on value type mismatch** — `crates/wafer-sql-utils/src/query.rs:20-35`  
  build_condition is fed Filter values that originate from external/user/config input (serde_json::Value). Two operators silently misbehave on a type mismatch instead of surfacing the error: the In branch `continue`s (drops the predicate entirely) when `filter.value` is not a JSON array, and the Like branch coerces a non-string via `.as_str().unwrap_or("")`, turning e.g. a numeric value into `LIKE ''`. A dropped In-predicate widens the result set (a search/authorization filter can vanish silently), and `LIKE ''` returns no rows where the caller expected a match. Neither is logged or returned; the caller gets a quietly-wrong query.  
  *Fix:* Make build_condition return a Result (or have the In/Like arms produce an explicit always-false / always-true sentinel and document it) rather than silently dropping or coercing. At minimum, on a non-array In value emit `1=0` (no match) instead of `continue` (which is the opposite, widening the set), and reject a non-string Like value rather than coercing to empty. The crate already returns Result from build_create_table, so a fallible build_condition is consistent.
- **M7. Stringly-typed errors in library crate (Result<_, String>)** — `crates/wafer-sql-utils/src/ddl.rs:16-27, 116`  
  wafer-sql-utils is a library crate (it has no binary). validate_fk_action returns Result<&'static str, String> and build_create_table returns Result<crate::Statement, String>. Stringly-typed errors force every consumer to match on substrings (the test does `err.contains("invalid foreign-key referential action")`) and lose the ability to handle distinct failure modes programmatically. The crate has no thiserror error enum at all.  
  *Fix:* Define a `#[derive(thiserror::Error)] pub enum SqlBuildError { InvalidFkAction { action: String }, ... }` and return it from the fallible builders. thiserror is already used across the workspace; add it to Cargo.toml ([dependencies] only — no anyhow in a lib).
- **M8. assert! panic in aggregate builder where Result is the crate's established pattern** — `crates/wafer-sql-utils/src/aggregate.rs:80-85`  
  build_daily_count is a public library builder. It panics via assert! if date_field is not a plain identifier. The doc-comment argues date_field 'is always an internal constant from caller code', but that is a soft invariant the type system does not enforce — a misuse (or a future caller that threads a config/user value through) crashes the whole runtime rather than returning an error. The crate already demonstrates the fallible-builder pattern (build_create_table -> Result, validate_fk_action), so a panic here is inconsistent. Contrast with vector.rs / introspect.rs which sanitize identifiers rather than asserting.  
  *Fix:* Either return Result<Statement, _> and reject the bad identifier as an error value, or sanitize date_field via ident::sanitize_ident the way the other identifier-interpolating builders do, instead of asserting. If the invariant truly cannot be violated, prefer debug_assert! so a release build degrades gracefully — but the fallible-return form is the better fit here.
- **M9. sqlite table_columns unit-error type swallows DB failure** — `crates/wafer-block-sqlite/src/service.rs:143-153`  
  `table_columns` returns `Result<Vec<String>, ()>`, mapping every rusqlite failure to `()` via `.map_err(|_| ())` and additionally dropping per-row errors with `.filter_map(|r| r.ok())`. The real error (prepare failed, locked DB, corrupt schema) is destroyed. Both callers (`ensure_columns_from_data`, `ensure_columns_for_query`) then treat the `Err(())` as 'no columns known' and silently skip column creation. So a transient DB error during introspection is indistinguishable from an empty table, and the subsequent INSERT/UPDATE will fail with a confusing 'no such column' instead of the underlying cause. The Postgres sibling (`get_columns`, service.rs:239-251) correctly returns `Result<_, DatabaseError>` and propagates.  
  *Fix:* Return `Result<Vec<String>, DatabaseError>` (or `rusqlite::Result`) and propagate the real error up through the ensure-columns helpers, matching the Postgres backend. At minimum, log the discarded error instead of mapping to `()`.
- **M10. sqlite ensure_columns helpers silently swallow ALTER errors** — `crates/wafer-block-sqlite/src/service.rs:124-208`  
  `ensure_columns_from_data` and `ensure_columns_for_query` both run `db.execute_batch(&alter.sql).ok()` and return `()`, discarding any failure of `ALTER TABLE ... ADD COLUMN`. The Postgres equivalents (`ensure_columns_from_data` service.rs:257-280, `ensure_columns_for_query` service.rs:287-326) return `Result<(), DatabaseError>` and propagate `add column {key}: {e}`. This is a behavioural divergence between the two backends on the same logical operation: on SQLite a failed lazy column-add is invisible, so a later INSERT referencing the missing column fails with an opaque error far from the cause. The early-return on `table_columns` error (`let Ok(existing) = ... else { return; }`) compounds this — the whole ensure step is skipped without a trace.  
  *Fix:* Make both helpers return `Result<(), DatabaseError>`, propagate `execute_batch` errors with context (`format!("add column {key}: {e}")`), and have `create`/`update`/`ensure_query_columns` `?` them — mirroring the Postgres backend so the two stay consistent.
- **M11. postgres row_to_record swallows try_get errors as Null** — `crates/wafer-block-postgres/src/service.rs:654-730`  
  Every type arm in `row_to_record` maps a `try_get` failure to `serde_json::Value::Null` (`Err(_) => serde_json::Value::Null`) with no logging. A genuine decode failure (e.g. a column whose Postgres type the match doesn't anticipate, or an out-of-range value) is silently turned into a NULL in the returned Record, so callers see missing data with no signal that decoding failed. The SQLite read path (run_fetch, service.rs:234-240) at least `tracing::warn!`s on a row error before skipping. For numeric/UUID/JSON columns this can hide real data-loss.  
  *Fix:* Distinguish 'column is SQL NULL' (Ok(None)) from 'decode failed' (Err): on the Err arm, `tracing::warn!(column = %col_name, type = %type_name, error = %e, "failed to decode column")` before falling back, or propagate a DatabaseError::Internal for the unexpected-type case so the failure is observable.
- **M12. skill parameters JSON panics at runtime instead of compile time** — `crates/wafer-block-macro/src/lib.rs:730-741`  
  The generated `block_info()` parses the author-supplied `skill(parameters = "...")` JSON at RUNTIME with `serde_json::from_str(#parameters_json).expect(concat!("skill parameters JSON parse error in block ", #name))`. `parse_skill` (lines 260-324) only verifies `parameters` is a string literal at macro-expansion time; it never validates the JSON. So malformed JSON in the literal compiles cleanly and then panics the first time `block_info()` is called (which happens during native registration via `register_static_block!` and during WASM `__wafer_info`). The input is fully author-controlled and known at compile time, so this is a compile-time-detectable error deferred to a runtime panic — exactly the failure mode the macro's own design avoids elsewhere (it funnels attribute typos through syn::Error::to_compile_error per the wafer_block_impl doc comment). Validate the JSON in `parse_skill` with `serde_json::from_str::<serde_json::Value>(&parameters)` and emit a spanned `syn::Error` on the `parameters` literal, so a bad schema fails the build at the macro site rather than panicking a running block. (proc-macro2/syn are already deps; serde_json would need adding as a non-dev dependency for the validation, or validate structurally.)  
  *Fix:* In parse_skill, after extracting `parameters`, validate it parses as JSON and return a spanned syn::Error pointing at the parameters literal on failure. Then the generated code can keep the .expect() as a genuine invariant (unreachable on validated input) or be documented as such.
- **M13. compare_values silently returns false for non-numeric ordered comparisons (contradicts TypeError doc)** — `crates/wafer-flow/src/expr.rs:468-485`  
  `compare_values` returns `bool`, so an ordered comparison (`>`, `<`, `>=`, `<=`) of non-numeric operands silently yields `false` instead of surfacing an error. The `ExprError::TypeError` doc in error.rs:69-72 explicitly advertises this case ("comparing non-numeric values with `>`") as a TypeError, but no such error can ever be produced — the `(None, _) | (_, None)` arm just falls through to `_ => false`. A `when` expression like `$.a.name > $.b.name` (string fields) authored in a flow JSON will quietly evaluate the branch as not-taken rather than failing validation/execution, hiding an authoring mistake. Either make `compare_values` return `Result<bool, ExprError>` and emit TypeError, or correct the error-type doc to match the lenient behaviour.  
  *Fix:* Change `compare_values` to return `Result<bool, ExprError>` and return `Err(ExprError::TypeError(...))` when either operand of an ordered comparison is non-numeric; propagate through `eval`. Alternatively, if lenient false is intentional, drop the misleading clause from the `ExprError::TypeError` doc.
- **M14. unknown field type silently mapped to String** — `crates/wafer-schema/src/manifest/to_schema.rs:85-98`  
  field_type_to_data_type takes a type string straight from an external block manifest (JSON config, attacker/user/registry-controlled) and maps any unrecognised value to DataType::String via the catch-all `_ => DataType::String`. A typo like "itn" or a genuinely-unsupported type silently produces a STRING column instead of surfacing a configuration error. The conversion is infallible by signature, so the caller cannot distinguish "explicitly String" from "unknown, fell through". This swallows a real config mistake that will only manifest later as wrong column types / failed inserts.  
  *Fix:* Return an error for unknown types (change the fn to `Result<DataType, SchemaError>` with a thiserror enum, e.g. `UnknownFieldType { collection, field, ty }`, and propagate through collection_to_table/collections_to_tables) so a bad manifest is rejected at conversion time rather than silently becoming a String column.
- **M15. malformed FK ref string silently dropped** — `crates/wafer-schema/src/manifest/to_schema.rs:70-80`  
  A non-empty `r#ref` from the manifest that does not split into exactly two parts on '.' (e.g. "users" with no column, or "a.b.c") is silently ignored: the `if parts.len() == 2` guard means a malformed FK declaration produces a column with NO foreign key and no error/warning. The user declared a reference and it vanished. This is a swallowed config error on external input.  
  *Fix:* Use let-else and error/warn on the malformed case: `let Some((table, column)) = f.r#ref.split_once('.') else { return Err(SchemaError::BadRef{...}) };` (split_once is also cleaner than splitn+Vec+index). At minimum emit a tracing::warn so the dropped reference is visible.
- **M16. inspector swallows serde errors into empty 200 body** — `crates/wafer-block-inspector/src/lib.rs:158-240`  
  Every JSON route serializes with `serde_json::to_vec(...).unwrap_or_default()` (lines 164, 170, 176, 182, 198, 218, 240). On a serialization failure this silently substitutes an empty Vec, so the handler returns HTTP 200 with `Content-Type: application/json` and a zero-byte body instead of an error. The failure is invisible to operators — exactly the swallowed-error pattern the handbook warns against (ch4: don't `.unwrap_or_default()` away a Result that should be surfaced). The inputs (`registered_blocks`, `flow_defs_json`, etc.) are runtime-controlled and effectively always serialize, so this is unlikely to fire — hence medium not high — but a real failure would hand the operator a blank screen with no diagnostic.  
  *Fix:* Map serialization failures to a `WaferError` (e.g. `OutputStream::error` with `ErrorCode::Internal`) or at least `tracing::error!` the `serde_json::Error` before falling back. A small helper `fn json_or_error(v: &impl Serialize) -> OutputStream` would centralize this for all seven call sites.
- **M17. s3 init error uses unrecognized stringly code "init"** — `crates/wafer-block-s3/src/lib.rs:127`  
  On S3 service init failure the block builds `WaferError::new("init", format!("wafer-run/s3: {e}"))`. `WaferError::new` takes `impl Into<ErrorCode>` and `From<&str> for ErrorCode` (crates/wafer-block/src/types.rs:1121-1144) only recognizes a fixed set of strings — "init" is NOT one of them, so it falls through the `_ => Self::Unknown` arm. The init failure is therefore tagged `ErrorCode::Unknown` rather than `ErrorCode::Internal`. This is exactly the "magic code / implicit mapping" hazard the workspace rules call out: a typo-prone string silently downgrades a real internal failure into an unclassified one (and http-listener only maps it to 500 via the `_ => 500` catch-all, masking the mistake).  
  *Fix:* Pass a real code: `WaferError::new(ErrorCode::Internal, format!("wafer-run/s3 init: {e}"))`. The handle() path already uses `ErrorCode::Internal` for the not-initialized case (lib.rs:93-96); make the init-failure path consistent with it.
- **M18. respond body non-UTF-8 silently dropped to empty string** — `crates/wafer-ffi/src/lib.rs:104-118`  
  For the `respond` terminal, `String::from_utf8(buf.body).unwrap_or_default()` silently replaces a non-UTF-8 response body with an empty string. The caller receives `{"action":"respond","body":""}` with no signal that data was lost. Note the `halt` branch (lines 128-143) correctly base64-encodes its body precisely because halt 'may carry non-UTF-8 or empty bodies' — but a `respond` body has no such guarantee either (a block could emit binary). This is a swallowed-data path: bytes the block produced never reach the consumer.  
  *Fix:* Either base64-encode the respond body the same way as halt (add a `body_base64` field), or distinguish UTF-8 failure from a genuinely empty body (e.g. emit `body_base64` only when `from_utf8` fails). Don't collapse a decode failure into a valid-looking empty `body`.
- **M19. respond body non-UTF-8 silently dropped (node)** — `crates/wafer-run-node/src/lib.rs:164-178`  
  Same swallowed-data bug as the FFI crate: the `respond` branch uses `String::from_utf8(buf.body).unwrap_or_default()`, turning a non-UTF-8 block response into an empty `body:""` with no error. The `halt` branch (lines 192-207) base64-encodes for exactly this reason, so `respond` is inconsistent.  
  *Fix:* Base64-encode the respond body or emit a distinct field on UTF-8 failure, matching the halt branch. Keep both crates' encoding behavior identical.
- **M20. PureBlock trait uses stringly-typed Result<Vec<u8>, String> in a library crate** — `sdks/rust/src/pure.rs:44-50`  
  `PureBlock::handle` returns `Result<Vec<u8>, String>` — a stringly-typed error on the public trait of a library (SDK) crate. The rest of the SDK uniformly returns the structured, thiserror-backed `WaferError` (used by every clients/* module and stream.rs). A `String` error loses the `ErrorCode` discriminant the host bridge relies on and forces every implementor to hand-format messages with `.map_err(|e| e.to_string())` (see the doc example lines 19-23).  
  *Fix:* Return `Result<Vec<u8>, WaferError>` from `PureBlock::handle` to match the rest of the SDK surface and preserve the error code, or define a dedicated `thiserror` enum for pure-block failures. Update the doc example accordingly.

### Avoidable allocation on hot paths (ch3-perf)

- **M21. run_block clones full BlockInfo to read requires on hot path** — `crates/wafer-run/src/runtime/runner.rs:82-89`  
  `run_block` is the per-request dispatch entry point (called by the HTTP adapter for every external request). On each call it does `let info = block.info();` solely to read `info.requires`. `BlockInfo` (wafer-block/src/types.rs:209) is a large struct of many owned `String`/`Vec` fields (name, version, interface, summary, requires, collections, config_keys, flow_config, grants, endpoints, ...), and `Block::info()` returns a fresh clone of the whole thing. So every request allocates and clones the entire BlockInfo just to test/move one `Vec<String>`.  
  *Fix:* Expose a cheap accessor on the `Block` trait (producer-side change in wafer-block) such as `fn requires(&self) -> &[String]` so the hot path borrows instead of cloning the whole BlockInfo. Short of the trait change, this clone is unavoidable, but the trait should grow the narrow accessor — the same `info()` clone also appears in the obs path nearby.
- **M22. registered_blocks/block_configs/interface_specs clone whole collections per call** — `crates/wafer-run/src/context.rs:501-516`  
  `registered_blocks()` clones `snapshot.blocks` (Vec<BlockInfo>), `block_configs()` clones a whole `HashMap<String, serde_json::Value>`, and `interface_specs()` clones a `Vec<InterfaceSpec>`, every time a block calls them. These are introspection accessors over an immutable Arc'd `StartupSnapshot`; the data never changes after seal, so a per-call deep clone of potentially large config/interface maps is pure waste on any block that introspects (admin UI, SQL explorer, etc.). The trait returns owned values, but the snapshot is already Arc-shared.  
  *Fix:* Where the trait signature allows, return borrows (`&[BlockInfo]`) instead of owned Vecs; where owned is mandated by the trait, consider changing the trait to hand back the shared `Arc<StartupSnapshot>` (or `Arc`-wrapped sub-fields) so callers clone an Arc, not the contents. At minimum document why a deep clone is acceptable here if these are cold paths.
- **M23. obs context + message cloned per step regardless of handlers** — `crates/wafer-run/src/waferflow/executor.rs:160-191`  
  On every flow step the executor unconditionally builds an `ObservabilityContext` with five allocations (`flow.id.clone()`, `step.id.clone()`, `step.block.clone()`, `trace_id.to_string()`) plus a full `message: Some(current_msg.clone())`. `fire_block_start`/`fire_block_end` only take `&ObservabilityContext` and, with zero registered handlers (the default — observability is opt-in), iterate an empty Vec and touch nothing. The whole context — most expensively the `Message` clone — is therefore allocated and discarded on the hot per-step path even when no subscriber exists. `ObservabilityBus` exposes the handler vecs internally, so a cheap `has_block_handlers()` guard (or building obs_ctx lazily only when handlers are present) avoids the clone entirely in the common no-observability case.  
  *Fix:* Add an `ObservabilityBus::any_block_handlers(&self) -> bool` (reads both RwLocks for non-empty) and gate the `ObservabilityContext { .. current_msg.clone() .. }` construction + `fire_block_start`/`fire_block_end` behind it, so the per-step Message clone only happens when a handler is actually registered.

### Duplication & idiom (ch1-idioms)

- **M24. duplicated NativeTypedFrameStream in llm and image clients** — `crates/wafer-core/src/clients/image.rs:188-272`  
  `NativeTypedFrameStream<T>` plus its `Stream` impl in clients/image.rs (lines 188-272) is byte-for-byte identical to the one in clients/llm.rs (lines 198-282) — same fields, same `new`, same ~55-line `poll_next` match handling every StreamEvent terminal. Two independent copies of the same generic decoder means a fix to one (e.g. a new StreamEvent variant or a decode-error-context tweak) has to be made twice and can silently drift. This is exactly the kind of mechanical duplication the handbook flags against DRY/maintainability.  
  *Fix:* Hoist a single generic `NativeTypedFrameStream<T>` into a shared module (e.g. clients/mod.rs alongside `read_header_frame`/`buffered_header_and_body`) and have both clients/llm.rs and clients/image.rs re-use it. The only per-call difference is the `context: &'static str` label, which is already a field.
- **M25. yank parse_target duplicates info::parse_target and accepts malformed targets** — `crates/wafer-cli/src/commands/yank.rs:16-21`  
  yank parses the target inline with two `split_once` calls instead of reusing the well-tested `info::parse_target`. This duplicate parser is laxer and admits malformed input: `org/block/extra@1.0` puts `block/extra` into the block segment, and `org/@1.0` accepts an empty block. info::parse_target already rejects extra segments, empty segments, and trims whitespace, and is unit-tested. Two parsers for the same `org/block@version` grammar will drift.  
  *Fix:* Reuse `crate::commands::info::parse_target(&target)?` and require the version: `let (org, block, version) = parse_target(&target)?; let version = version.ok_or_else(|| anyhow::anyhow!("target must be org/block@version"))?;`. Delete the inline split_once parsing.

### Stale / misleading comments (ch8-comments)

- **M26. stale comment: find_wasm_in_dir claims stem-preference it doesn't do** — `crates/wafer-cli/src/build.rs:187-196`  
  The multi-wasm arm's comment says it will "prefer one whose stem matches common output names" then "Fall back to the first one", but the code does neither preference step — it unconditionally warns and returns found[0] (insertion order from read_dir, which is filesystem-arbitrary). The comment describes behaviour that was never implemented, which will mislead the next maintainer into thinking there is a selection heuristic to debug.  
  *Fix:* Either implement the stem-matching preference (e.g. prefer a stem equal to the package name / manifest block name) or correct the comment to state plainly that the first .wasm in arbitrary read_dir order is used and that callers with multiple .wasm outputs should disambiguate. Reading the manifest name here and matching it would also make multi-bin/workspace projects deterministic.
- **M27. run output-to-JSON logic duplicated across ffi and node crates** — `crates/wafer-run-node/src/lib.rs:163-222`  
  The entire `collect_buffered` -> JSON match (all six TerminalNotResponse arms, the respond/halt meta-map construction, the base64 halt encoding, the 'stream ended without terminal event' message) is a near-exact copy of `output_to_json` in crates/wafer-ffi/src/lib.rs:101-155. Two copies of the wire-format mapping will drift: a fix to one (e.g. the non-UTF-8 body issue above, or a new terminal variant) must be remembered in both. The shape (`{action: respond|drop|error|continue|halt}`) is the FFI/node JSON contract and should have a single source of truth.  
  *Fix:* Extract the BufferedResponse/TerminalNotResponse -> serde_json::Value mapping into one shared function (e.g. in wafer-block or a small shared helper) and call it from both crates. Both already depend on wafer-block.
- **M28. blocking_write panic claim in wafer_register is overstated** — `crates/wafer-ffi/src/lib.rs:400-403`  
  `wafer_register` calls `runtime.inner.blocking_write()` with the comment 'we're guaranteed to be on the C caller's thread (no tokio context expected)'. That guarantee is not enforceable: the module docs (lines 45-47) state the async callbacks may run on tokio-owned threads, and nothing stops a C consumer from calling `wafer_register` from inside a `WaferDoneCb` (i.e. on a tokio worker thread). `Mutex/RwLock::blocking_write` panics if called from within a tokio runtime context, so this is a reachable panic. It is caught by the surrounding `catch_unwind` and turned into a confusing 'panic in wafer_register' JSON error rather than a meaningful one — and the comment misleads a maintainer into thinking the precondition is guaranteed.  
  *Fix:* Either soften the comment to state the precondition is a caller contract (not a guarantee) and that violating it yields a panic-to-error, or make registration robust by spawning onto `runtime.rt` and using `.write().await` like the other ops (registration is already an async-callback-capable surface in the node crate). At minimum, the 'guaranteed' wording should be corrected.
- **M29. BlockDef::to_json silently drops input/output schema fields** — `sdks/rust/src/pure.rs:73-91`  
  BlockDef has public fields `input` and `output`, both documented as "Optional JSON schema describing the expected input/output shape" (lines 63-66). But `to_json()` only serializes id, name, version, description, and runtime — it never inserts `input` or `output`. So a block author who populates those fields gets them silently dropped from the WIT `info()` export. Either the fields are dead and the doc is misleading, or the serialization is incomplete data-loss; either way the code and the public-API docs disagree.  
  *Fix:* Serialize the two missing fields (insert `input`/`output` when `Some`, mirroring the `description` arm) so the public fields and their docs match the actual JSON output; or, if pure blocks genuinely have no input/output schema concept, remove the fields and their doc comments.

### Unsafe documentation (ch9-pointers)

- **M30. unsafe Send/Sync on ContextWrapper lacks SAFETY comment** — `crates/wafer-run/src/wasm/host.rs:46-48`  
  `struct ContextWrapper(*const dyn Context)` holds a raw pointer and gets `unsafe impl Send for ContextWrapper {}` / `unsafe impl Sync for ContextWrapper {}` with no `// SAFETY:` justification at the impl site. This is the most safety-critical unsafe in the file: the wrapper transmutes a non-'static `&dyn Context` and the Send/Sync assertion lets that raw pointer cross threads. The Drop strong-count assertion and `clone_arc` panic document the lifetime hazard, but the thread-safety claim itself (why it is sound to treat `*const dyn Context` as Send+Sync — i.e. the underlying Context is itself Send+Sync and the guard bounds its lifetime) is undocumented. By contrast WasmiBlock's analogous `unsafe impl` (wasmi_loader/mod.rs:254-257) does carry a justification.  
  *Fix:* Add a `// SAFETY:` comment above the `unsafe impl Send/Sync for ContextWrapper` stating the invariant: the pointed-to `dyn Context` is itself Send+Sync, the ContextGuard transmute only extends the lifetime (not relaxes thread bounds), and the strong-count assertion in ContextGuard::drop bounds the pointer's validity window.

### Missing docs (ch8-docs)

- **M31. wasm32 add_registrar/add_config_expander lack docs** — `crates/wafer-run/src/runtime/registry.rs:24-33, 118-127`  
  The `#[cfg(target_arch = "wasm32")]` variants of `add_registrar` (24-33) and `add_config_expander` (118-127) are `pub` but carry no doc comment, while their `#[cfg(not(target_arch = "wasm32"))]` siblings (14-22, 108-116) are documented. The crate enforces `#![warn(missing_docs)]` (lib.rs:7) and wasm32 is a real build target here (solobase-cloudflare / browser WASM), so these public items emit missing_docs warnings whenever the crate is compiled for wasm32 — a target the rest of the codebase actively ships.  
  *Fix:* Add a doc comment to each wasm32-cfg'd variant (or hoist the existing doc onto a shared `#[doc]` so both cfg arms inherit it). Simplest: copy the native variant's `///` doc onto the wasm32 variant.

---

## LOW severity (76)

Cosmetic idioms, minor allocations off the hot path, doc nits, and swallowed-error patterns that are unlikely-to-fire. Grouped by file.

| # | File:lines | Chapter | Finding |
|--:|---|---|---|
| L1 | `crates/wafer-block-config/src/toml.rs:71-88` | ch3-perf | linear scan over ENV_ALIASES on every config get |
| L2 | `crates/wafer-block-config/src/toml.rs:169-174` | ch4-errors | toml array values silently dropped on serialize failure |
| L3 | `crates/wafer-block-config/src/toml.rs:187-332` | ch5-tests | test names use test_ prefix instead of behaviour sentences |
| L4 | `crates/wafer-block-crypto/src/service.rs:178` | ch3-perf | derive_block_key allocates a String per byte for hex encoding |
| L5 | `crates/wafer-block-http-listener/src/lib.rs:128-143` | ch1-idioms | apply_response_meta match uses redundant guard arms instead of literal patterns |
| L6 | `crates/wafer-block-inspector/src/lib.rs:148-150` | ch4-errors | inspector flow_introspection expect on request path |
| L7 | `crates/wafer-block-inspector/src/lib.rs:32-34, 91, 256-269` | ch9-pointers | inspector policy uses RwLock where OnceLock fits |
| L8 | `crates/wafer-block-inspector/src/lib.rs:132, 141` | ch1-borrow | inspector needless to_string of action and path |
| L9 | `crates/wafer-block-local-storage/src/service.rs:142-147` | ch4-errors | local-storage get/delete probe unvalidated path before validate_path |
| L10 | `crates/wafer-block-logger/src/service.rs:15-45` | ch1-idioms | logger emits trailing space when fields empty |
| L11 | `crates/wafer-block-monitoring/src/lib.rs:180-185, 194-197` | ch8-comments | monitoring top_paths field name misdescribes contents |
| L12 | `crates/wafer-block-postgres/src/lib.rs:79-87` | ch4-errors | postgres handle() panics if dispatched before lifecycle Init |
| L13 | `crates/wafer-block-readonly-guard/src/lib.rs:63-67` | ch1-borrow | readonly-guard needless to_string before &str compare |
| L14 | `crates/wafer-block-sqlite/src/lib.rs:28-29` | ch8-comments | ensure_vec_loaded carries a dead _conn parameter |
| L15 | `crates/wafer-block-sqlite/src/vector.rs:393-396` | ch1-borrow | apply_filter takes &Option<T> instead of Option<&T> |
| L16 | `crates/wafer-block/src/config.rs:84-107` | ch4-errors | parse_duration multiply can overflow u64 on config input |
| L17 | `crates/wafer-block/src/error.rs:59-118` | ch4-errors | RuntimeError carries six stringly-typed payload variants |
| L18 | `crates/wafer-block/src/hash.rs:48-71` | ch4-errors | expand_env_vars silently drops undefined variable references |
| L19 | `crates/wafer-block/src/helpers.rs:28-36` | ch3-perf | MessageExt::header allocates two format! strings per lookup on request path |
| L20 | `crates/wafer-block/src/streams/output.rs:337-401` | ch4-errors | TerminalNotResponse is ad-hoc enum without thiserror despite being an error |
| L21 | `crates/wafer-block/src/streams/output.rs:531-538` | ch4-errors | OutputStream::from_producer panics on wasm32-wasi |
| L22 | `crates/wafer-block/src/types.rs:736-790` | ch1-idioms | Builder/constructor methods take &str then immediately allocate |
| L23 | `crates/wafer-cli/src/build.rs:153-155` | ch4-errors | build_go uses out.to_str().unwrap() — panics on non-UTF8 path |
| L24 | `crates/wafer-cli/src/build.rs:135-136` | ch4-errors | build_rust/build_go: out.parent().unwrap() on always-present parent |
| L25 | `crates/wafer-cli/src/build.rs:213-215` | ch4-errors | check_wafer_lock_sync labels a reachable load failure 'unreachable' |
| L26 | `crates/wafer-cli/src/commands/info.rs:73-103` | ch1-idioms | push_str(&format!) intermediate allocation in render_package |
| L27 | `crates/wafer-cli/src/commands/info.rs:61-64` | ch3-perf | render_package clones entire versions vec to sort |
| L28 | `crates/wafer-cli/src/commands/search.rs:70-84` | ch3-perf | truncate allocates full Vec<char> for every summary cell |
| L29 | `crates/wafer-cli/src/detect.rs:13-20` | ch1-idioms | detect.rs Lang::from_str shadows std FromStr without the trait |
| L30 | `crates/wafer-cli/src/install.rs:35-43` | ch8-docs | InstallOutcome public fields lack doc comments |
| L31 | `crates/wafer-cli/src/lockfile.rs:90-98` | ch3-perf | to_toml_string clones+re-sorts despite insert_or_replace maintaining order |
| L32 | `crates/wafer-cli/src/manifest.rs:11-26` | ch8-docs | Manifest public fields lack doc comments |
| L33 | `crates/wafer-cli/src/manifest.rs:41-50` | ch1-idioms | Duplicated org/block name-validation logic across manifest and scaffold |
| L34 | `crates/wafer-cli/src/registry_client.rs:219-234` | ch4-errors | client builder expect() on reqwest build |
| L35 | `crates/wafer-cli/src/test_runner.rs:137-144` | ch4-errors | guest-controlled name_len drives unbounded vec allocation in call_block stub |
| L36 | `crates/wafer-core/src/clients/logger.rs:104-112` | ch4-errors | WASM logger client silently drops call failures (asymmetric with native) |
| L37 | `crates/wafer-core/src/clients/logger.rs:100-112` | ch8-comments | WASM logger header comment stale vs body (no runtime::log fallback) |
| L38 | `crates/wafer-core/src/clients/network.rs:58-71` | ch3-perf | do_request clones headers map on hot request path |
| L39 | `crates/wafer-core/src/clients/network.rs:156-210` | ch1-idioms | network NetworkResponseStream lacks duplication note while llm/image share |
| L40 | `crates/wafer-core/src/discovery.rs:27-44` | ch3-perf | extract_params required-lookup is O(n) per property |
| L41 | `crates/wafer-core/src/interfaces/auth/handler.rs:31-36` | ch8-comments | Role round-trips as bare magic strings with asymmetric casing |
| L42 | `crates/wafer-core/src/interfaces/config/handler.rs:27-39` | ch4-errors | config.get swallows decode error, misreports malformed body |
| L43 | `crates/wafer-core/src/interfaces/database/handler.rs:245-266` | ch8-comments | exec_raw used to run DATABASE_EXECUTE bypasses ensure_query_columns |
| L44 | `crates/wafer-core/src/interfaces/database/service.rs:94,142,167` | ch8-comments | Magic 10000 row cap duplicated and inconsistently formatted |
| L45 | `crates/wafer-core/src/interfaces/database/service.rs:88-107` | ch1-borrow | delete_where default re-lists full match set each loop |
| L46 | `crates/wafer-core/src/interfaces/llm/router.rs:22-48` | ch6-dispatch | MultiBackend routers store Vec of Box/Arc dyn instead of generic registry |
| L47 | `crates/wafer-core/src/interfaces/network/handler.rs:41-49` | ch4-errors | network handler error context uses Display of codec error |
| L48 | `crates/wafer-core/src/interfaces/vector/handler.rs:189-194` | ch3-perf | vector embed loses model identity in service-level vectors |
| L49 | `crates/wafer-core/src/interfaces/vector/rrf.rs:24-31` | ch4-errors | rrf fuse sort partial_cmp swallows NaN ordering |
| L50 | `crates/wafer-ffi/src/lib.rs:106-110` | ch1-idioms | meta map clones owned key/value instead of into_iter |
| L51 | `crates/wafer-flow/src/accumulator.rs:88-107` | ch3-perf | Accumulator::resolve_input rebuilds Maps/Vecs even when no $. references exist |
| L52 | `crates/wafer-flow/src/expr.rs:110-136` | ch4-errors | Escape-aware quote scanner mis-detects closing quote after escaped backslash |
| L53 | `crates/wafer-flow/src/expr.rs:291-302` | ch8-comments | Dead unreachable branch in find_operator for single-char '=' operator |
| L54 | `crates/wafer-run-node/src/lib.rs:166-171` | ch1-idioms | meta map clones owned key/value (node) |
| L55 | `crates/wafer-run/src/discovery.rs:24-44` | ch4-errors | discovery scan helpers unbounded recursion on deep/symlinked trees |
| L56 | `crates/wafer-run/src/observability.rs:64-73` | ch8-comments | ObservabilityBus::new lacks doc-test / Default explanation parity |
| L57 | `crates/wafer-run/src/registry_loader.rs:143-155` | ch4-errors | parse_lockfile maps non-NotFound IO error to CacheMiss with empty name/version |
| L58 | `crates/wafer-run/src/registry_loader.rs:89-93` | ch4-errors | LockLoaderError -> RuntimeError stringifies and drops the typed source chain |
| L59 | `crates/wafer-run/src/runtime/lifecycle.rs:89-102` | ch8-comments | stale Network/Storage rejection message excludes Crypto |
| L60 | `crates/wafer-run/src/runtime/resolver.rs:186-213` | ch1-idioms | deeply nested if-let ladder in seal capability resolution |
| L61 | `crates/wafer-run/src/waferflow/executor.rs:204-208` | ch4-errors | pipeline output deserialize swallows error to Null silently |
| L62 | `crates/wafer-run/src/wasm/wasmi_loader/mod.rs:345-370` | ch3-perf | handle ABI JSON-encodes binary body as integer array |
| L63 | `crates/wafer-run/src/wasm/wasmi_loader/mod.rs:467-471` | ch8-comments | stale comments reference nonexistent call_block host import |
| L64 | `crates/wafer-run/src/wasm/wasmi_loader/mod.rs:437-450` | ch4-errors | guest Error action drops error code path detail vs Continue default kind |
| L65 | `crates/wafer-run/src/wasm/wasmi_loader/mod.rs:727-751` | ch3-perf | warn-once helpers call info() which can re-instantiate the module |
| L66 | `crates/wafer-schema/src/manifest/to_schema.rs:110-121` | ch4-errors | out-of-range JSON number defaults silently to 0 |
| L67 | `crates/wafer-schema/src/manifest/to_schema.rs:71-72` | ch1-idioms | splitn+Vec+index for two-part ref |
| L68 | `crates/wafer-schema/src/manifest/to_schema.rs:9-16` | ch1-idioms | HashMap re-indexed by key in collections_to_tables |
| L69 | `crates/wafer-schema/src/types.rs:209-219` | ch8-comments-docs | not_null builder is a no-op duplicating default state |
| L70 | `crates/wafer-sql-utils/src/value.rs:9-25` | ch4-errors | sea_value_to_json catch-all silently maps unhandled variants to Null |
| L71 | `crates/wafer-sql-utils/src/vector.rs:54-57, 115-250` | ch8-comments | ddl_stmt helper name misleads — used for SELECT/INSERT/UPDATE/DELETE, not just DDL |
| L72 | `crates/wafer-test-support/src/fake_crypto.rs:280-335` | ch5-tests | fake_crypto verify_fails_on_wrong_secret asserts many unrelated things via setup |
| L73 | `crates/wafer-test-support/src/fake_db.rs:188-197` | ch1-idioms | fake_db handle_list filters rows twice |
| L74 | `examples/api-server/src/main.rs:133-157` | ch1-idioms | api-server NotesHandler builds create payload with HashMap then clones |
| L75 | `examples/wasmi-block/src/lib.rs:13-19` | ch4-errors | wasmi-block guest handle unwraps serde_json::to_vec |
| L76 | `sdks/rust/src/stream.rs:54-61` | ch9-pointers | Unsafe FFI host-import calls lack // SAFETY: comments (inconsistent within file) |

---

## Appendix — `clippy -W pedantic -W nursery` (advisory, ~1,900 hits)

These tiers are **deliberately not enabled** in the workspace; listed for completeness. Overwhelmingly cosmetic. Top lints:

| Lint | Count | Note |
|---|--:|---|
| `doc_markdown` | 63 | backtick identifiers in doc comments |
| `missing_const_for_fn` | 29 | fns that could be `const` |
| `redundant_closure_for_method_calls` | 22 | `|x| x.foo()` → `T::foo` |
| `wildcard_imports` | 19 | `use foo::*` |
| `must_use_candidate` | 18 | add `#[must_use]` |
| `map_unwrap_or` | 16 | ⟵ **handbook-relevant** (ch1/ch2) — `.map().unwrap_or()` → `.map_or()` |
| `missing_errors_doc` | 14 | add `# Errors` section |
| `option_if_let_else` | 13 | ⟵ handbook-relevant (ch1.3) — prefer `map_or_else`/`let-else` |
| `cast_* (truncation/wrap/sign/precision/lossless)` | 60 | numeric cast audits |
| `significant_drop_tightening` | 7 | ⟵ **ch9-relevant** — lock guard lives longer than needed; reviewers hand-checked all 7, none cross an await |
| `needless_pass_by_value` | 8 | ⟵ handbook-relevant (ch1.1) — take `&T` |
| `match_same_arms` | 10 | merge identical arms |

Per-crate advisory density: wafer-block 558 · wafer-run 397 · wafer-core 251 · wafer-cli 170 · wafer-sql-utils 89 · wafer-schema 55 · (rest <45 each).

**Recommendation:** the four handbook-relevant lints (`map_unwrap_or`, `option_if_let_else`, `needless_pass_by_value`, `significant_drop_tightening`) are worth promoting to `warn` in `[workspace.lints.clippy]` and fixing; the rest (doc_markdown, casts, must_use) are noise for this project's posture.
