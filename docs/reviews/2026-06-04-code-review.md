# wafer-run Code Review — 2026-06-04

Exhaustive multi-agent review of the wafer-run monorepo (~62k LOC, 30 Rust crates).
61 findings confirmed after adversarial verification; 30 candidate findings dropped as
false positives. This doc is both the findings record and the implementation tracker.

**Conventions referenced:** root-cause fixes only; no compat shims; no magic code /
implicit mapping layers; no raw SQL in block code (use `wafer-sql-utils`); no hardcoded
domain values (use `ConfigVar`); branch + PR per change; subagent task scope (split by
concern).

Severity legend: **critical** = correctness/security/data-loss · **high** = will bite
soon / clear design defect · **medium** = real but contained · **low** = nit/polish.

---

## Proposed PRs (implementation grouping)

Each PR is scoped to one concern / file-cluster so subagents don't conflict.

| PR | Concern | Crates touched | Severity | Status |
|----|---------|----------------|----------|--------|
| 1  | `dispatch_call` name resolution + resource-type enum + magic-string consts | wafer-run, wafer-block | critical | ☐ |
| 2  | wasmi context cleanup → RAII | wafer-run | critical | ☐ |
| 3  | `OutputSink` invariants enforced in release | wafer-block | high | ☐ |
| 4  | LLM error → `ErrorCode` mapping + router log parity | wafer-core | high | ☐ |
| 5  | `BlockInfo`/`ConfigVar` validation (reserved prefix, builder type) | wafer-block | high | ☐ |
| 6  | `ddl::build_add_text_column` + drop raw ALTER fallback | wafer-sql-utils, sqlite, postgres | high | ☐ |
| 7  | CLI publish-response validation | wafer-cli | high | ☐ |
| 8  | IP-rate-limit single source of truth + value fix | wafer-block-ip-rate-limit | high | ☐ |
| 9  | WASI stubs return `WASI_ERRNO_FAULT` on write failure | wafer-run | medium | ☐ |
| 10 | small-block consistency (`RwLock`→`OnceLock`, hardcoded→`ConfigVar`) | cors, security-headers, monitoring, http-listener, s3 | medium | ☐ |
| 11 | SSRF security extracted to shared module; drop block→runtime dep | wafer-run, wafer-core, wafer-block-network | medium | ☐ |
| 12 | waferflow executor through accessor + shared `lookup_block` | wafer-run | medium | ☐ |
| 13 | model-management dedup (shared `model_common` + generic router) | wafer-core | medium | ☐ |
| 14 | WRAP block-id naming validated at registration | wafer-block, wafer-run | medium | ☐ |
| 15 | FFI JSON escaping; postgres ident sanitize; credentials test `env_guard` | wafer-ffi, wafer-block-postgres, wafer-cli | medium | ☐ |
| 16 | polish batch (macro validation, flow-expr escapes, accumulator, docs, sign-check, helpers) | wafer-block-macro, wafer-flow, wafer-run, wafer-block | low | ☐ |
| —  | `common/codegen` cleanup-or-wire-up | common/ | high | ☐ (re-audit first; deferred) |
| —  | wasm-component `call_service` (TODO #103) | wafer-core | high | ☐ (large; design needed) |

---

## Architecture smells

### A1. Fragmented name resolution in `dispatch_call` (security attribution) — **critical** · PR 1
`canonicalize(block_name)` is computed 3× under 3 names; the *unresolved alias* leaks into
security-critical paths.
- `context.rs:165` `resolved_name` (used only for `requires`)
- `context.rs:188-191` WRAP caller attribution uses `self.node_id`
- `context.rs:250` `resolved_block_name` (used only for lookup)
- `context.rs:341` sub-context `node_id` set from raw `block_name`
**Fix:** resolve once at top; reuse for WRAP caller/check, capability checks, lookup, and
`sub_ctx.node_id`. Invariant already documented at `runner.rs:103-109`.

### A2. `wafer-block-network` → `wafer-run` reverse dependency — **medium** · PR 11
`wafer-block-network/Cargo.toml:9` depends on `wafer-run` for
`wafer_run::security::{is_blocked_ip,is_blocked_url}` (`src/service.rs`). Blocks must not
depend on the runtime.
**Fix:** move SSRF predicates from `wafer-run/src/security.rs` into a shared location
(`wafer-core::security` or new `wafer-security`); drop the `wafer-run` dep.

### A3. Orphaned `common/codegen` output — **high** · deferred (re-audit first)
`common/codegen/generate.py` emits rust into `common/generated/rust/` that nothing
`include!`s; hand-coded `wafer-block/src/common/service_names.rs` is already ahead.
**Fix:** wire up (via `include!`, brought to parity) or delete pipeline + `generated/` and
note in `CLAUDE.md`. Workspace `CLAUDE.md` tracks this as deferred — re-audit first.

### A4. waferflow executor bypasses `RegistrationCore` — **medium** · PR 12
`waferflow/executor.rs:143-146` reads `wafer.registration.all_blocks` directly despite
`runtime.rs:286` `all_blocks_arc()`. Decomposition leftover.
**Fix:** route through accessor; fold canonicalize-then-fallback lookup (dup of
`context.rs:250-254`) into one `Wafer::lookup_block`.

### A5. Duplicate model-management types (LLM vs Image) — **medium** · PR 13
`interfaces/llm/service.rs:428-569` and `interfaces/image/service.rs` define
field-identical `ModelInfo`/`ModelStatus`/`ModelState`/`LoadProgress`;
`MultiBackend{Llm,Image}Service` reimplement the same router.
**Fix:** `interfaces::model_common` for shared types + generic
`MultiBackendRouter<S: BackendService>`; keep capability structs specialized.

### A6. WRAP block-id naming assumed, never enforced — **medium** · PR 14
`wafer-block/src/wrap.rs:9-33` roundtrips `__`↔`/` and `_`↔`-` unconditionally; an
underscore in a name breaks the roundtrip → bypass/deny.
**Fix:** validate `^[a-z0-9]+(?:-[a-z0-9]+)*$` per segment at registration; document the
invariant.

### A7. (low) `RuntimeError` mixes boot/registration/operational domains (`error.rs:6-115`) —
add an enum-level doc comment; no code change. · PR 16
### A8. (low) No wire envelope/version negotiation — strategy is sound
(`to_vec_named` + `#[serde(default)]`); document only. · PR 16

---

## Code smells

### C1. `dispatch_call` security checks use the alias — **critical/high** · PR 1
- `context.rs:341` `node_id: block_name.to_string()` → should be `resolved_block_name`
  (caller identity for downstream WRAP). **critical**
- `context.rs:210` `caps.allows_call_block(block_name)` — `allows_call_block`
  (`capabilities.rs:181`) matches canonical names from registration
  (`wafer-block-macro/src/lib.rs:657`) → alias = false denial. Pass `resolved_block_name`.
  **high**
- `context.rs:165` & `:250` duplicate `canonicalize` → collapse to one. **low**

### C2. wasmi resume loop leaks context guard (panic-on-drop) — **critical** · PR 2
`wasm/wasmi_loader/mod.rs` installs context at `:441`; `ContextGuard::drop`
(`host.rs:31-44`) asserts `strong_count == 1`. Error returns at `:444`, `:448`, `:453`,
`:554-559`, `:600-605`, `:642-644`, `:647`, and the trap `else` `:655-660` don't clear it →
host panic on a guest contract violation.
**Fix:** RAII scope guard that clears `store.data_mut().context` on drop, covering every
exit path.

### C3. `OutputSink` invariants compiled out of release — **high** · PR 3
`wafer-block/src/streams/output.rs` enforces "terminal can't follow Chunk/Meta" only under
`#[cfg(debug_assertions)]` (`any_body_sent` `:33-34,:42-43,:53-54`; asserts
`:81-87,:97-103`). Release silently allows protocol violations (`:761-767`).
**Fix:** make `any_body_sent` unconditional (or return `Result` from terminal methods).
**Related (medium):** `Drop` auto-complete (`:128-134`) and `Halt`-discards-chunks
(`:370-380`) should `debug_assert!`/`warn!` on the bug-shaped case.

### C4. LLM errors flattened to `INTERNAL` — **high** · PR 4
`interfaces/llm/handler.rs` maps every `LlmError` to `INTERNAL` in `list_models`/`status`/
`unload_model` (`:352-401`) and `chat`/`load_model` (`:258-267,:306-316`). Image already
maps properly (`image/handler.rs:102-113`).
**Fix:** `llm_error_to_block_error` (RateLimited→UNAVAILABLE, Unauthorized→UNAUTHENTICATED,
NotSupported→UNIMPLEMENTED, Cancelled→CANCELLED, Network→INTERNAL); use at all 5 sites.
**Related (medium):** router list_models log parity — LLM logs (`router.rs:86`), image
swallows (`image/router.rs:69-79`).

### C5. "No magic code" violations — **high/medium** · PR 1
- `context.rs:221-235` hardcoded resource-type strings vs `ResourceType::parse` at `:186`
  (same fn). **high** — parse once, match variants.
- `__raw_sql__`/`__ddl__` hardcoded `context.rs:223,225` + `wrap.rs:129,146`. **medium** —
  named consts in `wafer-block`.
- `"trace_id"` hardcoded `waferflow/executor.rs:172`, `runtime/runner.rs:117`. **medium** —
  add `META_TRACE_ID`.
- `SOLOBASE_SHARED__` hardcoded `types.rs:759` + `wrap.rs:163`. **medium** — one
  `SOLOBASE_SHARED_PREFIX` const.

### C6. `ConfigVar`/`BlockInfo` validation gaps — **high/low** · PR 5
- Reserved `SOLOBASE_SHARED__` `ConfigVar`s not rejected in `BlockInfo::new`
  (`types.rs:303-335`; `is_deletable` `:756-760`). **high**
- `config_keys()` builder typed `Vec<BlockConfigKey>` but field is `Vec<ConfigVar>`
  (`types.rs:363-365`, alias `:770`). **low** — use `ConfigVar`, drop alias.

### C7. WASI stubs swallow `memory.write` failures — **medium** · PR 9
`wasm/wasmi_loader/imports.rs` `let _ =` + errno 0: `fd_write` `:443-445`,
`environ/args_sizes_get` `:468-471,:495-498`, `clock_time_get` `:545-546`, `random_get`
`:567`. `random_get` → non-random seed material on bad pointer.
**Fix:** check write result, return `WASI_ERRNO_FAULT` (pattern at `:563-565`).

### C8. Smaller code smells — **low/medium** · PR 15/16
- `unpack_ptr_len` no sign-bit check (`abi.rs:30-33`) → negative sentinel = garbage to
  `read_guest_bytes`. **low**
- Unneeded `Arc::clone` + owned-body move `service_blocks/image.rs:42`, `llm.rs:42`. **low**
- CLI `set_var`/`remove_var` on `HOME` unsynced in tests (`credentials.rs:78-80,143,147`);
  reuse `cache.rs:160-165` `env_guard()`. **medium**
- `std::sync::Mutex` for `warned_unknown_interfaces` (`context.rs:61`) → `parking_lot`. **low**
- `registry_client.rs:281` `text().await.unwrap_or_default()` swallows body errors. **low**
- Postgres `ensure_columns_*` ALTER trusts caller sanitization (`service.rs:264-265,289-290`)
  → move `sanitize_ident(table)` inside. **medium**
- SQLite PRAGMA quoting inconsistent (`service.rs:145` vs `:159`) → unify. **low**

---

## Room for improvement

### R1. WASM typed-service-client path stubbed (TODO #103) — **high** · deferred (design)
`wafer-core/src/clients/mod.rs:155-170,:254-269` error on wasm-component
`call_service`/`call_service_with_msg`. WASM blocks can't use any typed service client.
**Fix:** implement `call_block` ABI host import + wire `call_service` (mirror native path),
streaming-frame handling on resumable calls; or document as a hard blocker.

### R2. Lazy `ALTER TABLE ADD COLUMN ... TEXT` raw SQL in both backends — **high** · PR 6
`wafer-block-sqlite/src/service.rs:135,197,204` + `wafer-block-postgres/src/service.rs:264-268,289-293,302-305`.
Builder-API gap (`ddl::build_add_column` needs a full `Column`).
**Fix:** add `ddl::build_add_text_column(table,column,backend)` to `wafer-sql-utils`; use in
both backends.

### R3. Publish response unvalidated — **high** · PR 7
`wafer-cli/src/commands/publish.rs:76-84` `as_str().unwrap_or_default()` after 2xx → empty
strings on malformed response.
**Fix:** typed `PublishResponse` with non-optional fields; fail clearly.

### R4. Small-block consistency drift — **high/medium/low** · PR 8/10
- IP-rate-limit split source: struct defaults (1000/60) disagree with `flow_config`
  (`'60'`/`'60'`) and two read paths (`ip-rate-limit/src/lib.rs:96-97,115-130,151-163`).
  **high** — single source in `flow_config`, fix mismatch. · PR 8
- `RwLock` write-once config → `OnceLock`: cors `lib.rs:44`, security-headers `:37`,
  monitoring `:39`. **medium** · PR 10
- Hardcoded → `ConfigVar`: CORS `max_age=86400` (`cors/lib.rs:70,178`), http-listener
  `MAX_BODY_SIZE=10MiB` (`http-listener/lib.rs:447`). **medium** · PR 10
- S3 const-vs-config dup (`s3/lib.rs:18-19` vs `:62,82`) — reference consts in `ConfigVar`
  defaults. **low** · PR 10
- Nine infra blocks repeat `.instance_mode(Singleton).category(Infrastructure)` → add
  `BlockInfo::infrastructure(...)`. Mixed `if let LifecycleType::Init =` vs `==` → unify.
  **low** · PR 16

### R5. Polish — **high(narrow)/medium/low** · PR 15/16
- FFI hand-escape only `"` (`wafer-ffi/src/lib.rs:443-450`) → invalid JSON on error path;
  use `serde_json` like `:87-98`. **high (narrow path)** · PR 15
- Flow expr string literals don't process escapes (`wafer-flow/src/expr.rs:319-321`).
  **medium** · PR 16
- Macro: no mutual-exclusivity check `capabilities(...)`+`skill(...)` (`lib.rs:356-366`); no
  2-segment name validation (`:533-544`) → compile-time `syn::Error`s. **medium/low** · PR 16
- Accumulator O(n) path rebuild (`wafer-flow/src/accumulator.rs:60-65`). **low** · PR 16
- `apply_config_overrides` empty-allowlist = deny-all undocumented (`capabilities.rs:213-223`).
  **low** · PR 16

---

## Verified-clean (false positives dropped)
Notable plausible-but-wrong findings the code already handles: sync-vs-async handler split,
granular per-operation crypto WRAP resources, the `libc::kill` SAFETY block, 16 MiB WASM
page math, `from_events` compile-time `assert!(N>=1)`, LLM wire-to-service "duplication"
(inherent to the message model), DbExec dedup (#188 complete). 30 total dropped.
