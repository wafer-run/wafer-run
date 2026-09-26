# Changelog

## Unreleased

### Breaking changes

- Every `CryptoService` method is `async` (`hash`, `compare_hash`,
  `sign_for`, `verify_for`, `random_bytes`), and the crypto block awaits
  each call on every target. On wasm32 the block used to call the service
  synchronously, so a backend that answers only after awaiting something —
  password hashing in a Cloudflare Durable Object, a signing key held by a
  KMS — could not be written without blocking. An implementation adds
  `#[wafer_core::wafer_async_trait]` (the implementing crate needs
  `async-trait` as a dependency, as for the other service traits) and makes
  each method `async fn`; a caller of a method adds `.await`.
  `interfaces::crypto::handler::handle_message` is now `async`, and
  `handle_message_native` is removed: moving Argon2 off the executor thread
  is the service's job, not the handler's. `Argon2JwtCryptoService` hashes
  and verifies passwords on a dedicated thread on native hosts (behind the
  same semaphore as before, the result awaited over a oneshot channel, so
  any executor can drive it — it no longer needs a Tokio runtime) and
  inline on wasm32; `wafer-block-crypto` gains `tokio` (for its `Semaphore`)
  and `futures` on native, and `wafer-core` drops its direct native `tokio` dependency.
- A block's init can have a time limit, which the block declares. The new
  `BlockInfo::init_timeout(Duration)` (field `init_timeout_ms`) sets the
  longest one attempt at the block's init — loading its declared config and
  its `lifecycle(Init)` — may run; `WaferBuilder::init_timeout(Duration)`
  lets an embedder cap every block's budget (the smaller of the two
  applies). Neither is set by default, so an Init without a declared budget
  in a runtime without a cap still runs as long as it takes: only the block
  knows how long its Init may legitimately take (a migration over a large
  table), and an attempt cut short is retried from the beginning. An
  attempt still running when its budget passes is dropped, and one that
  fails after its deadline has passed (whatever error it returns) failed
  because of it; either way the outcome is `InitError::Transient` naming
  the block, the budget and who set it, and noting that work the attempt
  started outside the runtime (a database statement, an outbound request)
  may still be running. It is retried after the init backoff. The Init
  context carries the attempt's deadline, and the time of a callee's Init
  that runs inside the attempt counts against it; a callee attempt dropped
  with it records no outcome. The timer is `futures-timer` on native hosts
  (any executor) and the host's global `setTimeout` on wasm32 (a browser, a
  Cloudflare Workers isolate): `wafer-run` gains `futures-timer` on native
  and `wasm-bindgen`, `js-sys` and `wasm-bindgen-futures` on wasm32.
  The budget runs from when the attempt starts, so an Init that blocks its
  thread before its first `.await` is still cut off on time. A declared
  budget under 1 ms is refused at registration with the new
  `BlockInfoError::ZeroInitTimeout`, and a zero cap by
  `WaferBuilder::build`: either would time every Init out at once.
  `BlockInfo` is not `#[non_exhaustive]`, so a struct literal outside this
  workspace needs the new field (`BlockInfo::new` does not), and an
  exhaustive `match` on `BlockInfoError` needs the new arm.
- `call_block` from a context whose deadline has passed answers
  `DeadlineExceeded` (it answered `Cancelled`, the code for a cancelled
  flow). A context whose flag was cancelled still answers `Cancelled`.
- The 500 that answers a terminal holding unsendable response meta now
  keeps the terminal's security headers. `http_codec::unsendable_response`
  takes the terminal's meta: `unsendable_response(meta, invalid)`, `meta`
  being the slice `invalid` came from. Its 500 carries the headers of that
  meta the codec can send (`Content-Security-Policy`, `X-Frame-Options`,
  `X-Content-Type-Options`, CORS headers, …) — for each name the last
  value that can be sent, a refused entry displacing nothing — plus
  `Cache-Control: no-store`. It skips the terminal's status, content type,
  `Set-Cookie` directives, `Cache-Control` and `Expires`, and the headers
  describing the body it replaces: the new
  `http_codec::BODY_RESPONSE_HEADERS` (`Content-Encoding`,
  `Content-Disposition`, `ETag`, `Last-Modified`, `Location`, …), the set
  the flow executor already kept off a short-circuit terminal, now shared.
  The new `http_codec::unsendable_response_meta(meta)` returns those
  entries, and the embedder wire format's `Internal` error for such a
  terminal carries them as its `meta`. Before, the 500 carried only
  `Content-Type`, so a terminal that did not come through a flow — where the
  executor refuses the bad step and keeps the middleware's headers — was
  answered with no CSP, no `X-Frame-Options` and no `nosniff`. An adapter
  that calls `unsendable_response` itself passes the meta it classified.
- The fixed cap on `database.create_many` rows and `database.batch` ops is
  gone: `wire::database::MAX_BATCH_WRITES` (1000) is removed, and each
  backend now reports its own budget. `DatabaseService` and `DbExec` gain a
  required `fn statement_budget(&self) -> Result<StatementBudget,
  DatabaseError>` (no default: a decorator that forgot it would hide its
  backend's limit), reporting `StatementBudget::Unbounded` or
  `Limited { limit, used }` — the statements one invocation may run and how
  many this one already has; an error is the call's answer. The
  database handler admits a `create_many` (one statement per row) or `batch`
  (one per op) against it before anything runs, and the shared `DbExec`
  orchestration admits every `run_transaction` again after planning, so the
  introspection a write ran while planning counts too. A call over the whole
  limit is `InvalidArgument`; one that fits the limit but not what the
  invocation has left is the new `DatabaseError::ResourceExhausted`
  (`ErrorCode::ResourceExhausted`, HTTP 429), and nothing is written.
  Native SQLite and PostgreSQL report `Unbounded` and now take a call of any
  size (before, anything over 1000 was refused); a Cloudflare D1 adapter
  reports D1's per-invocation query limit (1000 on Workers Paid, 50 on Free)
  and the statements it has sent in this invocation, which the old global
  cap could not see — a `batch` of 1000 after earlier queries in the same
  request overflowed D1. Every `DatabaseService`/`DbExec` implementor must
  add the method; a `forward_database_service!` ledger needs
  `statement_budget: forward` (or `custom`) after `set_strict_schema`, and an
  exhaustive `match` on `DatabaseError` needs the new arm. The
  `database.create_many`/`database.batch` action schemas drop `maxItems`.
- A statement-budget refusal carries a detail code
  (`WaferError::detail_code`), so a caller can tell it from the other
  errors sharing its coarse code — any malformed request's
  `InvalidArgument`, or the `ResourceExhausted` of a rate limit or the
  call-depth limit. A write over the backend's whole per-invocation limit is
  the new `DatabaseError::StatementLimitExceeded` (still `InvalidArgument`
  on the wire), with detail code `database.statement_budget_exceeds_limit`:
  no invocation can run it, so send less. One that fits the limit but not
  what the invocation has left is `ResourceExhausted` with
  `database.statement_budget_exhausted`: retrying it in the same invocation
  fails the same way. The new `wire::database::STATEMENT_BUDGET_EXCEEDS_LIMIT`
  and `STATEMENT_BUDGET_EXHAUSTED` name the two codes, and
  `DatabaseError::detail_code()` returns them. The code reaches every
  caller: a native block, a wasm guest (both the SDK and the `wafer-core`
  clients decode the whole error), the HTTP error body's `code` field and
  the embedder JSON's `detail_code` (the C ABI, the Node addon). A backend
  refusing a write over its limit itself, instead of through
  `StatementBudget::admit`, must return `StatementLimitExceeded` (the
  conformance suite now requires it), and an exhaustive `match` on
  `DatabaseError` needs the new arm.
- `wire::database::BatchWrite` gains `DeleteWhere { collection, filters }`,
  `BatchWriteResult` gains `DeletedWhere { rows_affected }`, and the
  service-side twins `WriteOp` / `WriteOutcome` gain the same variants.
  None of the four is `#[non_exhaustive]`, so an exhaustive `match` on one
  outside this workspace needs the new arm. A `DatabaseService` that
  forwards `batch` to `DbExec::batch` needs no change.
- The `wafer-run/vector` block no longer carries an embedding service.
  `VectorBlock::new` and `service_blocks::vector::register_with` take only
  an `Arc<dyn VectorService>`, and the block's info no longer declares
  embedding grants. Its `embedding.embed` and `embedding.count_tokens`
  arms could not be reached: the block declares `vector@v1`, whose actions
  are the `vector.*` ops, so `call_block` refused every `embedding.*` call
  to it as `Unimplemented`. Embedding is served by an `embedding@v1` block
  through `interfaces::vector::handler::handle_embedding_message`, and
  callers name that block (`clients::vector::embed(ctx, block, texts)`).
  A host that registered the vector block with an embedding model no
  longer needs to load one for it: drop the second argument, and register
  an embedding block where it serves embeddings.
- `EmbeddingService::grants` is removed. Its only reader was the vector
  block's info. The `embedding@v1` block serving the service declares, in
  its own `BlockInfo::grants`, which blocks may call it; move any override
  of `grants()` there.
- A flow step can no longer loosen a security header an earlier step set.
  Where a responding or failing step (in the same flow or in a `next`
  transfer's target) sets its own value for one of these headers, the flow
  executor now sends a value at least as strict as each instead of the
  later one: `Content-Security-Policy` sends both policies,
  comma-separated, and a browser enforces each; `X-Frame-Options` keeps
  `DENY` over `SAMEORIGIN` over anything else; `X-Content-Type-Options`
  keeps `nosniff`; `Referrer-Policy` keeps the policy that sends less to
  another origin; `Strict-Transport-Security` takes the longer `max-age`,
  `includeSubDomains` if either value has it, and `preload` if a value with
  `includeSubDomains` has it; `Permissions-Policy` takes, per feature, the
  intersection of the two allowlists, a feature only one value names being
  held to `(self)` (member parameters such as `;report-to` are dropped).
  Before, the later value replaced the security-headers middleware's, so a
  handler's `X-Frame-Options: SAMEORIGIN` turned the site-wide `DENY` off
  for its response (a 401 across a flow transfer included). There is no
  per-route escape hatch: the `wafer-run/security-headers` config is read
  once at Init and applies site-wide, so a handler that relaxed framing or
  the CSP for one route must relax it for the site (`frame_ancestors`,
  `csp`) or not at all, and `Permissions-Policy`, `Referrer-Policy` and
  HSTS have no config knob — a handler can no longer enable, say, the
  camera for one route. Tightening still works: a handler's own sandboxing
  `Content-Security-Policy` or `Referrer-Policy: no-referrer` applies on top
  of the middleware's. Not combined, so the later value still replaces the
  earlier: `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy`
  (cross-origin isolation is a page's opt-in that a route may relax, e.g.
  to keep a handle on an OAuth or payment popup) and
  `Content-Security-Policy-Report-Only` (which enforces nothing).
- `wafer_core::security` is removed, and `wafer-core` no longer depends on
  `wafer-net-security`. The module only re-exported `is_blocked_ip`,
  `is_blocked_ipv4`, `is_blocked_ipv6` and `is_blocked_url` from
  `wafer-net-security`, where they are defined; import them from there
  (`wafer_net_security::is_blocked_url`) and add `wafer-net-security` to
  the crate's dependencies. The predicates' behaviour is unchanged.
- JWT claims are a `BTreeMap<String, serde_json::Value>` everywhere they
  were a `HashMap`: `wire::crypto::SignRequest::claims`,
  `wire::crypto::VerifyResponse::claims`, `CryptoService::sign_for` /
  `verify_for`, `wafer_core::clients::crypto::sign` / `verify`, and
  `wafer_block_crypto::primitives::jwt_sign` / `jwt_verify`. The signer now
  encodes the payload as canonical JSON, keys sorted at every depth, so
  equal claims signed in the same second with the same expiry by the same
  calling block (the same derived key) give byte-identical tokens;
  caller-supplied `iat`/`exp` do not count, as the signer overwrites both.
  Before, the payload followed a `HashMap`'s randomised iteration order:
  equal claims usually signed to different bytes and occasionally to the
  same bytes, so a caller that told tokens apart by their bytes worked by
  chance. A caller that needs two tokens to differ must now put a
  differing claim in them (a random `jti`). The msgpack wire format is
  unchanged (a map either way), and verification does not depend on key
  order, so tokens already issued keep verifying. The workspace's
  `serde_json` floor is 1.0.129, for `Value::sort_all_objects`.
- `wafer_schema::DefaultValue` is an enum — `Null`, `Now`, or
  `Value(DefaultVal)` — instead of a struct with `raw` / `is_raw` /
  `is_null` / `value` fields, so a column default can no longer carry a
  SQL expression the DDL builders splice in verbatim. The `default_*`
  helpers are unchanged; code that built the struct by hand switches to a
  variant, and code that set `is_raw` for `CURRENT_TIMESTAMP` uses
  `DefaultValue::Now`. `DefaultVal::Float` must be finite.
- `ddl::build_add_column_with_type` is private: it spliced its `type_sql`
  argument into the statement verbatim. Callers use
  `ddl::build_add_column_for_value`, or `ddl::build_add_column` with a
  `Column`.
- `wafer-block-postgres` is built on sqlx 0.9 (was 0.8), so
  `PostgresDatabaseService::from_pool` takes a sqlx 0.9 `PgPool`. An
  embedder that builds its own pool moves its `sqlx` dependency to 0.9;
  `PostgresDatabaseService::connect(url)` is unchanged. The move takes the
  workspace's own crypto onto the RustCrypto digest 0.11 line together
  (`sha2` 0.11, `hmac` 0.13, `hkdf` 0.13, `pbkdf2` 0.13): sqlx 0.8 pinned
  `hkdf` 0.12, so bumping `hkdf` alone would have linked two copies. Hash,
  HMAC, HKDF and PBKDF2 outputs are byte-identical (the SHA-256, RFC 4231
  HMAC, HKDF known-answer and PBKDF2 vector tests pass unchanged), so
  stored password hashes, tokens and derived keys stay valid.
- sqlx 0.9 changes how `PostgresDatabaseService::connect(url)` reads two
  connection settings, so an operator's existing configuration can mean
  something different:
  - A password read from a `.pgpass` file (or the file `PGPASSFILE` names)
    is now backslash-unescaped, as libpq does: `\:` is `:` and `\\` is `\`
    (sqlx #3993). A password containing a backslash that relied on sqlx
    0.8 sending it literally now sends a different password; write each
    literal `\` in the file as `\\`.
  - A `options[key]=value` URL parameter's value is now escaped
    automatically (a space becomes `\ ` and a backslash `\\`) before it is
    sent as `-c key=value` (sqlx #3800). A value hand-escaped for sqlx 0.8,
    such as `options[application_name]=my\ app`, is now escaped twice and
    reaches the server with the backslash in it; write the plain value
    (`my app`, URL-encoded as `my%20app`). The `PGOPTIONS` environment
    variable is still passed through unescaped.
- An `OutputSink` dropped without an explicit terminal now ends its stream
  with an `Error` terminal (`ErrorCode::Internal`, message `output stream
  ended without a terminal event: …`, naming a panic when the drop happens
  during one) instead of an automatic `Complete`. A producer that panicked,
  was cancelled or aborted (a runtime shutting down drops its tasks) or
  returned early mid-body used to hand every consumer a successful response
  carrying a truncated body — a `200` at the HTTP listener, a
  `{"action":"respond"}` through `output_to_json` (FFI, Node, Go). It now
  surfaces as a `500` / `{"action":"error"}` / `Err`. wafer-run's own
  transports (the HTTP listener, the FFI, Node and Go) buffer the body
  before sending anything, so on them no headers or bytes are on the wire
  when the error arrives. An embedder that streams the body after
  committing headers (such as a `ReadableStream`-backed Cloudflare Workers
  or browser adapter) now receives an `Error` terminal mid-body and must
  abort the response body on it, never close it cleanly. A producer that
  relied on the automatic `Complete` must end with
  `sink.complete(meta).await`: every `OutputStream::from_producer` closure
  and every `new_streaming` sink needs exactly one explicit terminal on each
  success path.
- `OutputStream::body_stream` is removed: it ended cleanly on an `Error`
  terminal and on a stream with no terminal, so a truncated body read as a
  whole one. Use `OutputStream::body_stream_or_error`, which now also yields
  a final `Err` for a stream that ends with no terminal
  (`TerminalNotResponse::Malformed`) and yields a `Halt` terminal's body
  instead of discarding it.
- `OutputSink::halt` returns `Result<(), SinkSendError>` and refuses to
  follow a `Chunk`/`Meta` event (`SinkSendError::BodyAlreadySent("Halt")`),
  as `drop_request` and `continue_with` already did; the refused sink's drop
  then ends the stream with an `Error`. A stream that carries a `Halt` after
  body events anyway (a non-sink source) is an error to both
  `collect_buffered` (`TerminalNotResponse::Error`; it used to discard the
  streamed chunks and return the `Halt`) and `body_stream_or_error` (a final
  `Err`).
- `wafer_block::codec::decode` returns the new `codec::DecodeError`
  instead of a `WaferError` whose code was always `Internal`. The caller
  now picks the code by who sent the body: `DecodeError::invalid_argument`
  for a request a caller sent, `DecodeError::internal` for a reply from a
  service or host the decoder relies on. There is no `From<DecodeError> for
  WaferError`, so `codec::decode(..)?` in a function returning `WaferError`
  no longer compiles; `e.code`/`e.message` become that choice and
  `e.to_string()` (which keeps the `codec decode error in <type>: <cause>`
  text). `DecodeError::type_name` and `DecodeError::cause` expose the parts;
  the cause is capped at `codec::MAX_DECODE_CAUSE_LEN` (256) bytes plus `…`,
  so a value serde echoes into its message cannot make the error unbounded.
- `wafer-test-support` no longer ships `FakeDb`, `FakeCrypto`,
  `fake_db::FailureMode`, `WaferBuilder::with_fake_db` or
  `WaferBuilder::with_fake_crypto`. The
  fakes parsed JSON request bodies while every production client encodes
  MessagePack (`wafer_block::codec`), so a test routing
  `wafer_core::clients::database::*` to them failed with `fake-db: bad
  request`, and nothing used them. A test that needs a database registers
  the real handler over an in-memory SQLite service
  (`wafer_core::service_blocks::database::register_with_tables` with
  `SQLiteDatabaseService::open_in_memory`); `WaferBuilder` stays.
- `DEFAULT_SENSITIVE_HEADERS` adds `x-content-type-options`,
  `referrer-policy`, `permissions-policy`, `cross-origin-opener-policy` and
  `cross-origin-embedder-policy` — with HSTS, `x-frame-options` and CSP,
  every header the security-headers block sets. A WASM guest no longer
  reads or writes any of them unless its `HeaderPolicy` names it
  (`readable` / `writable`), so a guest step can no longer replace the
  middleware's value (e.g. `Referrer-Policy: unsafe-url`) without a grant
  the operator can narrow. A guest that sets one of these headers declares
  it in `headers.writable`, or its value is dropped (logged once).

- The `wafer-run/network` block's declared limits
  (`WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES`, `…__CONNECT_TIMEOUT_SECS`,
  `…__READ_TIMEOUT_SECS`, `…__REQUEST_TIMEOUT_SECS`,
  `…__STREAM_TIMEOUT_SECS`) come from the embedder's `ConfigSource`: the
  block reads them from its `lifecycle(Init)` config and hands them to its
  service through the new `NetworkService::configure(NetworkLimits)` (a
  default no-op for backends without such knobs). An invalid value fails
  Init, naming the key. `HttpNetworkService::from_env` is removed — the
  process environment was the only source it read; construct with
  `HttpNetworkService::new(NetworkLimits::default())` and let Init apply
  the configured limits. `HttpNetworkLimits`, the `*_KEY` constants and the
  `DEFAULT_*` limits move to `wafer_core::interfaces::network::service`
  (`HttpNetworkLimits` is now `NetworkLimits`).
- STRICT_SCHEMA comes from the database block's own Init config.
  `wafer-run/database` reads `WAFER_RUN__DATABASE__STRICT_SCHEMA` from its
  Init payload, not from `ctx.config_get` (the embedder's synchronous
  snapshot, which a `ConfigSource`-only embedder never fills), and
  `wafer-run/postgres` declares and reads `WAFER_RUN__POSTGRES__STRICT_SCHEMA`
  (it cannot declare the `WAFER_RUN__DATABASE__` key). A postgres-block
  deployment that set `WAFER_RUN__DATABASE__STRICT_SCHEMA` sets the
  postgres key instead. `database::handler::handle_lifecycle` takes the
  resolved flag (`strict_schema: bool`) in place of the context;
  `handler::strict_schema_from(event, key)` reads it.
- `FastembedService::new(model_id, cache_dir)` and
  `FastembedService::default_model(cache_dir)` take the model cache
  directory from the embedder; the service no longer reads
  `WAFER_RUN__FASTEMBED__CACHE_DIR` from the process environment (the old
  default was `data/models`).

- Go SDK (`go/wafer-run-go`): `HasBlock` returns `(bool, error)` and
  `FlowsInfo` returns `([]FlowInfo, error)` — a crashed FFI call (the -1 of
  `wafer_has_block`, the `{"error": ...}` of `wafer_flows_info`) is an error,
  where it read as "registered" and as "no flows". `Stop` and `Close` return
  `error`. `Close` now stops the runtime before freeing it, waits for calls
  using the runtime, and is safe to call concurrently with any other method;
  calls after it return `ErrClosed`. `wafer-ffi`: after `wafer_stop`,
  `wafer_run` is refused with an `Unavailable` error result instead of
  dispatching to stopped blocks.
- `wafer-ffi`: `wafer_resolve`, `wafer_start`, `wafer_stop` and `wafer_run`
  take their callback as `Option<WaferDoneCb>` (the C `wafer_done_cb`, NULL
  allowed by the type) and return `int`: `WAFER_ACCEPTED` (0) when they took
  the work, `WAFER_REFUSED_NULL_CALLBACK` (-1) — having done nothing — when
  the callback is NULL. A NULL callback used to be undefined behaviour: the
  Rust `fn` pointer type admits no NULL, and the spawned work called through
  it. C callers that ignore the return value compile unchanged; a Rust caller
  of the exports wraps its callback in `Some`.
- `wafer-run-node` no longer ships a prebuilt `wafer-run.node`: the tracked
  binary was built from an early revision and no longer matched the source.
  `npm run build` (or `build-debug`) in `crates/wafer-run-node` writes it,
  and `package.json` `main` now names it (it named an `index.js` that did not
  exist, so `require('wafer-run')` failed). `*.node` is gitignored.
- `WasmiBlock::load(path)` is `WasmiBlock::load(path, limits)`: the path
  loader takes its per-call `ResourceLimits` explicitly (pass
  `Wafer::resource_limits()` to match the runtime) instead of silently
  applying the loader defaults.
- `seal()` keys every block config by the block it configures: a config
  registered with `add_block_config` under an alias moves to the alias's
  target, so the target's `lifecycle(Init)` payload, its dispatch config and
  its `capabilities` subkey all see it. Before, Init and the `capabilities`
  subkey read only the target's own name and missed it. A block with a
  config under two of its names (its own and an alias, or two aliases) now
  refuses `seal()` with `RuntimeError::Config` naming both.
- A `PerFlow` WASM block reuses warm instances only within one flow; each
  flow has its own set, and calls outside every flow share one more.
  Before, every call of the block shared one set, so guest state crossed
  flows. `Context` gains `flow_id() -> Option<&str>` (default `None`), and
  a block's lifecycle context (Init, Start, Stop) and the calls made under
  it carry an empty flow id — observability hooks see `""` there instead
  of the labels `init` / `startup` / `shutdown`.
- `runtime::validation::check_action_interface` takes the interface specs
  as a `HashMap` keyed by interface name. `call_block` reads the callee's
  interface and `requires` allowlist from the plan compiled at `seal()`
  instead of building the callee's `BlockInfo` on every call.

- The windowed-counter upsert (`OnConflict::WindowedCounter`) stamps its
  `created_fields`/`updated_fields` with the server's current instant as
  RFC 3339 text, bound as a parameter — the form `database.create` stamps —
  instead of SQL `CURRENT_TIMESTAMP`. On SQLite and D1 the stored text
  changes from `2026-09-24 10:00:00` to `2026-09-24T10:00:00.123+00:00`
  (Postgres `TIMESTAMPTZ` columns take it as a timestamp; a Postgres TEXT
  column stored `2026-09-24 10:00:00.123+00`). A retention sweep comparing
  those columns against a cutoff must now spell the cutoff in RFC 3339; a
  space-format cutoff no longer matches the stored text (and never bound
  into a `TIMESTAMPTZ` column). The counter is keyed by the column
  `conflict_columns` names, with its value taken from `data[that column]`,
  not from a data field literally named `key`: `conflict_columns` must name
  exactly one column other than `id`, and `data` must hold a string `id`, a
  string for that column and nothing else — a second conflict column or any
  other data field (both used to be dropped without a word) is
  `InvalidArgument`, from the handler and from `DbExec::upsert` alike (was
  `Internal` for a missing `id`/`key` reaching the executor directly).
  `wafer_sql_utils::upsert::build_windowed_counter_upsert` takes a
  `stamped_at: &str` before `now`, its `key` argument is `conflict_value`,
  and it refuses a column named for two roles with the new
  `SqlBuildError::DuplicateColumn`; `DbExec::upsert` maps its builder errors
  to `InvalidArgument` (was `Internal`). The executor's check is public as
  `interfaces::database::exec::windowed_counter_row`.
- `wafer-run/ip-rate-limit` no longer declares
  `WAFER_RUN__IP_RATE_LIMIT__DISABLE`: it was read from the process
  environment only, never from the `ConfigSource`. Set the flow config
  `max_requests = 0` to turn the limiter off. A present `max_requests`,
  `window_seconds` (now positive) or new `ipv6_prefix` value that does not
  parse fails the block's Init and denies each request with
  `InvalidArgument`, where it used to fall back to the default.
- The `wafer-run/s3` and `wafer-run/postgres` blocks read their declared
  config vars (`WAFER_RUN__S3__ENDPOINT` / `__REGION` /
  `__MAX_OBJECT_BYTES`, `WAFER_RUN__POSTGRES__DATABASE_URL`) from the Init
  payload the runtime resolves through the embedder's `ConfigSource`. The
  s3 block read them from `std::env` and the postgres block from the
  embedder's config snapshot, so a source other than the process
  environment configured neither. An embedder that set these only as env
  vars needs a `ConfigSource` that reads the environment or holds them.
  Both read these once, at Init: a changed value applies after a restart.
  `WAFER_RUN__S3__ENDPOINT` is now optional: empty means AWS, and a source
  without it no longer fails the block's Init with a missing required key.
  With an empty endpoint the block uses the AWS SDK's default config, so
  its region comes from the AWS environment or profile, not from
  `WAFER_RUN__S3__REGION` (which applies only with an endpoint).
- `clients::database::list_all` and `list_sorted` no longer return a
  silent prefix. They asked for 10 000 rows and returned whatever came back,
  so a caller over a larger table saw the first 10 000 as if they were all.
  They now ask for one row more than `LIST_ALL_MAX_ROWS` (10 000, newly
  public) and fail with `ErrorCode::OutOfRange` when that row exists; read
  such a table with `paginated_list`. `DatabaseService::take_where` and
  `update_where` lose their trait defaults, which listed at most 10 000
  matching rows and then deleted or updated only those, one at a time: a
  backend (or a `forward_database_service!` ledger entry marked `inherit`)
  must implement them. Every in-tree backend already does, through
  `DbExec`. The `delete_where` default, which lists and deletes until
  nothing matches, fails with `Internal` when a row it deleted is listed
  again, where it used to loop forever.
  `wafer_sql_utils::aggregate::build_grouped_query` returns
  `Result<Statement, SqlBuildError>` and refuses a `DateBucketGroup::field`
  that is not a plain identifier with `InvalidIdentifier`, the rule
  `build_daily_count` applies; the field is spliced into the `date(…)` /
  `to_char(…)` expression text, and the builder used to trust its caller to
  have checked it.

- `NotFound` only ever comes from a service saying the thing a request
  names does not exist; the runtime no longer answers `NotFound` for "no
  such block". A client that reads `NotFound` as "unset" or "no row"
  (`clients::config::get_optional` / `get_default`,
  `clients::database::upsert_by_field`) used to fall back silently when the
  service block was not registered at all. Dispatch to an unregistered
  block (`run_block`, `call_block`, a flow step), to an unregistered flow
  (`Wafer::run`, `next.flow`) or to a missing `next.step` now fails with
  `ErrorCode::Unimplemented` (HTTP 501), as a service answers an operation
  it does not have; a `call_block` action outside the target's declared
  interface is `Unimplemented` too (was `InvalidArgument`), and so is an
  unknown `llm.*` / `image.*` operation. Over HTTP that check answers 501
  where it answered 400: `OPTIONS`, `TRACE` and `CONNECT` map to the
  `execute` action, which `http-handler@v1` does not declare, so such a
  request routed to an `http-handler@v1` block gets 501. A wasm guest passing an unknown
  stream handle to a `__wafer_host_stream_*` import gets `InvalidArgument`
  (was `NotFound`). `seal()` refuses to boot with
  `RuntimeError::BlocksNotFound` when a registered block's `requires` names
  a block that is not registered; the new
  `BlockReferenceSource::Requires { from_block }` names the requiring
  block. A dependency a block can run without moves to the new
  `BlockInfo::optional_requires` (`#[wafer_block(optional_requires = [...])]`):
  it stays on the block's `call_block` allowlist, is not checked at seal,
  and a call to it while it is absent fails with `Unimplemented`.
  `BlockInfo::call_allowlist` returns the combined allowlist.
- `seal()` downloads only what `wafer.lock` pins, and only bytes matching
  the pin. A flow step, route or block config naming an unregistered
  `org/block` — bare, `@latest` or `@version` — used to make `seal()` fetch
  the registry's current artifact (the latest one, for a bare name or a
  typo) and run it with no digest check, from a default registry
  (`raw.githubusercontent.com/wafer-run/registry/main`) that does not exist.
  Now such a reference is reported in `BlocksNotFound` and nothing is
  fetched. The one download left is a `wafer.lock` entry from a
  `registry+<url>` source whose cache directory is missing: the lockfile
  loader defers it (instead of failing the build with a cache miss) and
  `seal()` fetches `{url}/registry/download/{org}/{block}/{version}.wafer` —
  the URL `wafer install` uses — refusing it unless the tarball hashes to
  the entry's `sha256` and its `.wasm` to its `wasm_sha256`, then registers
  it under the entry's name with the entry's `capabilities` bound, like a
  cached entry. Downloads are capped at `MAX_PACKAGE_BYTES` and unpacked in
  memory within `MAX_DECOMPRESSED_BYTES` / `MAX_PACKAGE_ENTRIES` /
  `MAX_UNPACKED_BYTES` (new in `wafer_block::lockfile` with the
  `BoundedPackageStream` reader, shared with `wafer install`); a package
  with two `wafer.toml` or two `.wasm` files is refused. A lockfile entry
  naming the admin block, cached or not, fails `seal()`. Removed with the
  old path: the `WAFER_RUN_REGISTRY_BASE_URL` variable and
  `wafer_run::REGISTRY_BASE_URL_KEY`, the registry manifest format
  (`manifest.json`, `wasm_url`, `flow_url` — remote flows are no longer
  downloaded), `wafer_run::{parse_versioned_block, parse_unversioned_block,
  RemoteBlockRef, ABI_VERSION}`, `RuntimeError::AbiMismatch`, and the
  versioned-reference aliases (`org/block@version` is not an alias of a
  downloaded `org/block`; name blocks by `org/block` and pin the version in
  `wafer.lock`).
- `wafer install` treats `wafer.lock` as the authority without `--frozen`
  too: when the lockfile pins the resolved version, a registry sha256 that
  differs from the pin is an integrity failure, and neither the cache nor
  `wafer.lock` changes (it used to download the new tarball and re-pin the
  lockfile to it). A different version — `@version`, a bare `org/block`
  resolving past the pin, or a bumped `[dependencies]` entry — is what
  records a new sha, and that sha is whatever the registry reports for the
  new version (trust on first use: nothing earlier pins it). `--frozen`
  only ever downloads the shas `wafer.lock` already pins. Downloads are
  refused past the version's advertised `size_bytes` (and never above
  `MAX_PACKAGE_BYTES`), and extraction past `MAX_DECOMPRESSED_BYTES` of
  decompressed tar stream (headers, GNU long-name/long-link and pax records,
  and skipped bodies included), `MAX_PACKAGE_ENTRIES` entries or
  `MAX_UNPACKED_BYTES` of content, or of any entry that is not a regular
  file or directory. `registry_client::download_tarball` takes the byte cap.
- Config reads fail closed. `wafer_core::clients::config::get_default`
  returns `Result<String, WaferError>` (was `String`) and falls back to the
  default only when the key is not set (`ErrorCode::NotFound`); a WRAP
  denial, a transport or a decode failure is returned instead of the
  default. New `get_optional` returns `Result<Option<String>, WaferError>`,
  `Ok(None)` for an unset key. The config block's `config.get` reads the key
  from the request body only: a body that does not decode is
  `InvalidArgument`, and a `key` message meta is no longer a fallback.
- Infrastructure blocks fail closed. `wafer-run/monitoring`: `/_stats` and
  `/_monitoring` are gated by the new `stats_access` setting — `roles`
  (default; the caller's `auth.user_roles` must hold one of `stats_roles`,
  default `admin`) or `loopback` (the caller's `req.client.ip` must be
  loopback). An empty client IP is refused in both modes (it used to be
  trusted), and a loopback peer alone no longer suffices by default, which
  made the endpoint public behind a same-host reverse proxy. An unknown
  `stats_access` or an empty `stats_roles` fails Init. The payload's
  `top_paths` (raw request paths, which carry reset tokens and share links)
  is replaced by `routes`: counts per declared `BlockEndpoint::path`
  template, with undeclared paths under `"(unmatched)"`.
  `wafer-run/readonly-guard`: in read-only mode only `retrieve`, `list` and
  an HTTP `OPTIONS` preflight pass; `execute`, custom and empty actions are
  denied (only `create`/`update`/`delete` were). `readonly` accepts exactly
  `true`/`1`/`false`/`0`/empty — any other block-config value (including
  `null` or an array) fails Init, and any other step-config value denies the
  request; `ReadonlyGuardBlock` is a unit struct (the dead `enabled` field is
  gone). `wafer-run/router`: `parse_routes` returns
  `Result<Vec<Route>, RouteConfigError>` and a malformed entry (missing or
  non-string `path`/`block`, both `actions` and `methods`, a non-string
  action, a non-array `routes`) fails Init instead of being dropped with a
  warning. `wafer_block::match_path`: a `{var}` segment needs a non-empty
  path segment (`/users/{id}` no longer matches `/users/`).
  `wafer-run/web`: `web_prefix` is whole path segments and enforced — a
  path not under it is `NotFound` (it used to be served from the folder
  root, and `/docs` stripped `/docsecret.txt` to `ecret.txt`); a prefix
  without a leading `/` fails Init. `wafer-run/inspector`: `/app` and
  `/flows/{id}` replace with `"[redacted]"` the value of every config key
  that is or ends in `SECRET`, `KEY`, `TOKEN` or `PASSWORD` (as `_SECRET`
  etc., case-insensitive) or is declared `InputType::Password`, and mask
  the userinfo of any other value that is a URL with credentials
  (`postgres://redacted:redacted@db/x`), at any depth of block configs and
  flow step configs.
- `wafer-run/security-headers` refuses more operator CSP. In `default-src`,
  `script-src`, `script-src-elem`, `script-src-attr`, `worker-src` and
  `child-src`, every host wildcard is refused (`*.example.com` included:
  the old single-label check passed `*.github.io`, `*.pages.dev`,
  `*.workers.dev` and `*.co.uk`, where anyone can register a subdomain), as
  is a host-source with an explicit scheme other than `https`
  (`http://cdn.example.com`). `report-uri` accepts only a path on this
  origin (`/csp-reports`), because reports carry page URLs and, under
  `'report-sample'`, script samples; `report-to` takes exactly one group
  name. A `base-uri` / `form-action` the baseline lacks is added only when
  every source is well-formed. Refused sources are dropped and logged at
  Init as before. `wafer-run/cors` adds `Origin` to an earlier block's
  `Vary` (folding every `resp.header.{vary}` spelling into one
  `resp.header.Vary`) instead of replacing it.
- Flow config is typed and a flow is validated wherever it is added.
  `wafer_flow::FlowConfig` fields are `on_error: Option<OnError>` (`Stop` /
  `Continue`), `timeout: Option<FlowTimeout>`, `timeout_ms:
  Option<FlowTimeoutMillis>`, `max_steps: Option<NonZeroU64>`, and the struct
  denies unknown keys. `wafer_flow::parse` refuses an `on_error` other than
  exactly `"stop"` or `"continue"` (a `"Stop"` used to mean "continue past a
  failed step"), a `timeout` that is not `<n>ms`/`<n>s`/`<n>m`/`<n>h`/`<n>`
  (a malformed one used to mean "no timeout"), a `timeout` or `timeout_ms`
  that is zero or above `wafer_flow::MAX_FLOW_TIMEOUT` (24h; an overflowing
  value such as `"5124095576030428h"` used to panic the first run), a zero
  `max_steps`, and a misspelled config key. `validate`
  also refuses a config setting both `timeout` and `timeout_ms`, a
  `next.step` naming a step inside a `parallel` branch (the executor only
  jumps to top-level steps), `next` on a step inside a branch, a `next`
  entry naming both `step` and `flow`, and a `next.flow` naming the flow
  itself. `Wafer::add_flow` now validates and returns
  `Result<(), RuntimeError>`, so flows added through the typed API and flows
  `seal()` downloads from the registry are validated like `add_flow_json`
  ones. `wafer_block::config::parse_duration` is removed (its one caller was
  flow timeouts; `FlowTimeout` parses them).
- A `next.flow` transfer no longer starts the target with a fresh step
  budget and deadline. The flows one request passes through share one step
  counter, deadline and cancellation flag; each flow entered can only
  tighten them (its `max_steps` caps the running count, its timeout counts
  from when it is entered). A cycle of transfers ends with
  `ResourceExhausted` instead of recursing without bound, and transfers run
  as a loop rather than nested calls. `flow_end` hooks for a chain fire once
  it finishes, last-entered flow first.
- `InputStream` can fail. Its items are `Result<Vec<u8>, WaferError>`
  (were `Vec<u8>`): an `Err` item means the body did not arrive whole — the
  connection dropped, a size cap or read deadline was hit, the producer
  aborted — and it is terminal (every later poll is `None`). A body that
  failed used to end like a complete one, so a truncated upload was stored
  as a success. `from_stream` / `from_stream_with_cancel` take a stream of
  `Result<Vec<u8>, WaferError>`: an adapter maps its transport's read error
  to `Err`, never to an empty chunk (`body.map(|c| c.map_err(…))`; an
  infallible source maps its chunks with `Ok`). `collect_to_bytes` returns
  `Result<Vec<u8>, WaferError>` and discards the prefix on failure; a block
  answering a failed body passes the error on
  (`Err(e) => return OutputStream::error(e)`). In-tree consumers treat the
  failure as a failure: the `service_block!` buffered path, the flow
  executor and the wasm guest boundary answer with the body's error without
  running the handler, flow or guest; `storage.put_streaming` stores
  nothing.
- `StorageError` has two new variants. `Body(WaferError)`: the body stream
  of a `put_streaming` failed, and nothing was stored at the key (the
  previous object, if any, is left whole); the trait default returns it
  before calling `put`, and the storage handler answers with the body's own
  error. `InvalidArgument(String)`: a malformed request (the storage handler
  maps it to `InvalidArgument`); `LocalStorageService` returns it for a
  path that escapes the root and for an invalid list cursor, which were
  `Internal`. An exhaustive `match` on `StorageError` needs both arms.
- `LocalStorageService` stages writes in `.wafer-staging/` directly under
  its root instead of a hidden temp file next to the object, so `list`
  never returned an in-flight or orphaned `.{key}.tmp.{pid}.{seq}` as an
  object. `list` and `list_folders` skip the directory, a request for a
  path inside it — under any spelling the filesystem resolves to it, such
  as `.WAFER-STAGING` on a case-insensitive one — is `InvalidArgument`, and
  `LocalStorageService::new` deletes whatever it holds (writes a stopped
  process left behind). One process owns a storage root, and every folder
  must be on the root's filesystem (the staged file is renamed onto the
  key); a folder mounted or symlinked onto another filesystem fails every
  write with an error naming that cause. Writes are now durable: the
  staged file is `fsync`ed before the rename and, on Unix, the key's
  directory after it, so a power loss cannot leave an empty object.
- `wafer-run/http-listener` streams the request body to the flow or block
  instead of buffering it before dispatch. A `Content-Length` over
  `max_body_bytes` is still refused with `413` before dispatch; a body that
  grows past the cap, is not complete within `body_read_timeout_secs`, or
  whose connection drops fails the `InputStream` (`ResourceExhausted`,
  `DeadlineExceeded`, `InvalidArgument`), and the client gets `413`, `408`
  or `400` with `Connection: close` whatever the flow or block answered.
  `body_read_timeout_secs` counts from the request head, including time the
  flow or block spends between reads.
- `HttpNetworkLimits` has a new field, `stream_timeout: Option<Duration>`
  (default `None`), so a struct literal without `..Default::default()`
  needs it. It is the total for `do_request_streaming`, response body
  included, read by `HttpNetworkService::from_env` from the new declared
  key `WAFER_RUN__NETWORK__STREAM_TIMEOUT_SECS` (unset or empty: no total;
  anything but a positive integer fails construction). Without it an
  upstream that sends a byte within every idle `read_timeout` holds a
  stream open indefinitely.
- The database schema cache never records that a table is missing.
  `SchemaCache::table_exists` / `set_table_exists_if_gen` are replaced by
  `table_known_present` / `mark_table_present_if_gen`, which can only say
  a table exists; `SchemaCache` gains `id_policy` / `set_id_policy_if_gen`.
  `DbExec::run_insert` and `DbExec::table_autogenerates_id` are removed;
  the shared `DbExec::id_policy` (returning `introspect::IdPolicy`)
  replaces them, so a backend that overrode either drops the override.
  See Fixed for the behaviour.
- `create` (and `create_many`, `batch`, `insert_guarded`) refuses a row
  without an `id` for a table whose `id` holds integers that nothing fills
  — SQLite `id INT`/`BIGINT PRIMARY KEY`, a composite or `WITHOUT ROWID`
  key, a Postgres `integer` key with no identity or sequence — with
  `InvalidArgument` naming the table. Postgres already failed these
  inserts (a minted string bound into an integer); SQLite stored them with
  a `NULL` id. Such a row must carry its `id`, or the key must be declared
  so the database numbers rows.

  **Existing SQLite tables declared this way hold `NULL`-id rows** written
  by the old executor, which reported each row's rowid to the caller as
  its id. The executor refuses new id-less rows rather than minting string
  ids among them, so the table's ids stay one type; the old rows still
  need repair. For each such table (`PRAGMA table_info(t)` shows `id` with
  an `INT`-containing type and `pk` > 0, not declared exactly
  `INTEGER PRIMARY KEY`):
  1. `SELECT COUNT(*) FROM t WHERE id IS NULL;` — any rows are affected.
  2. `UPDATE t SET id = rowid WHERE id IS NULL;` — gives each row the id
     callers were given. On a composite key, check first that no
     `(rowid, …)` pair collides with an existing key.
  3. Either keep supplying ids on every create, or rebuild the table so
     SQLite numbers rows: `CREATE TABLE t_new (id INTEGER PRIMARY KEY, …);
     INSERT INTO t_new SELECT * FROM t; DROP TABLE t;
     ALTER TABLE t_new RENAME TO t;` (recreate its indexes), in one
     transaction.
- The `wafer-run/postgres` URL config var is `InputType::Password`
  (sensitive), so a `user:password@` URL is masked wherever config is
  served back.
- `VectorError` has a new `Unavailable(String)` variant (a busy or locked
  store); an exhaustive `match` on it needs an arm. The vector handler
  answers it with `ErrorCode::Unavailable`.
- The `wafer-run/postgres` block reads `WAFER_RUN__POSTGRES__DATABASE_URL`
  through its declared config (`ctx.config_get`), resolved by the
  embedder's `ConfigSource`, instead of from the process environment. An
  embedder whose `ConfigSource` is not the environment and who set only
  the env var must supply the value through the source.

- `wafer_block_security_headers::merge_csp` returns a `CspMerge`
  (`policy` plus the `refused` directives and sources) instead of a
  `String`. The operator `csp` config is now parsed case-insensitively
  (one directive per name, later duplicates refused, as browsers ignore
  them) and more of it is refused: in `default-src`, `script-src`,
  `script-src-elem`, `script-src-attr`, `worker-src` and `child-src`, any
  `*` host at any scheme, port or path (`https://*/`, `https://*:443`,
  `*:443`), every scheme-only source (including `blob:` in `worker-src`),
  single-label wildcards (`*.com`), `'unsafe-eval'` and malformed sources;
  any widening of `base-uri`/`form-action`; and `frame-ancestors` in any
  spelling (use the `frame_ancestors` key). Previously an upper-case
  `FRAME-ANCESTORS *` or `SCRIPT-SRC …` was emitted as a second directive
  that the browser enforced over the baseline, and `script-src-elem
  https:` passed unfiltered. Refused items are left out of the policy and
  logged at Init with `warn`; Init still succeeds. A `csp` config holding
  a character no header can carry (anything but visible ASCII and ASCII
  whitespace, e.g. a pasted smart quote or NBSP) fails Init with
  `InvalidArgument` naming the character.
- `wafer-run/cors` never sends `Access-Control-Allow-Origin` on a request
  without `Origin` (it used to send the raw configured list, e.g.
  `https://a,https://b`), and sends `Vary: Origin` on every response once
  `allowed_origins` is configured, not only when it reflected an origin.

- The HTTP codec (`wafer_block::http_codec`) refuses response meta no
  transport can send, instead of handing it to the adapter.
  `classify_response_meta` now returns
  `Result<Option<ResponseMetaPart>, InvalidResponseMeta>`, whose `kind` is
  `Unsendable` for a `resp.status` outside `100..=999`, a header name that
  is not an RFC 9110 token, or a header, cookie or content-type value
  holding a control or non-ASCII character, and `TransportOwned` for
  `TRANSPORT_OWNED_RESPONSE_HEADERS` (`Connection`, `Content-Length`,
  `Keep-Alive`, `Proxy-Connection`, `TE`, `Trailer`, `Transfer-Encoding`,
  `Upgrade`). A transport-owned header is dropped with a `warn` log. An
  unsendable entry fails the response closed: `response_meta_parts` and
  `response_meta_entries` now return a `Result`, and every buffered path
  answers `unsendable_response` — a 500 with the JSON `Internal` error body,
  logged by key — rather than serve the page without, say, its
  `Content-Security-Policy`. The embedder wire format encodes such a
  terminal as an `Internal` error. (Which headers that 500 keeps: see the
  `unsendable_response(meta, invalid)` entry above.) The flow
  executor fails a step whose output carries an unsendable entry with
  `Internal` (whatever `on_error` says) before laying any of it over the
  flow message, so a malformed value never displaces a middleware's valid
  one and the 500 keeps the middleware's headers. Before, the native
  listener turned a CR/LF in any header value into a bare 500. Header names
  are case-insensitive end to end: any case of `resp.header.content-type`
  is the response content type and any case of `resp.header.set-cookie` a
  `Set-Cookie`; the buffered codec emits one header per case-insensitive
  name, the later write winning (so a block setting `content-type` no
  longer gets a second, default `Content-Type`), and every `Set-Cookie`.
  A request header repeated on the wire is one `http.header.*` entry, its
  lines joined in order (`"; "` for `Cookie`, `", "` otherwise, RFC 9110
  §5.3); `http.content_type`, `http.host` and `req.content_type` carry the
  same joined value. Before, the mirrors kept the first line and
  `http.header.*` the last (every `X-Forwarded-For` line but the last was
  lost). A single-valued header (`SINGLETON_REQUEST_HEADERS`: `Host`,
  `Authorization`, `Content-Type`, `Content-Length`) is not joined:
  `wafer-run/http-listener` answers a request repeating one with
  `400 Bad Request` (`repeated_singleton_header`).
  lost). `ResponseBuilder::set_cookie` keys a cookie by its identity —
  `http_codec::cookie_meta_key`: `resp.set_cookie.{name}`, plus
  `;Domain=…`/`;Path=…` when set — instead of its position
  (`resp.set_cookie.0`, `.1`, …), and a second directive for the same
  cookie replaces the first. New `wafer_block::response::cookie_meta`
  builds that entry for producers writing meta directly (an error's meta, a
  middleware's message); `http_codec::CookieId` / `cookie_id` are the
  identity the flow executor merges cookies by. Code that read
  `resp.set_cookie.0` back from a builder's meta reads the identity key.


- `wafer_core::interfaces::storage::service::StorageError` has a new
  variant, `TooLarge(String)`: an object over a backend's read cap. An
  exhaustive `match` on `StorageError` needs an arm for it. The storage
  handler maps it to `ResourceExhausted`.

- `wafer-run/http-listener` bounds every connection. New `flow_config`
  knobs (also routed by the `wafer-run/http-server` flow's `config_map`,
  with `max_body_bytes`): `header_read_timeout_secs` (default 30; a client
  that does not finish its request head in time, or an idle keep-alive
  connection, is closed), `body_read_timeout_secs` (default 120; a body
  that does not arrive in time gets `408 Request Timeout` and the
  connection closes), `write_timeout_secs` (default 60; a response write
  that makes no progress that long — the client stopped reading — drops the
  connection), `max_connections` (default 1024; at the cap the listener
  stops accepting and further clients wait in the kernel backlog) and
  `shutdown_grace_secs` (default 10; `Stop` lets open connections finish
  their in-flight request for that long, aborts the rest, and returns once
  they are gone). There were no timeouts and no cap: a client could hold a
  connection and its task open forever by sending a request head or body
  one byte at a time, or by never reading its response (slowloris), and
  graceful shutdown waited on such connections in a task nothing awaited. Slow clients that took longer than the defaults are now cut
  off; raise the knobs if you serve them. The knobs and `max_body_bytes`
  accept a JSON number or a decimal string in `1..=max` (timeouts at most
  86400); `0`, fractions and anything else fail Init with
  `InvalidArgument` — `max_body_bytes: 0` used to be accepted, and a
  numeric `max_body_bytes` used to be ignored in favour of the default.
  The listener now serves HTTP/1.1 only, through hyper's HTTP/1 connection
  builder: `axum::serve` also answered cleartext HTTP/2 (prior knowledge)
  in any build where Cargo feature unification enabled `hyper-util/http2`,
  and protocol detection read the connection preface with no deadline.
  The request body now streams to the dispatch target; see the
  `InputStream` failure-terminal entry for how the cap and the body
  deadline reach it.
- The network grant covers every redirect hop. `HttpNetworkService`
  followed redirects inside reqwest, after the network handler had
  authorized only the first URL, so an allowed API that redirected (an open
  redirect is enough) handed the caller a body from any public URL. Now the
  handler (`wafer_core::interfaces::network::handler`) follows redirects
  itself: each hop's URL passes the same `(url, Network, Read)` check before
  it is issued as a new service call, so the service's SSRF gates run on it
  too; a hop outside the grant fails the call with the check's
  `PermissionDenied` and is never contacted; more than
  `handler::MAX_REDIRECT_HOPS` (10) hops is `Unavailable`. 301/302/303 turn
  any method but `GET`/`HEAD` into a body-less `GET`, 307/308 replay method
  and body, and a hop to another origin (scheme, host or port) drops
  `Authorization`, `Proxy-Authorization`, `Cookie`, `Cookie2` and
  `WWW-Authenticate`. The `NetworkService` contract is now that an
  implementation MUST NOT follow redirects: return the 3xx with its
  `Location`, or fail the request if the platform cannot expose it (a
  browser `fetch` in `manual` mode). New `NetworkService::buffered_deadline`
  (default: never) resolves when a buffered request's total has elapsed; the
  handler races the whole redirect chain against it, so the total bounds the
  chain, not each hop (`HttpNetworkService`: `request_timeout`).
  `wafer_net_security::ssrf_redirect_policy` and
  `wafer_net_security::MAX_REDIRECT_HOPS` are removed.
- A network capability entry matches paths on segment boundaries.
  `BlockCapabilities::allows_network_url` compared paths with a plain
  prefix, so `https://a.com/v1/public` also admitted `/v1/public-admin` and
  `/v1/publicity`. An entry now admits its own path and paths below it
  (`/v1/public/x`); an entry ending in `/` still admits everything under it.
  A path whose part below the entry holds an encoded `/` or `\` (`%2F`,
  `%5C`) is refused, since an upstream that decodes it could resolve `..`
  out of the granted path.
- `HttpNetworkService` timeouts no longer cut off streams.
  `do_request_streaming` shared the client's 30 s total timeout, so a
  download still making progress failed at 30 s. The client now has a
  connect timeout and an idle read timeout (reset on every read); only the
  buffered `do_request` has a total. All three are `HttpNetworkLimits`
  fields, read once by `HttpNetworkService::from_env` from the new declared
  keys `WAFER_RUN__NETWORK__CONNECT_TIMEOUT_SECS` (default 10),
  `WAFER_RUN__NETWORK__READ_TIMEOUT_SECS` (30) and
  `WAFER_RUN__NETWORK__REQUEST_TIMEOUT_SECS` (30), rejected at construction
  when not a positive integer. `HttpNetworkService::with_max_response_bytes`
  is replaced by `HttpNetworkService::new(HttpNetworkLimits)`; migration:
  `new(HttpNetworkLimits { max_response_bytes: n, ..Default::default() })`.

- `VectorService` has a required `rename_index(from, to)` method (see
  Added). Every implementation must provide it; there is no default,
  because a backend that cannot move an index leaves the indexes its users
  created under mixed-case names unreachable. `VectorError` has a new
  `InvalidRename { from, to }` variant (`InvalidArgument` on the wire), and
  `InvalidIndexName`'s message states the lowercase rule index names follow.

- `wafer_block_sqlite::vector::SqliteVecService::new` returns
  `rusqlite::Result<Self>`: it registers the SQL function filtered searches
  call (`wafer_sql_utils::vector::METADATA_FILTER_FN`) on the connection,
  which can fail. Its docs now state the connection's busy timeout is what
  `upsert` and `delete` wait on when another writer shares the file
  (`Connection::open` sets 5 s).

- A list page is never unbounded by accident. `ListOptions::limit` and the
  wire `ListRequest::limit` are `Option<u32>`: `None` (absent on the wire)
  returns every matching row, as `0` silently did while the docs called it
  "backend default". `Some(0)`, and a positive `offset` with no `limit`
  (which SQLite and D1 cannot render — it was a syntax error surfacing as
  `Internal`), are `InvalidArgument` on every backend, checked before the
  table probe (`wafer_sql_utils::query::check_pagination`). An encoder from
  before this change always sent `limit: 0`, so an unrebuilt guest's lists
  now fail loudly instead of returning everything; rebuild it. The select
  builders (`build_select`, `build_select_with_condition`,
  `build_select_columns`) and `apply_pagination` return
  `Result<_, SqlBuildError>` (new variants `ZeroLimit`,
  `OffsetWithoutLimit`). `Message::pagination_params` keeps `page_size` in
  `1..=100`: an absent, unparsable or `0` `?page_size=` takes the default,
  where `0` used to pass through and list the whole table.
  `clients::database::paginated_list` refuses a `page_size` above `u32::MAX`.
  Migration: `limit: 0` → `limit: None` (or `..Default::default()`),
  `limit: n` → `limit: Some(n)`.
- A stored value reads back with the structure its column declares, never
  the structure its text happens to have. The SQL row codec used to parse
  any text value that started and ended with `{…}`/`[…]` and parsed as JSON,
  so a user who titled something `[1]` or `{}` got an array or object back
  (and block code reading it with `as_str()` saw nothing). A text value is
  now parsed only in a column declared to hold JSON: `JSON` on SQLite / D1 /
  sql.js, `json`/`jsonb` on Postgres (`wafer_sql_utils::introspect::
  is_json_decl_type`). `DataType::Json` now creates a SQLite column declared
  `JSON TEXT` (`ddl::SQLITE_JSON_TYPE`; it was `TEXT`) — TEXT affinity, so
  numeric-looking JSON text is not turned into a number — and so does a
  column lazily added for an object or array value; a bare `JSON` column is
  still read as JSON. Postgres keeps `JSONB`. Every typed write stores a JSON
  column's value as its JSON text (`codec::encode_json_value`), so any JSON
  value round-trips: the string `"123"` is stored `"\"123\""` and reads back
  a string, not a number. A block that wrote pre-serialized JSON strings to a
  JSON column must write the parsed value instead. On Postgres a text
  parameter for a `json`/`jsonb` column is JSON text. Raw SQL is not encoded
  or decoded: `query_raw` returns a JSON column's text on the SQLite family
  and the structured value on Postgres. `build_list_columns` returns a
  `decl_type` column next to `name`; the `SchemaCache` column entry is a
  `TableColumns { names, json }` and no longer caches a missing table's empty
  column list. Every row-returning `DbExec` primitive (`run_fetch`,
  `run_fetch_one`, `run_execute_returning`, `BatchOp::Rows`/`FetchOne`,
  `TxOp::Returning`) takes the result's JSON columns (`codec::JsonColumns`),
  which the executor looks up from the table a statement reads
  (`DbExec::json_columns`, one cached introspection per table, in
  STRICT_SCHEMA mode too); raw SQL and aggregate rows are decoded with
  `JsonColumns::NONE`. `codec::decode_text_value` is replaced by
  `codec::decode_text(column, text, json)`, and `codec::record_from_json_row`
  takes the JSON columns. Embedders: a `DbExec` implementation (the D1 and
  browser adapters) passes the `json` argument to `record_from_json_row`; a
  table that stored JSON in a `TEXT` column and relied on the old guess must
  declare that column `JSON` (a new migration; SQLite cannot retype a column
  in place, so rebuild the table), or its objects read back as their text.
- `PostgresDatabaseService::from_pool` returns `Result` and refuses a pool
  whose connections have no statement cache (`statement-cache-capacity=0`),
  as `connect` refuses such a URL: every statement is prepared once to learn
  its parameter types, and without the cache each prepare would leave a
  named statement open on the connection.
- `DatabaseError` has a new `Unavailable` variant for a transient backend
  fault, and `DatabaseError::code()` names each variant's wire code (see
  Fixed: "A database fault that may clear is `Unavailable`"). A `match` over
  `DatabaseError` needs the arm.

- The public `wafer_run::runtime::init_stack` module (`InitStack`,
  `InitGuard`) is gone. Init cycles are refused by a runtime-wide wait-for
  graph of in-flight inits instead (see Fixed: "A failed Init is retried when
  the failure was transient"); nothing outside the runtime used the per-dispatch
  stack. `BlockSlot` keeps its API; a `Transient` outcome now starts a retry
  backoff (`slot::TRANSIENT_RETRY_BASE`, doubling to `slot::TRANSIENT_RETRY_MAX`)
  during which `get_or_init` and `try_cached` return it without re-running init.

- A block's storage is its own. WRAP admitted every Storage path without a
  leading `@` for any attributable caller, assuming the storage block would
  rewrite it into the caller's namespace, and wafer-core's storage handler
  never did — so any block could read, overwrite and delete another block's
  objects by spelling their path (`folder: "acme/files/uploads"`). The storage
  handler now resolves every `folder` / folder `name` before it authorizes:
  a plain folder is relative to the calling block's own namespace
  (`uploads` from `acme/app` is `acme/app/uploads`; the empty folder is
  `acme/app` itself), and `@{org}/{block}/…` names a namespace explicitly.
  The handler authorizes the resolved path and hands that same path to the
  `StorageService`. WRAP's Storage rule is now plain ownership: a path is
  admitted for the block its `{org}/{block}` prefix names, the admin block,
  or a Storage grant, and a path with an empty, `.` or `..` segment — or any
  `\`, which a Windows filesystem backend reads as a separator — is
  refused (`wafer_block::wrap::is_traversal_safe_path`; the handler answers
  `InvalidArgument`); `@` is request syntax the handler strips and WRAP no longer reads
  (`storage_resource_owner("@a/b/c")` is `@a/b`, and a Storage grant written
  with `@` no longer passes the owner check). A plain folder from a call with
  no calling block is `PermissionDenied`. `decode_and_authorize_checked`
  takes a resolver returning `(resolved, resource, type, access)` and returns
  `(request, resolved)`; the resolver itself is public as
  `wafer_core::interfaces::storage::handler::resolve_folder`. Embedders
  registering
  `wafer_core::service_blocks::storage::StorageBlock` directly: objects a
  block stored under a plain folder `f` now resolve to `{block}/f`, so data
  written before this change under the raw path is not found there — move it
  under the writer's namespace, or have callers address it as `@f` (admin or
  grant). `wafer-run/web` serves `web_root` from `wafer-run/web/{web_root}`.
  Embedders that already scoped paths in a wrapper block (impresspress's
  `ImpresspressStorageBlock`) must drop that rewrite in the same upgrade, or
  every path is prefixed twice. `BlockCapabilities::storage_folders` entries
  are checked against that resolved path, so they name
  `{org}/{block}/{folder}`: a block writing its plain folder `uploads` needs
  the entry `acme/app/uploads`, and a bare `uploads` entry (which the field
  doc used to describe as covering `uploads/*`) now admits nothing.
- A flow that stops early keeps the response headers its middleware set.
  A step's `Error` (under `on_error = "stop"`), `Halt` or `Drop`, and an error
  the executor raises itself (step budget, deadline, a failing `next`
  condition, an unresolvable input, a missing block, a `next` transfer to an
  unknown flow), used to return only the stopping step's own meta, so every
  401/403/404/429 behind `security-headers` and `cors` shipped without CSP,
  `X-Content-Type-Options` or `Access-Control-Allow-Origin` (a browser cannot
  even read a cross-origin error without the last). The flow boundary now
  carries the flow message's `resp.header.*` and `resp.set_cookie.*` entries
  that a middleware (`Continue`) step left there, or that the flow's inbound
  message carried, after any middleware overwrote or removed one. Cookie
  semantics: a cookie a middleware set (a refreshed session) is carried onto
  the error; a cookie a responding step set is NOT — that response was
  discarded, so a login step's session cookie is never set on a failed
  request — and neither is any header a responding step set (a static
  file's `Cache-Control: immutable` must not cache a 500), unless a later
  middleware rewrote it; where a responding step overwrote a middleware's
  header or cookie, the middleware's entry is carried in its place (CORS's
  `Vary: Origin` survives an asset step's `Vary: Accept-Encoding`, and
  `X-Frame-Options` reverts to the security-headers value). Never carried, whatever set them: body-describing
  headers (`Content-*`, `ETag`, `Last-Modified`, `Location`,
  `Accept-Ranges`), `resp.status`, `resp.content_type`, the stopping step's
  own partial output (streamed `Meta` before an `Error`), and a parallel
  branch's message (discarded at the join, as on success). The terminal's
  own entries win: a header by name, case-insensitively; a cookie by name,
  `Path` (case-sensitive; a missing `Path` is not `Path=/`, since the
  browser derives it from the request URI) and `Domain` — not by its
  `resp.set_cookie.*` key, which a producer writing meta by hand may pick
  by position (`.0`, `.1`, …) so unrelated cookies from two producers
  collide. `Vary` values are unioned, so the CORS
  middleware's `Vary: Origin` survives a terminal's own `Vary`. The same
  header, cookie and `Vary` rules now apply when a responding step's meta is
  laid over the flow message on the success path (before, a responder's
  `resp.set_cookie.0` replaced a middleware's unrelated `resp.set_cookie.0`,
  and a header differing only in case was emitted twice). A `next` flow
  transfer hands the target flow the message and the record of what
  responding steps wrote, and returns its terminal unchanged. A WASM guest's
  `Error` is still sanitized by the guest egress allowlist before the
  executor sees it; carried entries come from the host's flow message, never
  from the guest's output. `Drop` now carries response meta so a flow's drop
  can keep its CORS headers: `StreamEvent::Drop` and
  `TerminalNotResponse::Drop` are struct variants `Drop { meta }` (match
  `Drop { .. }`), and `OutputStream::drop_request_with_meta` /
  `OutputSink::drop_request_with_meta` build one (`drop_request()` is the
  empty-meta drop). The HTTP codec renders a drop as a `204` with its meta's
  headers and cookies (no `Content-Type`, `resp.status` ignored), and the
  embedder wire format's `drop` action now carries `meta` (headers and
  cookies only) like every other action. Forwarders that re-emit a received
  `Drop` must pass its meta on. Internal: `runner::run_resolved` returns its
  init failure as a `WaferError`.
- A block is the name it is registered under. `register_block` (and every
  path built on it: `load_inventory_blocks`, the lockfile loader,
  `embed::register_path` behind the Node/Go/C bindings) refuses a block whose
  `info().name` differs from the registration name, with the new
  `RuntimeError::BlockNameMismatch { registered, reported }`. Before, the two
  could differ, and WRAP grant ownership and the admin-block match were
  decided against the reported name while `check_access` attributed calls to
  the registration name — so a WASM guest registered as `x/attacker` that
  reported itself as `a/victim` could grant itself read-write on
  `a__victim__*`, or claim the admin block's name and declare typed
  Network/Crypto grants. Grant validation (at registration and on the
  `set_admin_block` rescan), `requires` resolution and the `block_infos` /
  startup-snapshot order now key on the registration name only. Embedders that
  registered a block under a different name must register it under the name it
  reports (for a WASM guest, the `name` its `__wafer_info` returns). A WASM
  module whose `__wafer_info` fails reports the placeholder `unknown` and is
  therefore refused too, where it used to register and never route. Namespace
  grant wildcards are also held to the declaring block's own namespace: the
  owner is read from the pattern's literal text before the first `*`, which
  must include the whole owner and its terminator (`acme/files/*`,
  `acme__files__*`). `acme/files*` (which also matched `acme/filesx/...`) and
  `acme/*` (all of `acme`) are rejected at `seal()` with `GrantsRejected`.
- The database handler admits only plain lowercase identifiers
  (`[a-z0-9_]`, 1 to 63 bytes — PostgreSQL silently truncates a longer name
  into its 63-byte prefix, another table) as collection, column, sort,
  filter, projection, guard, alias and schema-op names; anything else is
  `InvalidArgument`. The rule is `wafer_block::db::is_plain_ident`
  (`MAX_IDENT_LEN = 63`), and `wafer_sql_utils::ident::validate_ident` now
  enforces the same rule (it admitted uppercase and any length), so the
  executor, every DDL builder and every backend refuse what the handler
  refuses. `ddl::build_drop_table`, `build_add_column` and
  `build_add_column_for_value` return
  `Result<Statement, SqlBuildError>`, and `DatabaseError` implements
  `From<SqlBuildError>` (as `InvalidArgument`). Block names are capped at
  59 bytes (`validate_block_name`), so a block's `{org}__{block}__` table
  prefix always leaves room for a table name. A
  collection is checked BEFORE the caller is authorized on it, and the SQL
  executor runs on the authorized string verbatim: `sanitize_ident` (which
  stripped every other character after authorization, so a block `acme/a-b`
  requesting `acme__a-b__t` passed WRAP Rule 3 as its own owner and then read
  and wrote `acme/ab`'s table `acme__ab__t`) is removed from
  `wafer-sql-utils`, and `DbExec` refuses a non-identifier table or column
  name with the new `DatabaseError::InvalidArgument`. Callers that spelled a
  collection with `-`, uppercase or non-ASCII letters must switch to the
  lowercase `{org}__{block}__{name}` form; those spellings never reached their
  own table (they were silently aliased to the stripped one).
- A read, or the filter of a write, never adds a column. `list`, `count`,
  `delete_where[_count]`, `take_where`, `update_where[_count]`, `sum`,
  `aggregate`,
  `increment_field_where`, `update_guarded` and `insert_guarded` (guard
  filters and `SumAtMost` fields) no longer `ALTER TABLE` for an unseen
  filter, sort or projection column — which let a Read-only or append-only
  caller reshape the owner's table. Outside STRICT_SCHEMA they answer
  `InvalidArgument` naming the unknown column (checked against the table's
  column list, re-read once uncached — without evicting the cache — before
  refusing), and add nothing; in
  STRICT_SCHEMA the backend's own "no such column" error answers, as before.
  Writes still add the columns their DATA names; schema growth otherwise goes
  through `database.ensure_table` / `database.add_column`. `DbExec`'s
  `ensure_query_columns` is replaced by `require_columns`, and
  `wafer_sql_utils::ddl::build_add_text_column` (used only by the removed
  path) is removed — `build_add_column_for_value` covers it.
- A WASM guest no longer receives the request's `Cookie` or `Authorization`
  header unless it names the header in its declared
  `BlockCapabilities.headers.readable` (in `BlockInfo::capabilities`, which is
  what the inspector's `/blocks/{name}` shows; operators narrow it with the
  `capabilities` block-config subkey). The inbound filter matched request
  headers under a `req.header.` prefix that nothing produces, so every
  request header the HTTP codec writes (`http.header.*`), session cookies
  included, reached every guest. On the way out, `headers.writable` now
  applies to every guest egress: an `Error` result's meta and a `Continue`
  message skipped it and could set cookies, `Location`, CORS and the other
  sensitive response headers; a nested `call_block` message gets it too. A
  `Continue` message also keeps the host's value of every sensitive request
  header the guest may not write, so a guest can neither forge nor drop the
  credentials the next flow step sees. A guest that reads a credential
  header, or sets a sensitive header on an error, must declare it. The
  default sensitive set is now public as
  `wafer_block::capabilities::DEFAULT_SENSITIVE_HEADERS`, and gains
  `refresh` (navigates like `Location`), `clear-site-data` (wipes the
  origin's cookies and storage) and `proxy-authorization`. Header-policy
  names are lowercased when deserialized, and declared ∩ config ∩ host
  narrowing compares them case-insensitively, matching enforcement.

  The `readable`/`writable` opt-in is **declared by the guest itself**, so
  it is a request, not a grant: an embedder that loads untrusted guests MUST
  approve or narrow it (the `capabilities` block-config subkey, or refusing
  the guest — impresspress refuses any sandbox guest that declares one,
  `CAP_HEADERS`), or load it with `WasmiBlock::load_with_capabilities*`,
  whose capabilities bound what the guest's declaration can obtain (see the
  capability-bound entry below).
- The capabilities an embedder loads a WASM guest with are an upper bound.
  `Wafer::seal` computed each block's capabilities from the guest's own
  `__wafer_info` declaration (∩ config) and installed them, replacing the set
  passed to `WasmiBlock::load_with_capabilities*` /
  `load_with_engine*` — so a guest loaded with `BlockCapabilities::none()`
  that declared `collections: Any`, raw SQL or a header opt-in ran with them.
  Every WASM guest now has a bound someone other than the guest stated, and
  enforces `bound ∩ declared ∩ config`; `Wafer::effective_capabilities`
  reports that set. The bound is the embedder's load capabilities
  (`load_with_capabilities*`, `load_with_engine*`), or a `wafer.lock`
  entry's new optional `capabilities` table (`LockfilePackage::capabilities`,
  written by the operator, not `wafer install`); a guest with neither —
  `WasmiBlock::load`, `load_from_bytes*`, a lockfile entry without
  `capabilities`, a block `seal()` downloads, and every `.wasm` registered
  through the Node/Go/C bindings — is bounded by its `capabilities` block
  config read as a full statement
  (`ConfigCapabilityOverrides::as_stated_bound`: an omitted field is denied),
  and runs with `BlockCapabilities::none()` when there is none. Such a guest
  also runs with `none()` before `seal()` (it ran `unrestricted()`).
  **An existing `capabilities` narrowing on such a guest changes meaning**:
  `{ "collections": { "Only": [...] } }` used to narrow only `collections`
  and leave every other declared field as declared; it is now the whole
  bound, so every field it does not list — storage, config, network,
  `callable_blocks`, headers, `schema` — is denied. Restate each field the
  guest needs. **An
  embedder that loaded guests with `load_from_bytes*` and relied on their
  declared capabilities must now state them**: pass a bound to
  `load_with_capabilities*`, state them in the block's `capabilities`
  config, or — for a guest it vetted or built — use the new
  `WasmiBlock::load_approving_declaration(bytes, limits)`, which makes the
  declaration the bound. Native discovery that loads its own built blocks
  with `load_from_bytes` — impresspress's
  (`impresspress-core/src/builder/registration.rs`) — must switch to
  `WasmiBlock::load_approving_declaration(bytes, wafer.resource_limits())`,
  or its blocks lose every capability. `wafer install` carries an entry's
  `capabilities` forward when it reinstalls or upgrades the block
  (`Lockfile::record_resolved`). New `Block::capability_bound` (default `None`)
  reports a block's embedder bound; `runtime_capabilities_mut` documents
  the rule. `BlockCapabilities` and `HeaderPolicy` derive `PartialEq`/`Eq`.
- `Wafer::seal` runs once. A second call — after a successful seal or a
  failed one — returns the new `RuntimeError::AlreadySealed`, and
  `Wafer::start`/`start_with_priority`, which seal, refuse a runtime already
  sealed. A second pass recomputed capabilities after the first had consumed
  each block config's `capabilities` narrowing, widening every narrowed WASM
  block back to its declaration; and a seal refused for rejected grants could
  be retried into success, since the refusal drained the rejections. New
  `Wafer::seal_state()` returns the outcome (`SealState::Unsealed`, `Sealed`
  or `Failed(reason)`). The Node binding's `start()` and the C ABI's
  `wafer_start` (Go `Start`) seal only a runtime `resolve()` has not, and
  after a failed `resolve()` report that failure again rather than start; a
  second `resolve()` / `wafer_resolve` reports `AlreadySealed`. Top-level
  dispatch — `Wafer::run`, `Wafer::run_block` (so `RuntimeHandle`, the
  Node `run()` and the C `wafer_run` / Go `Run`) — now answers a
  `FailedPrecondition` error unless `seal()` succeeded: an unsealed runtime
  ran blocks under their load-time capabilities with no grant gate, and a
  failed seal left it half-built. Embedders and tests that dispatched
  without sealing must call `seal()` (or `start()`) first.
  `Wafer::rebuild_all_blocks`, whose only use was populating the dispatch
  map without sealing, is removed; `seal()` does it.
- A block `seal()` downloads from the registry is admitted like a
  code-registered one. It is registered under its unversioned `{org}/{block}`
  (a version selects the artifact; the block's tables, config keys and grants
  belong to `{org}/{block}` whatever version runs), must report that name
  (`BlockNameMismatch`), and passes `BlockInfo::validate`, including the
  config-key prefix rule — it could declare another block's secret, or an
  unprefixed infrastructure key, and receive its value in its `Init`
  payload. A versioned reference (`acme/widget@1.2.0`) becomes an alias of
  the identity, aliases that targeted it are retargeted, and block config
  written under it becomes the block's config (config under both names is a
  `Config` error). One runtime holds one version of a block: a second
  version, or a reference whose name a registered block already has, is
  `DuplicateBlock` — refused before anything is fetched, as are a reference
  naming the admin block and one whose name or versioned reference is
  already an operator alias (`Config`; an alias is never overwritten). Blocks named only by a flow step, route or block config
  are now downloaded and registered before the grant gate and the capability
  computation, so their rejected grants refuse boot (`GrantsRejected`)
  instead of being dropped, and they get effective capabilities (they kept
  none). `block_infos()` and the startup snapshot therefore carry the
  registration name for every block.
- The `{ORG}__{BLOCK}__` config-key prefix rule moved into
  `BlockInfo::validate`, which now takes the registration name:
  `validate(&self, registered_name: &str)`, failing with the new
  `BlockInfoError::ConfigVarPrefix { block, key, prefix }`.
  `RuntimeError::ConfigVarPrefix` is removed — registration reports the
  failure as `RuntimeError::InvalidBlockInfo`. The prefix derivation is public
  as `BlockInfo::config_var_prefix(block_name)`.
- `InputStream` is no longer `Send` on `wasm32`. It boxes a `LocalBoxStream`
  there instead of a `BoxStream`, so that a JS-backed request body can be
  streamed to a block; native builds are unchanged and still hold a `Send`
  inner stream (pinned by a static assertion). wasm32 code that required an
  `InputStream` to cross a thread boundary has to drop that requirement. The
  `cfg` keys on `target_arch`, so it covers every wasm32 configuration,
  including the threaded ones (`+atomics` with shared memory) where `Send`
  would in principle mean something — wafer-run targets neither of those, and
  the rest of the block ABI (`MaybeSend`, `#[wafer_async_trait]`,
  `spawn_producer`) already dropped `Send` on wasm32 unconditionally. Almost
  every break is a compile error naming the type; the one that is silent is a
  downstream `Send` probe built on autoref or inherent-impl specialization
  (the pattern this PR's own fixture uses to assert `!Send`), which flips its
  answer for `InputStream` with no diagnostic at all. See the entry under
  **Added** for the reason.
- `Wafer::new` now takes `Arc<dyn ConfigSource>` instead of returning
  `Result<Self, RuntimeError>` with no config arg. Embedders must implement
  `ConfigSource` (or use `StaticConfigSource` for tests). The return type is
  still `Result<Self, RuntimeError>`.
- `Wafer::resolve()` is renamed `Wafer::seal()`. Same call-site semantics
  (call once after `register_block` / `add_block_config`, before first
  dispatch). Composite/uses expansion, capability resolution, and snapshot
  finalization still happen there.
- `Wafer::start_without_bind` is removed. Use `Wafer::seal()` instead, which
  no longer runs the eager `lifecycle(Init)` walk — per-block `Init` runs
  lazily on first dispatch per isolate.
- Boot-time required-config validation is removed. Missing required keys
  no longer prevent boot; they surface as a 5xx on first dispatch of the
  affected block. Use `Wafer::validate_all_block_configs()` for an explicit
  health check (intended for `/_health` routes).
- A block's `lifecycle(Init)` payload is its `add_block_config()` JSON with
  the values the registered `ConfigSource` resolves for its declared config
  keys laid over it (those win). The JSON also participates in
  composite/uses expansion and is surfaced via
  `RuntimeContext::block_configs()`.
- `RuntimeContext::make_context` gains an `init_breadcrumbs: InitStack`
  parameter. Update non-test in-tree callers if any exist (all in-tree
  callers were updated in this PR).
- `RuntimeContext` gains `slots` and `config_source` fields used by
  `dispatch_call` to lazily init callees of `call_block`. External code
  that constructs `RuntimeContext` literals (none today; the fields are
  `pub(crate)`) would need updating.
- `WebMcpRefusal::RecursiveSchema` and `WebMcpRefusal::OutputSchemaRecursive`
  are removed. A recursive endpoint schema is no longer a refusal: its cyclic
  definitions are published under `$defs` and referenced by `$ref`, so tools
  that used to be dropped now appear in the manifest. Two variants replace
  them: `CollidingDefinitions { names }` (two sources of one flat
  `inputSchema` keep definitions of the same name with different bodies — the
  tool is refused rather than misdescribed) and `SelectionNotFound` (a
  `ToolSelection` naming an endpoint no block declares). Matches on
  `WebMcpRefusal` must be updated; it stays `#[non_exhaustive]`.
- `BlockCapabilities::storage_folders` `Only` entries are folder PREFIXES, not
  exact object paths. An entry now admits itself and everything beneath it as
  a `/`-separated path, so `"uploads"` grants every key in `uploads/` (it
  previously granted only the literal resource `"uploads"`, which made
  folder-shaped grants unusable). Two rules bound the match: a resource with
  any empty, `.` or `..` segment is refused outright — nothing normalizes the
  path, so `site/jhg/../other` would otherwise pass a `site/jhg` grant while
  naming a sibling folder — and an `Only` entry that is empty or ends in `/`
  matches nothing. The storage handler rejects the same shapes one layer
  earlier with `InvalidArgument`. Operators who enumerated individual object
  paths to work around the old exact match should collapse them to the folder;
  operators who declared a folder expecting exact matching now grant its
  contents. `storage_folders` also narrows through
  `Allowlist::intersect_path_prefix` rather than a set intersection, so an
  override nested under a declared entry (or vice versa) survives as the
  narrower of the two instead of collapsing to deny-all.
- Rust API (not wire) changes from the `database` additions under **Added**:
  `wire::database::FilterDef` gains a `column` field and
  `AggregateColumnDef::{Sum, Avg}` gain `cast_as`, so struct literals need
  `column: None` / `cast_as: None`; `wafer_block::db::FilterTree` gains a
  `ColumnCompare` variant, `wire::database::AggregateColumnDef` and
  `service::AggregateColumnSpec` each gain a `SumWhere` variant, so
  exhaustive matches on any of the three need an arm;
  `service::AggregateColumnSpec::{Sum, Avg}` gain `cast_as`; and
  `wafer_sql_utils::aggregate::AggregateColumn::cast_as` is now
  `Option<CastType>` instead of `Option<String>`.
- `wafer_sql_utils::query::{build_select, build_select_with_condition,
  build_select_columns}` take a `unique_key: &[&str]` argument before
  `backend`: the table's primary-key columns, appended to the `ORDER BY` of a
  sorted or paged select (see the `database.list` entry under **Added**).
  Pass the key, or `&[]` for a table without one. A sorted `database.list`
  changes order only among rows that tie on every sort key, which previously
  came back in whatever order the backend produced. An unsorted but paged
  `list` (`limit` or `offset` set, `sort` empty) changes order for every
  row: it had no `ORDER BY` and came back in storage order, which on SQLite
  is insertion order, and now comes back in primary-key order. A caller that
  pages through a table without a `sort` and relied on insertion order must
  sort by its timestamp.
- `DbExec::create` and `create_many` mint a missing `id` as a UUIDv7
  (`Uuid::now_v7`) instead of a UUIDv4. The id is still a hyphenated UUID
  string, but it now leads with its creation time, so key order is creation
  order within a process and rows whose sort key ties list in the order they
  were created rather than a random one. The workspace `uuid` dependency
  gains the `v7` feature. On `wasm32-unknown-unknown` the v7 clock is
  `Date.now()` through uuid's `js` feature, which an embedding binary
  already enables for uuid's randomness source; a binary that picked another
  getrandom backend instead must enable `uuid/js` too, or `SystemTime` panics
  when the first id is minted. The policy is public as
  `wafer_core::interfaces::database::mint_record_id`, for a backend that
  inserts rows through its own path. A minted id now reveals its record's
  creation time, carries about 74 random bits and is near-sequential with
  ids minted in the same millisecond, so a record id must never serve as a
  bearer secret.
- `database.aggregate` rejects `Avg` with `cast_as: "BIGINT"` as
  `InvalidArgument`; `Avg` casts to `DOUBLE PRECISION` only. This is a
  wire-visible validation change to the `cast_as` addition under **Added**: a
  `BIGINT` cast rounds a non-integral value on Postgres and truncates it on
  SQLite, and an average is rarely integral, so the same request answered
  differently per backend. Read the average as a float and convert it where
  the rounding rule is yours to choose. `Sum` and `SumWhere` keep both
  types.
- `wafer_sql_utils::aggregate::AggFunc` gains `SumOrZero`
  (`COALESCE(SUM(...), 0)`), so exhaustive matches need an arm.
- Go SDK: `WaferError.Meta` is removed and `WaferError.DetailCode` added
  (see **Fixed**). No merged version of the embedder emitted meta inside
  the `error` object, so code that read `Meta` always saw no entries; read
  `DetailCode` instead, and an error's response headers from `Result.Meta`
  (see **Added**).
- `DatabaseService` gains two REQUIRED methods, `create_many` and `batch`,
  with no defaults: a backend that cannot make the writes atomic must say so
  with an error rather than inherit a loop that leaves half of them applied.
  Every implementor must add both — out-of-tree adapters (a Cloudflare D1 or
  browser sql.js service) and test fakes included — and a
  `forward_database_service!` ledger must list `create_many` (after `create`)
  and `batch` (after `aggregate`). `DbExec` gains a required
  `run_transaction(&[TxOp]) -> Vec<TxResult>` primitive: run the statements
  in one transaction, all or nothing (a backend with an atomic native batch,
  like D1's `batch()`, implements it with that). `DbExec::create_many` now
  runs as one `run_transaction` instead of one `run_batch`, so it is atomic
  on every backend, and it accepts rows with different column sets instead
  of refusing them. `ServiceOp::DATABASE_OPS` gains `DATABASE_CREATE_MANY`
  and `DATABASE_BATCH`, so any table kept in step with it needs both.
- `DatabaseService` gains two more REQUIRED methods, `insert_guarded` and
  `update_guarded`, again with no defaults: the check-and-write must be
  atomic, and only the backend knows how. Every implementor must add both,
  out-of-tree adapters and test fakes included; a `DbExec` backend forwards
  to the shared defaults, which run through its `run_transaction` (so a D1 or
  sql.js adapter gets them once `run_transaction` is atomic). A
  `forward_database_service!` ledger lists them after `batch`, in that order.
  `ServiceOp::DATABASE_OPS` gains `DATABASE_INSERT_GUARDED` and
  `DATABASE_UPDATE_GUARDED`.
- `DatabaseError` gains `AlreadyExists(String)`, so an exhaustive `match` on
  it needs an arm. A write that duplicates a primary or unique key is now
  that variant on SQLite (`SQLITE_CONSTRAINT_UNIQUE`/`_PRIMARYKEY`) and
  PostgreSQL (SQLSTATE `23505`), and reaches the caller as
  `ErrorCode::AlreadyExists` ("a record with this key already exists")
  instead of `ErrorCode::Internal` ("internal database error") — for
  `create`, `create_many`, `batch`, `upsert`, `update` and the guarded ops
  alike. A caller that treated a duplicate as `Internal` must match
  `AlreadyExists`. An out-of-tree adapter must map its driver's
  unique-violation the same way (D1 reports it only as the text
  `UNIQUE constraint failed`). Other constraint violations stay `Internal`,
  and so does a PostgreSQL `23505` on a `pg_catalog` index — two sessions
  creating the same table at once collide on `pg_type_typname_nsp_index`,
  which is a DDL race, not a taken key.
- The embedder wire format (`embed::output_to_json`, consumed by `wafer-ffi`,
  `wafer-run-node` and the Go SDK) emits a **projection** of each terminal's
  meta instead of all of it: every action but `drop` carries a `meta` object
  holding only the canonical response keys — `resp.status`,
  `resp.header.*`, `resp.set_cookie.*`, `resp.content_type` — under their
  own names. A host that read any other key (`http.header.*`, `http.method`,
  `http.path`, `req.*`, `auth.*`) off `meta` no longer finds it; those were
  request state, never part of the response (see **Fixed**).
- The `continue` action replaces its `message` object with the follow-up
  message's `kind` at the top level: `{"action":"continue","kind":"...",
  "meta":{...}}`. `message.meta` was also the one place the wire encoded meta
  as a list of `{key,value}` objects rather than an object; there is now one
  encoding. Read `kind` instead of `message.kind`.
- Go SDK: `Message` matches the runtime's `Message` — `Kind` plus an ordered
  `Meta []MetaEntry`, and no `Data` (the FFI's `wafer_run` is body-less).
  `NewMessage` takes only a kind. A `Message` with a nil `Meta` encodes it
  as `[]`, which `wafer_run` requires; `null` is rejected. The previous
  shape (`Data []byte`, `Meta map[string]string`) could not be deserialized
  by the runtime at all, so `Run` failed on every call that reached it.
- Go SDK: `Result` matches the wire format — `Body` / `BodyBase64` / `Kind` /
  `Meta` / `Error` at the top level, plus `ActionHalt` and `IsHalt()`. The
  `Response` type and `Result.Response` field are removed; no runtime version
  ever emitted a `response` object.
- `wafer_core::interfaces::auth::service::AuthError` gains a
  `Backend(WaferError)` variant, and a `match` over `AuthError` must handle
  it. An `AuthService` implementation wraps a failed lower-level call
  (database, storage, another block) in it — `.map_err(AuthError::Backend)`
  — and the auth block answers with that `WaferError` unchanged, code,
  message and meta included; the `Init` lifecycle hook passes it through the
  same way. A WRAP `PermissionDenied` or a `ResourceExhausted` from the
  database under `auth.require_user` / `require_token` / `require_role` /
  `user_profile` now reaches the caller with its code, where
  `AuthError::Internal(e.to_string())` turned every one of them into
  `Internal`. `Internal` is now for faults of the auth service itself.

- WRAP checks name a `ResourceAccess` (`Read` / `Append` / `Write`) instead
  of an `is_write: bool`: `Context::check_resource_access(resource,
  resource_type, access)`, `wrap::check_access(caller, resource, access, ..)`,
  and the `(resource, resource_type, access)` tuple the
  `decode_and_authorize*` closures return. `false` becomes
  `ResourceAccess::Read` and `true` becomes `ResourceAccess::Write`, which
  authorize exactly as before; `Append` is new (see **Added**). Every
  `Context` implementation and every direct caller has to be updated.
  `decode_and_authorize_all`'s closure now returns a `Result`, so it can
  refuse a request before any check runs, as `decode_and_authorize_checked`'s
  does. `Context` gains a required `resource_access_admitted` — the same
  decision as `check_resource_access`, without logging a denial. EVERY
  implementation must add it: an enforcing context answers from its own
  check, a context that forwards `check_resource_access` to an inner one
  forwards this too, and a mock answers by its policy. It has no default
  because a wrong `false` would silently hold every writer behind that
  context to the append-only insert rules.
- `ResourceGrant::write` is a `GrantWrite` (`None` / `Full` / `Append`)
  instead of a `bool`. `None` and `Full` encode as `false` and `true`, so
  every existing grant keeps its wire form and its meaning; code that reads
  or builds the field changes (`write: true` → `write: GrantWrite::Full`).
- `DatabaseService` gains a required `schema_columns(table)`, and
  `forward_database_service!`'s ledger a `schema_columns` entry after
  `schema_table_exists`. A `DbExec`-backed service forwards it.
- `Wafer::add_wrap_grants` returns `Result<(), RuntimeError>`: it rejects the
  whole call with `GrantsRejected` when any grant fails
  `ResourceGrant::check_shape`, where it used to install grants unchecked.
- `CryptoService` has no master-key `sign` / `verify`, and `sign_for` /
  `verify_for` are required. Their defaults fell back to the shared master
  key, so an implementation that forgot them signed every block's tokens
  under one key without a compile error. Implementations delete `sign` /
  `verify` and implement `sign_for` / `verify_for` (a derived key per
  `block_id`; `wafer_block_crypto::primitives::derive_block_key` is the
  shared derivation). The crypto handler answers a `crypto.sign` /
  `crypto.verify` that arrives with no calling block `PermissionDenied`
  instead of using the master key.
- `CryptoError` gains `MalformedHash`: a stored password hash that is
  malformed, names an unsupported scheme, or carries costs above the new
  ceilings. The crypto handler maps it to `Internal`, so `crypto.compare_hash`
  answers `Unauthenticated` only for a wrong password; an unsupported or
  corrupt stored hash (a bcrypt row, a truncated PBKDF2 digest) used to be
  `Unauthenticated` too. `wafer_block_crypto::primitives` returns it where it
  returned `VerifyError` (`pbkdf2_verify`, `verify_password_any_scheme`) and
  `HashError` (`verify_password`); `VerifyError` is JWT-only. Callers that
  treat every `compare_hash` error as bad credentials should branch on the
  code.
- Stored-hash verification refuses costs above fixed ceilings: argon2
  `m` > `ARGON2_MAX_M_COST` (47104 KiB, 46 MiB — OWASP's largest argon2id
  memory cost), `t` > `ARGON2_MAX_T_COST` (20), `p` > `ARGON2_MAX_P_COST`
  (10), PBKDF2 `i` > `PBKDF2_SHA256_MAX_ITERATIONS` (6,000,000), as
  `MalformedHash` before any derivation runs. The costs come from the
  stored string: `t` or `i` at `u32::MAX` pinned a thread for hours, and
  `m` at `u32::MAX` asked for terabytes. `pbkdf2_hash` refuses the same
  ceiling (`HashError`), so a `PasswordScheme::Pbkdf2Sha256` above it
  fails at hash time.
- The argon2 memory ceiling is the same on every target and sized for a
  Cloudflare Workers isolate (128 MB, shared with the module and the
  application). wasm32 linear memory never shrinks, and an allocator handed
  a slightly larger request than the block it just freed takes fresh
  memory: two stored hashes just under a ceiling grew it by twice the
  ceiling (91 MiB for m=46000 then m=47104, measured). Every argon2
  derivation now runs in a buffer of one of three fixed sizes
  (`ARGON2_MEMORY_CLASSES`: 4096, 19456 and 47104 KiB), taking the smallest
  that holds it; on wasm32 each is allocated once and kept, so the argon2
  share of linear memory is at most their sum, 69 MiB (69.25 MiB measured
  with allocator overhead), whatever hashes an isolate verifies.
  `scripts/check.sh wasm` measures this under node. The blocks a
  derivation used are zeroised when it returns, so a kept buffer holds no
  password-derived state between logins. A derivation smaller
  than a class takes that class's whole buffer: on native targets, where
  the buffer is freed after each call, a hash at m=20000 briefly holds
  46 MiB. Both presets `hash_password` writes (19 MiB and 4 MiB) are under
  the ceilings, held there by a compile-time assertion, so no credential
  this crate wrote is refused. A hash imported from elsewhere above 46 MiB
  is — including argon2-cffi's and RFC 9106's 64 MiB default — and has to
  be re-hashed within the ceiling (or reset) before it can be used.
- The auth service authorizes its caller per operation. Its handler never
  checked who was asking, so any block that could reach `wafer-run/auth`
  could read any user's email, role and orgs through `auth.user_profile`.
  `interfaces::auth::handler::handle_message` now takes the block's `ctx`
  (`handle_message(service, ctx, msg, body)`) and checks every op with
  `ctx.check_resource_access` against a new `ResourceType::Auth` resource in
  the auth block's own namespace: `wafer_block::wrap::AUTH_USER_PROFILE_RESOURCE`
  (`wafer_run__auth__user_profile`) admits the auth block, the admin block,
  or a caller holding a grant on it — and, the resource being namespaced,
  only the auth block can declare that grant (return it from
  `AuthService::grants`, e.g.
  `ResourceGrant::read("acme/directory", AUTH_USER_PROFILE_RESOURCE).typed(ResourceType::Auth)`;
  an untyped grant on `wafer_run__auth__*` covers it too). The credential
  ops `auth.require_user` / `require_token` / `require_role` resolve the
  credential the caller forwards, so any attributable caller may use them
  without a grant (`wrap::is_auth_credential_resource`); a call with no
  calling block is refused. `ServiceOp::AUTH_OPS` lists the auth ops.
- The logger attributes every record to its caller and keeps it on one
  line. It recorded no caller and wrote a block's message verbatim, so any
  block could write lines that read as another component's, including whole
  forged records after a newline. `LoggerService`'s four methods take the
  caller first (`fn info(&self, caller: Option<&str>, msg: &str, fields:
  &[Field])`): the handler passes `ctx.caller_id()`, the runtime's
  registered name for the calling block, and escapes control characters,
  U+2028/U+2029, bidirectional embeddings, overrides and isolates
  (U+202A-U+202E, U+2066-U+2069), zero-width and directional marks
  (U+200B-U+200F) and U+FEFF in the message and in every field key and text
  value (`interfaces::logger::service::escape_log_text`; `\` is escaped too,
  so the result is unambiguous), handing the fields over in key order.
  `interfaces::logger::handler::handle_message` takes the block's `ctx`.
  `TracingLogger` emits no event message: an event is three string fields,
  `caller` (`-` for no caller), `msg` and `fields`, which the text
  formatter writes quoted, so a message spelling `caller=…` cannot read as a
  field of its line (a JSON formatter now shows the block's text under
  `msg`, not `message`). `fields` is rendered by the new
  `interfaces::logger::service::RenderedFields`, which quotes any key or
  value that is empty or holds a space, `=` or `"`; text-rendering
  `LoggerService` implementations can use it too. Every `LoggerService`
  implementation must add the parameter and record the caller.
- The llm, image and embedding services authorize their caller per
  operation. Their handlers never checked who was asking, so any block that
  could reach `wafer-run/llm` could spend a paid provider's quota or unload
  a model other blocks were using, and any block could embed through an
  embedding block. `interfaces::llm::handler::handle_message`,
  `interfaces::image::handler::handle_message` and
  `interfaces::vector::handler::handle_embedding_message` now take the
  serving block's `ctx` and registered name (`handle_message(service, ctx,
  block, msg, body)`) and check every op with `ctx.check_resource_access`
  against a resource in that block's own namespace, typed by the new
  `ResourceType::Llm`, `ResourceType::Image` and `ResourceType::Embedding`.
  A model is `wafer_block::wrap::model_resource(block, backend_id,
  model_id)` (`wafer_run__llm__{backend_id}/{model_id}` for `wafer-run/llm`):
  `chat`, `generate` and `status` read it, and `load_model` and
  `unload_model` write it, since they change what every other caller finds
  loaded. `list_models`, `embedding.embed` and `embedding.count_tokens` read
  `wafer_block::wrap::op_resource(block, op)` (`wafer_run__llm__list_models`,
  `{prefix}embed`, `{prefix}count_tokens`). The serving block and the admin
  block are admitted; any other caller needs a grant, which only the serving
  block can declare: return it from the new `LlmService::grants`,
  `ImageService::grants` or `EmbeddingService::grants` (default empty), or
  on the routers with `MultiBackendLlmService::grant` /
  `MultiBackendImageService::grant` (a router also declares each registered
  backend's grants). For example
  `ResourceGrant::read("acme/chat", "wafer_run__llm__*").typed(ResourceType::Llm)`
  lets `acme/chat` use and list every model but not load or unload one. A
  `backend_id` containing `/` is refused with `InvalidArgument`, because
  the resource would not name one backend. `ServiceOp::EMBEDDING_OPS`,
  `LLM_OPS` and `IMAGE_OPS` list the ops, and every `service_block!` block
  has a `NAME` constant.

- `wafer-client-js` 3.0.0: `WaferErrorCode` is the server's error vocabulary
  as it is on the wire — the `ErrorCode` variant names (`'NotFound'`,
  `'Unauthenticated'`, `'Unimplemented'`, ...) — plus the client's own
  `'timeout'`, `'aborted'` and `'network_error'`. A request the caller's
  `signal` aborts rejects with `'aborted'` (was `'network_error'`). The snake_case members (`'not_found'`,
  `'internal_error'`, ...) are gone, as is the `(string & {})` widening, so
  `err.is('not_found')` (never true: the server sends `"NotFound"`) no longer
  compiles; write `err.is('NotFound')`. A non-2xx body without an `"error"`
  field gives `'Internal'` (was `'internal_error'`); an `"error"` the client
  does not know gives `'Unknown'` (was the raw string; `err.data` keeps the
  body). New exports: `WAFER_SERVER_ERROR_CODES` and `WaferServerErrorCode`
  (generated from `ErrorCode`; a `wafer-block` test fails when they drift,
  `WAFER_REGENERATE=1` rewrites them), `WaferClientErrorCode` and
  `isWaferServerErrorCode`.
- `wafer_core::discovery::generate_openapi` and `generate_agent_card` take
  the caller's level and an effective-auth resolver, as `generate_webmcp`
  does: `generate_openapi(blocks, caller, effective_auth, project_name,
  project_description, server_url)`, and the same for
  `generate_agent_card`. An endpoint whose `effective_auth(block, ep)` is
  above `caller` is left out of both documents; they described every
  schema-carrying endpoint to whoever asked. The OpenAPI `security`
  requirement follows the effective level too, so a `Public` endpoint the
  router gates as `Admin` is published with `bearerAuth`. A consumer
  whose routing never raises an endpoint's declared level passes
  `|_, ep| ep.auth`; one that already narrowed `BlockInfo::endpoints` to
  the caller before calling can pass that narrowing's resolver and drop
  its own filter.
- `Wafer::seal()` refuses boot with the new
  `RuntimeError::DuplicateEndpointRoutes` when two endpoints, in one block
  or across blocks, declare the same method on the same route. Routes are
  compared by the new `wafer_block::route_shape`, which writes every
  `{name}` placeholder segment as `{}` and every `{name...}` rest
  placeholder as `{...}`, so `GET /items/{id}` and `GET /items/{item_id}`
  collide while `GET /items/{id}` and `GET /items/{key...}` do not; a
  pattern ending in `/**` is compared as written. The error lists every
  collision with each claimant's block and declared path
  (`DuplicateEndpointRouteError`, `RouteClaimant`). Such a pair used to
  boot with nothing reported: `generate_openapi` kept only the later of
  two identical paths, and published a pair differing only in placeholder
  names under two path keys that describe one route. A `match` over
  `RuntimeError` needs an arm for the new variant.

### Security

- A string column default reaches PostgreSQL DDL as an `E'…'` escape string
  with every backslash and quote doubled. It was a plain `'…'` literal with
  only quotes doubled, which reads the value correctly only while the
  session's `standard_conforming_strings` is on; a server configuration or a
  connection option can turn it off, and then a default taken from a
  `database.ensure_table` / `database.add_column` request could end the
  literal early and have the rest of the value parsed as SQL. The literal now
  reads back as exactly the value whatever `standard_conforming_strings` and
  `backslash_quote` are set to, under the UTF8 `client_encoding` sqlx sets
  when it connects. SQLite rendering is unchanged (its literals have no
  backslash escape).
- `ddl::build_create_table` and `ddl::build_add_column` refuse a default no
  SQL literal can express — a string holding a NUL character or a non-finite
  float — with `SqlBuildError::InvalidDefault`, which the database handler
  returns as `InvalidArgument`.

### Added

- `database.batch` takes a filtered delete, `DeleteWhere { collection,
  filters }`, with the semantics of `database.delete_where_count`: it
  removes every row the AND-combined leaf filters match (no filters: every
  row), reports `DeletedWhere { rows_affected }`, matches nothing on a
  missing table (0, without aborting the batch) and refuses an unknown
  filter column. It runs inside the batch's one transaction on every
  `DbExec` backend, so "delete the matching rows, then create their
  replacements" is all-or-nothing: a replacement that fails leaves the
  deleted rows in place. It needs write access on its collection — an
  append-only or read-only grant refuses the whole batch — and counts as
  one statement against the backend's statement budget however many rows it
  matches.
- `wafer-run/security-headers` has an `allow_blob_workers` step config
  (`true`/`false`, default `false`; any other value fails Init). `true` adds
  `blob:` to `worker-src` and to no other directive, starting from the
  directive's fallback (`child-src`, then `script-src`, then `default-src`)
  when the policy has none, so workers keep the sources they had. It is for an
  embedder whose own page must start workers from blob URLs it builds (an
  in-browser toolchain spawning sub-workers); the operator `csp` value still
  cannot add `blob:` to a worker or script directive.

- Embedder bindings can bound a WASM guest's capabilities:
  `embed::register_block_path(wafer, name, path, capabilities_json)`, and the
  `wafer_register_block` C export, `WaferRuntime.registerBlock(name, path,
  capabilities)` in `wafer-run-node` and `Wafer.RegisterBlock` in the Go SDK
  over it. The JSON is a `BlockCapabilities` object (an omitted field
  denies) passed to `WasmiBlock::load_with_capabilities_and_limits`, so the
  guest runs under that bound ∩ its declaration. A `.wasm` registered through
  `register_path` / `wafer_register` / `register` / `Register` still has no
  bound and, since the bindings cannot set block config, runs with
  `BlockCapabilities::none()`. Both load with `Wafer::resource_limits()`.

- `vector.rename_index` (`wire::vector::RenameIndexRequest { from, to }`,
  `ServiceOp::VECTOR_RENAME_INDEX`, `VectorService::rename_index`,
  `clients::vector::rename_index`, and `rename_index` in the guest SDK) moves
  an index created under a mixed-case name, which index names may no longer
  have, to its lowercase spelling: `to` must be a valid index name and
  `from` the same name with some letters uppercase
  (`wire::vector::is_legacy_spelling_of`, checked by
  `interfaces::vector::check_rename`); this is the only op that accepts
  such a name, and it matches it exactly. The handler refuses other names
  as `InvalidArgument` before authorizing, then requires write access to
  both names. `NotFound` means no index is named exactly `from` (a
  startup migration treats that as done when `to` exists);
  `AlreadyExists` means `to` is taken — two spellings are never merged.
  The SQLite backend moves the index in one transaction through a
  `{to}-rename` staging stem, because SQLite folds identifier case: the
  `_meta` and FTS5 tables are renamed, and the vec0 table, which sqlite-vec
  cannot rename, is copied rowid for rowid into new vec0 tables declared
  with the old module arguments (`wafer_sql_utils::vector::
  VectorIndexRename`, `vec0_module_args`). There is no Postgres vector
  backend.
- CI builds and tests the embedder bindings (the `bindings` job in
  `ci-jobs.yml`, gated by `ci-ok`; `scripts/check.sh bindings`): `wafer-ffi`
  is built and driven through its `extern "C"` functions (register a wasm
  block and a flow, seal, run, stop, free), the Go SDK runs `gofmt`,
  `go vet` and `go test` linked against that `libwafer_ffi`, the Node addon
  is built from source by `npm run build-test` and a `node --test` smoke
  test runs a flow through it, and `packages/wafer-client-js` is
  typechecked, tested (vitest) and built. None of these had a CI job
  before. The root `package.json` no longer lists the `crates/wafer-site`
  workspace, which moved to its own repository.
- CI runs on every pull request and every push to `main`; the path filters
  that let a `rust-toolchain.toml`-, `rustfmt.toml`- or `.cargo/`-only change
  merge unbuilt are gone. A `ci-ok` job (`ci / ci-ok`) passes only when every
  other CI job succeeded — a failed, cancelled or skipped job turns it red —
  so it is the single check branch protection requires. Shellcheck moved
  into the same job set, and a `Workflow Lint` job runs actionlint plus
  `scripts/lint-workflows.sh`, which rejects any action not pinned by full
  commit SHA and any job missing from `ci-ok`'s `needs`. `cargo audit` also
  runs weekly on its own schedule (`audit.yml`) with a pinned cargo-audit,
  and Dependabot keeps the pinned actions and `Cargo.lock` current. Every
  advisory ignored in `.cargo/audit.toml` must carry a `REASON:` and a
  `REMOVE WHEN:` line (checked by the same lint script); the
  RUSTSEC-2026-0097 ignore is dropped instead, because `rand` 0.8.6 and
  0.9.3 fix it and `Cargo.lock` now uses them. The
  pre-commit hook formats with nightly rustfmt, as CI checks. The
  `release.yml` workflow is removed: it had never run, could not pass (its
  `cargo test --workspace` skipped the fixture build), and pushed manifests
  to a `wafer-run/registry` repository that does not exist, with a token
  that was never configured. `RELEASE.md` describes the manual release.

- The embedder wire format's `error` action carries a top-level `meta`
  beside `error`: the error's response-meta projection, the same keys every
  other action carries. An embedding host can now emit the `Retry-After` /
  `X-RateLimit-*` headers a 429 carries, as the native HTTP boundary does.
  No merged version of the embedder emitted an error's meta in any form, so
  this is new wire surface; request state in the error's meta never reaches it.
- `database.aggregate` output casts: `AggregateColumnDef::Sum` takes an
  optional `cast_as` of `BIGINT` or `DOUBLE PRECISION`, `Avg` of
  `DOUBLE PRECISION` only (ASCII case-insensitive), rendered as
  `CAST(<aggregate> AS <type>)`. The handler parses it against that
  allowlist (`wafer_sql_utils::aggregate::CastType`) and rejects anything
  else as `InvalidArgument` — the type name is spliced into SQL text, so it
  is never passed through. Postgres widens `SUM(<bigint>)` to `NUMERIC`,
  which decodes as a JSON float; `BIGINT` makes an integral sum read as an
  integer on every backend. A non-integral value is rounded by the `BIGINT`
  cast on Postgres and truncated on SQLite, so cast only a sum of integers.
- `AggregateColumnDef::SumWhere { field, when, alias, cast_as }`:
  `COALESCE(SUM(CASE WHEN <when> THEN field ELSE 0 END), 0)`, the sum of a
  column over the rows matching a predicate — `0`, not `NULL`, when nothing
  non-null is summed (no matching row in a group, matching rows whose
  `field` is `NULL`, or an ungrouped query over no rows) — validated like
  `CaseWhenSum` (an empty `when` is `InvalidArgument`) and cast like `Sum`.
  Builder: `AggregateColumn::sum_where`. On SQLite that `0` is the integer
  `0` even over a `REAL` column (Postgres gives the column's type), so a
  sum of a floating-point field that no row matches decodes as a JSON
  integer there; pass `cast_as: "DOUBLE PRECISION"` to read a float on
  every backend.
- Column-to-column filters: `FilterDef.column` compares `field` to another
  column of the same row instead of to `value` (`eq`/`neq`/`gt`/`gte`/`lt`/
  `lte` only). Both identifiers are validated; a non-null `value` alongside
  it, or another operator, is `InvalidArgument`. Accepted wherever a filter
  tree is — `list`, and the `CaseWhenSum` / `SumWhere` predicates — and
  rejected as `InvalidArgument` by the ops that take flat filters (`count`,
  `sum`, the `*_where` family, and `aggregate`'s own `filters`).
- All three are wire-additive: every new field defaults and is omitted from
  the encoding when unset, so existing requests encode exactly as before. An
  older runtime ignores `cast_as` (the result comes back uncast) and
  `column` (the leaf compares `field` to `NULL`, which matches no row); it
  rejects `SumWhere` as an unknown variant. Neither ignored field fails the
  request, so a caller that sends them to an older runtime gets a wrong
  answer, not an error: a column-compare leaf makes a read silently smaller
  (a `list` or `count` misses the rows it should match, a `CaseWhenSum`
  counts fewer), and makes a flat-filter write — which the newer runtime
  rejects — report success while changing nothing (`update_where_count`
  and `delete_where_count` return `0`, `take_where` returns no rows).
  Upgrade the runtime before sending either field.
- `database.list` orders deterministically: a sorted or paged select
  (`sort` non-empty, or `limit`/`offset` set) ends its `ORDER BY` with the
  table's primary key, every column of it in key order, skipping any the
  sort already names, in the direction of the last sort term (ascending with
  no sort). Rows that tie on the sort key — a `created_at` stamped to the
  second, a status column — therefore come back in one order on every query,
  so `limit`/`offset` pages are disjoint and complete; before, a tie could
  land on two pages or on none. The key is introspected
  (`introspect::build_list_primary_key`, `DbExec::get_primary_key`) and
  memoized in the backend's `SchemaCache`, in STRICT_SCHEMA mode too, so a
  warm backend pays nothing; a backend without a cache pays one catalog read
  per sorted or paged `list`. An empty key is memoized only for a table known
  to exist (a keyless table costs one existence probe the first time), so a
  `list` that runs before the migration creating its table does not pin "no
  key" for the life of the cache. Postgres reads the key from
  `pg_catalog.pg_index` (`indisprimary`), not
  `information_schema.table_constraints`, which hides constraints from a
  role that only holds `SELECT` on the table. An unsorted, unpaged `list`
  looks nothing up and has no `ORDER BY`. A table without a primary key is
  ordered by the sort alone. `aggregate` is unchanged — `query::apply_order`
  still emits the sort only, because a key column outside the `GROUP BY` is
  not a valid sort key there; the row selects use
  `query::apply_order_with_unique_key`.
- CI runs the shared `DatabaseService` conformance suite against a live
  PostgreSQL 16 service container (`scripts/check.sh postgres`, the
  `PostgreSQL Conformance` job). It previously ran only by hand.

- `InputStream::from_stream` and `from_stream_with_cancel` bound their
  stream by `MaybeSend + 'static` instead of requiring `Send` outright, and `InputStream` boxes its inner stream as a
  `LocalBoxStream` on `wasm32`. On native, `MaybeSend` *is* `Send` and
  nothing changes.

  On `wasm32` every byte stream a host can hand a block is `!Send`:
  `worker::ByteStream` and `wasm_streams`' `IntoStream` both hold a `JsValue`,
  which belongs to the isolate that created it. The old `Send` bound therefore
  left a Cloudflare Workers or browser service-worker adapter with no way to
  pass a request body on as a stream at all — it had to read the body to a
  `Vec<u8>` first and cap how large that buffer could get, which is why
  downstream adapters buffer uploads and reject anything over a few MiB while
  the storage half of the path (`clients::storage::put_stream` through to a
  backend that overrides `StorageService::put_streaming`) has always been able
  to stream. An adapter can now do:

  ```rust,ignore
  // `body` is a `worker::ByteStream` — `!Send`, and no longer a problem.
  let input = InputStream::from_stream(body.map(|chunk| {
      chunk.map_err(|e| WaferError::new(ErrorCode::Unavailable, e.to_string()))
  }));
  wafer.run_block("files", msg, input).await
  ```

  What a local body survives, end to end on `wasm32`: `Wafer::run_block` and
  `RuntimeContext::call_block` (which pass the stream through untouched),
  `clients::storage::put_stream` (which re-frames it behind a header chunk and
  keeps its cancellation token), and the `service_block!` streaming-ingress arm
  into `StorageService::put_streaming`. Whether the body is still a stream at
  that last hop is the backend's choice: `put_streaming`'s trait default
  collects it and forwards to `put`, so only an overriding backend streams —
  in-tree, only `wafer-block-local-storage`.

  Two consumers collect the whole body regardless, as they did before and on
  every target: the WaferFlow executor (`execute` reads the flow input into
  the accumulator, unconditionally — no flow shape avoids it) and the wasmi
  guest bridge (the guest ABI takes a `Vec<u8>`). Streaming ingress therefore
  means dispatching to a native block directly — not through a flow, and not
  into a wasm guest. This change removes the type-level blocker; an embedder
  whose every request goes through a flow needs flow-level streaming ingress
  (or a non-flow dispatch for the streaming route) before any of this reaches
  a block as a stream.

- Static block registration now works on `wasm32`. `register_static_block!`
  used to expand to nothing there — `linkme`'s distributed slice is a linker
  section that target does not have — so a wasm32 embedder booted with an
  empty block registry and had to re-list every block crate by hand as
  `register_block` calls, mirroring its own `use_static_blocks!` anchors with
  nothing keeping the two lists in step. The `wasm32` arm now emits a
  `pub const __WAFER_STATIC_BLOCK` per crate, and `use_static_blocks!` emits
  `pub const WAFER_STATIC_BLOCKS: &[&StaticBlockRegistration]` into the
  invoking module holding one entry per named crate. New
  `Wafer::register_static_blocks(&[&StaticBlockRegistration])` takes that
  list. `WAFER_STATIC_BLOCKS` is **empty on every target where `linkme`
  works**, so the call site needs no `cfg`:

  ```rust,ignore
  wafer_block::use_static_blocks!(wafer_block_cors, wafer_block_router);
  // ...
  wafer.register_static_blocks(WAFER_STATIC_BLOCKS)?;   // no-op off wasm32
  ```

  `StaticBlockRegistration` (the record type) is now exported on every
  target; `STATIC_BLOCK_REGISTRATIONS` (the `linkme` slice) stays
  native-only. Two consequences worth knowing: `use_static_blocks!` now adds
  a `WAFER_STATIC_BLOCKS` item to the module that invokes it (so
  `wafer_flow_http_server::WAFER_STATIC_BLOCKS` exists), and on `wasm32` a
  crate may invoke `register_static_block!` at most once, since a second
  invocation would define `__WAFER_STATIC_BLOCK` twice. Native builds take
  any number, as before.
- Structured schema operations on the database interface —
  `database.ensure_table`, `database.add_column`, `database.drop_table` and
  `database.table_exists` — taking a wire `TableDef` / `ColumnDef` the host
  converts to `wafer_schema` types and builds the statement from. Wrapped by
  `wafer_core::clients::database` and the Rust SDK.
- `BlockCapabilities::schema` (`bool`, default `false`, `true` in
  `unrestricted()`): gates the three structured write ops above via the new
  `wafer_block::wrap::SCHEMA_RESOURCE` (`__schema__`) sentinel, which follows
  the same WRAP rule as `__ddl__` — any attributable caller. It does NOT grant
  raw `database.ddl`, and `ddl` does not grant it: a sandboxed block can hold
  `schema: true, ddl: false` and still create its own tables. Overridable from
  block config like every other capability.
- `__wafer_host_codec` — an optional guest export negotiating the host-call
  payload codec. Returning `1` (JSON) makes the host transcode every host-call
  request body and response frame between JSON and MessagePack, so a
  dependency-free, std-only guest can drive the database / storage / config
  services. Attachments (`__wafer_host_stream_attach`) and attachment lookup
  remain MessagePack-only, and a JSON guest has no raw request-body path (the
  streaming-upload direction) — both answer `InvalidArgument`.
- `frame.encoding = raw` stream meta marker
  (`wafer_block::stream::raw_frames_marker`): a handler emits it to declare
  that every frame after it is opaque application bytes rather than a wire
  DTO, so a consumer that re-encodes frames forwards them verbatim. Emitted by
  `storage.get` / `storage.get_streaming` between the `ObjectInfo` header and
  the object body — no sniffing.
- `wafer_core::discovery::generate_webmcp_selected` + `ToolSelection`: build a
  page-scoped WebMCP manifest from an explicit endpoint allowlist (block,
  method, path, published name and description) instead of from whatever each
  block's author opted into globally. Runs the same name, auth and schema
  rules as the global manifest.
- `/openapi.json` hoists every schema's `$defs` into `components.schemas` and
  rewrites `#/$defs/X` references to `#/components/schemas/X`. Two different
  definitions that share a name are disambiguated with a content-hash suffix;
  identical bodies merge. A document with no `$defs` anywhere is unchanged.
- `cache_mode` config key on `wafer-run/web`: `"normal"` (default) or
  `"no-cache"`, which forces `Cache-Control: no-cache` on every response for
  sites edited live.
- `frame_ancestors` config key on `wafer-run/security-headers`: `"none"`
  (default) or `"self"`, which relaxes both the CSP `frame-ancestors`
  directive and `X-Frame-Options` for same-origin framing.
- `cross_origin_isolation` config key on `wafer-run/security-headers`:
  `"none"` (default: no `Cross-Origin-Opener-Policy` /
  `Cross-Origin-Embedder-Policy` headers, a true no-op) or `"credentialless"`
  / `"require-corp"`, both of which set `Cross-Origin-Opener-Policy:
  same-origin` and make the document `crossOriginIsolated`.
  `credentialless` keeps cross-origin no-cors subresources loadable — fetched
  without credentials — without the third party opting in; `require-corp`
  requires every cross-origin subresource to opt in via CORP or CORS. Lets a
  deployment that needs isolation (e.g. a threaded in-browser compiler) opt
  in per-response from the block that already owns response security
  headers; per the HTML spec, a document sending either value can only embed
  nested documents that also carry a compatible COEP.

- `ConfigSource` trait + `EnvBlockConfig` + `ConfigError` + `StaticConfigSource`
  (`wafer_run::runtime::config_source`).
- `BlockSlot`, `InitializedState`, `InitError` (`wafer_run::runtime::slot`).
- `InitStack` for cycle detection (`wafer_run::runtime::init_stack`).
- `Wafer::init_block` / `init_block_with_stack` for lazy per-block init.
- `Wafer::validate_all_block_configs` returning `ValidationReport`.
- `WaferBuilder::config_source` setter.
- `forward_database_service!` — writes a `DatabaseService` impl from an explicit
  per-operation ledger, each operation stated as `forward`, `custom` or
  `inherit`. An incomplete ledger does not compile. Eight of the trait's
  operations carry defaults that are *not* pass-throughs (`take_where` lists
  then deletes row by row, `delete_where_count` counts then deletes,
  `set_strict_schema` is a silent no-op, …), so a decorator that omits one
  quietly substitutes them for the wrapped backend's atomic statement. Two
  forward targets: `forward_to DbExec;` for a SQL backend, and
  `forward_to <accessor>();` for a decorator exposing
  `fn <accessor>(&self) -> &dyn DatabaseService`. `wafer-block-sqlite` and
  `wafer-block-postgres` now state their surface through it.
- `interfaces::database::codec` — one decode policy for SQL result rows, so a
  value written through `create` reads back the same shape on every backend:
  `decode_text_value` (a serialized JSON object/array in a TEXT column parses
  back to the structured value; everything else, malformed JSON included, stays
  a string), `record_from_json_row`, `record_id`, `first_scalar`, `scalar_i64`,
  `scalar_f64`. Both SQL backends decode through it, and `run_conformance` now
  pins the round trip for every backend that runs the suite.
- `DbExec::ensure_schema_table` (with `run_schema_table_ddl`) — `CREATE TABLE IF
  NOT EXISTS` → add every declared column the table is missing → indexes → FK
  indexes, invalidating the schema cache on both the success and failure paths.
  Fail-loud throughout, including the column adds, which go through
  `add_column_checked` so a lost race stays benign while a genuinely failed
  `ALTER` propagates.
- `DbExec::create_many` — inserts N rows as one `run_batch`, applying `create`'s
  per-row id/timestamp policy. Every row must share one column set (one INSERT
  shape is planned for the batch); a ragged batch is refused.
- `wafer_core::wafer_async_trait` — re-export of the platform-appropriate
  `async_trait` attribute, so `forward_database_service!`'s generated `impl`
  needs no `wafer-block-macro` dependency in the invoking crate.
- `AuthLevel` derives `PartialOrd` and `Ord`. The variants are declared
  weakest-first (`Public < Authenticated < Admin`), so the derive *is* the
  strictness ladder and a visibility ceiling is `required <= caller` rather
  than a rank function each consumer writes for itself. Variant order is now
  load-bearing for access decisions; an exhaustive ladder test in
  `wafer-block` fails to compile when a variant is added and fails at run
  time when one is misplaced. `wafer_core::discovery`'s private `auth_rank`
  is deleted in favour of the comparison.
- `MetadataFilter::matches(Option<&serde_json::Value>) -> bool` — the
  equality-filter predicate on the wire type, so every vector backend answers
  a query the same way instead of each carrying its own copy. Dot-path keys,
  typed JSON equality, conjunction of constraints, and an entry with no
  metadata satisfying only the empty filter. `wafer-block-sqlite` now calls
  it.
- `wafer_run::resolve_declared(block, declared_keys, lookup)` — the shared
  body of a `ConfigSource`: declared value → non-empty `ConfigVar::default`
  → `MissingRequired` if required → omitted if optional. An implementation
  supplies only `lookup`, so the resolution rules cannot drift between
  sources. `lookup` owns the meaning of an empty value: `Some("")` is a
  value, and a source where an empty entry means "unset" filters it itself.
  `StaticConfigSource` is now defined by this function.
- `wafer_core::interfaces::vector::fuse_scored` is re-exported alongside
  `fuse` and `DEFAULT_RRF_K`. `fuse` discards the fused RRF score, which is
  the reason consumers were re-implementing RRF rather than calling it;
  `fuse_scored` was already public but only under `vector::rrf`.
- `PasswordScheme` — `Argon2(Argon2Cost)` (the default) or
  `Pbkdf2Sha256 { iterations }` — plus
  `Argon2JwtCryptoService::with_password_scheme`, and
  `primitives::{pbkdf2_hash, pbkdf2_verify, hash_password_with,
  verify_password_any_scheme}` with
  `PBKDF2_SHA256_RECOMMENDED_ITERATIONS` (600,000, OWASP 2023) and
  `PBKDF2_SHA256_MIN_ITERATIONS` (10,000, NIST SP 800-132 §5.2). PBKDF2
  needs almost no memory; it is not cheaper in CPU (about 180 ms per hash
  at the recommended count in wasm32, against about 3-8 ms for
  `Argon2Cost::Constrained`), so under a tight CPU budget such as a Workers
  Free plan choose `Argon2(Constrained)`.

  **What this changes for a stored credential: nothing.** The scheme selects
  what `CryptoService::hash` *writes*; `compare_hash` dispatches on the PHC
  identifier in the hash it is *handed*, so a credential written under either
  scheme keeps verifying whatever the service is configured to write, and
  selecting a scheme is not a password reset. The default service still
  writes `$argon2id$` at the same cost as before. Old hashes are not
  upgraded in place — a credential keeps its scheme and cost until something
  rewrites it. The one widening: `compare_hash` used to reject a
  `$pbkdf2-sha256$…` string as malformed and now verifies it. New hashes
  below `PBKDF2_SHA256_MIN_ITERATIONS` are refused; *verification* enforces
  no floor, because refusing an existing low-cost credential locks a user
  out rather than protecting them.

  Hashes are `$pbkdf2-sha256$i=N$salt$dk`, standard (not URL-safe) base64
  with padding, 16-byte salt, 32-byte derived key — a persisted format,
  pinned by a known-answer test against an independent implementation. The
  derived-key length is fixed rather than read from the stored string:
  PBKDF2 at a shorter `dkLen` returns a prefix of the longer output, so
  deriving `stored.len()` bytes would let a truncated stored hash verify at
  reduced strength.
- `database.create_many {collection, rows} → {rows_affected}` inserts many
  rows into one collection, and `database.batch {ops} → {results}` applies a
  list of writes — `Create`, `Update`, `Delete`, `UpdateWhere`, `Upsert`,
  across collections — in order. Each runs as ONE transaction on SQLite and
  PostgreSQL: every write lands, or none does when any statement fails. A
  batch op has its single op's semantics, except that an `Update` or
  `Delete` whose id matches no row is reported in its result
  (`Updated(None)`, `Deleted { rows_affected: 0 }`) instead of failing the
  batch; `Created`/`Updated` carry the row as stored (`RETURNING *`). The
  handler authorizes every op's collection for WRITE before anything runs,
  and validates every op (filters, upsert identifiers) before any SQL, so a
  batch naming one collection the caller may not write, or one malformed op,
  touches nothing. Each op or row is one statement, admitted against the
  backend's statement budget before anything runs (see the statement-budget
  entry under Breaking changes). An
  `UpdateWhere` against a missing table matches nothing
  (`UpdatedWhere { rows_affected: 0 }`), as `update_where_count` returns 0.
  Lazily added columns are created before the transaction and are not
  rolled back with it. Guest clients:
  `wafer_core::clients::database::{create_many, batch}` and
  `wafer_sdk::clients::database::{create_many, batch}`; builders
  `wafer_sql_utils::query::{build_insert_returning,
  build_update_by_id_returning}`. An older runtime refuses both ops — the
  dispatcher's action check answers `InvalidArgument` ("does not expose
  action"), a database handler that predates them `Unimplemented` — so
  nothing is written. The shared conformance suite covers both, on SQLite
  and on the live-PostgreSQL CI job.
- `database.insert_guarded {collection, data, guards}` and
  `database.update_guarded {collection, filters, data, guards}` write only
  while every cap guard holds over the collection as it stands before the
  write: `CountBelow { filters, cap }` (fewer than `cap` matching rows) and
  `SumAtMost { field, filters, add, cap }` (`SUM(field)` of the matching rows
  plus `add` is at most `cap`, so landing exactly on the cap is admitted).
  The response says what happened: an insert is `Inserted { record }` or
  `Refused { guard }`; an update is `Updated { rows_affected }`,
  `Refused { guard }` or `NoMatch` (every guard held, no row matched — a
  takeover whose row is gone). `guard` is the index, in the request's
  `guards`, of the first guard that refused, so a caller can say which cap
  was hit. A key that is already taken is an `AlreadyExists` error, not a
  refusal. An update that replaces a row the sum already counts excludes it
  with a filter (`id != …`). The check and the write are one step: one
  transaction holding a probe of every guard's verdict, one
  `INSERT … SELECT … WHERE` / `UPDATE … WHERE` statement and the probe
  again, which SQLite's single writer (and D1's) already serialises. On PostgreSQL the transaction
  first sets `READ COMMITTED` — under a `default_transaction_isolation` of
  REPEATABLE READ or SERIALIZABLE the snapshot would be taken before the
  lock is granted — and then takes a transaction-scoped advisory lock keyed
  by the TABLE, so guarded writes to one table run one at a time there. The
  key is the table rather than the guard's filters because guards with
  different filters over the same rows (a per-bucket file count and a
  per-owner byte sum, or a sum that excludes a replaced row) must still
  exclude each other. A cap, and the refusal it reports, are exact against
  other guarded writes only: `create`, `update` and the other plain writes
  do not take the lock. The transaction probes every guard's verdict before
  and after the write, so a write that an unguarded write refused after the
  first probe is still reported as `Refused { guard }`, not `NoMatch`. The handler authorizes the collection
  for WRITE and validates every guard (filters as `update_where`'s, the
  `SumAtMost` field as a plain identifier, at most
  `wire::database::MAX_WRITE_GUARDS` = 16 guards) before the service runs.
  Guest clients: `wafer_core::clients::database::{insert_guarded,
  update_guarded}` (taking `CapGuard`) and
  `wafer_sdk::clients::database::{insert_guarded, update_guarded}`; builders
  `wafer_sql_utils::guard::{build_insert_guarded, build_update_guarded,
  build_guard_probe, build_guard_preamble}`. An older runtime refuses both
  ops, as for `batch`. The conformance suite covers every outcome on SQLite
  and live PostgreSQL, where a trigger-widened race (eight inserts under a
  cap of three, five updates under a byte cap) lands exactly the cap, with
  the session default at READ COMMITTED and at REPEATABLE READ.

- Append-only WRAP grants: `ResourceGrant::append(grantee, collection)` lets
  the grantee insert rows into a database collection it does not own and
  nothing else — it cannot read, update, delete, upsert or consume a row, or
  reshape the table. `database.create`, `database.create_many` and a
  `Create` inside `database.batch` are the appends; every other write needs a
  read-write grant, and `database.insert_guarded` needs a read grant as well,
  because its guards measure existing rows and its refusal names the guard.
  Read is a separate grant (`ResourceGrant::read`), so a grantee that must
  read too declares both. The database handler authorizes every op from one
  table, `wrap::DATABASE_OP_ACCESS`, and a test fails when an op in
  `ServiceOp::DATABASE_OPS` is missing from it; a batch is authorized per
  write (`BatchWrite::access`), so one `Update` behind a `Create` refuses the
  whole batch before anything runs. An insert admitted only through an
  append grant (no `Write`) must also leave the table and the row identity to
  the server: naming `id`, `created_at` or `updated_at`, or a column the table
  lacks, is `PermissionDenied` and nothing is written. So an append-only
  grantee cannot forge or back-date an entry, and cannot add a column (which
  outside `STRICT_SCHEMA` the insert would otherwise do, typed by its first
  value); and a collection without all three of `id`, `created_at` and
  `updated_at` refuses every append-only insert. An append grant must be typed `Db`; `ResourceGrant::check_shape`
  enforces it at registration and in `Wafer::add_wrap_grants`, and an append
  grant of another type admits nothing. An append grant encodes `write` as
  the string `"append"`, which a runtime that predates append grants fails to
  decode, so it refuses the declaring block rather than reading the grant as
  read-only.

### Fixed

- A `#[wafer_block]` guest handed a `__wafer_handle` or `__wafer_lifecycle`
  frame it cannot decode answers with an `Internal` error naming the frame
  type and the decode cause, which the host surfaces as the call's error.
  It used to return an empty packet, so the host reported only an EOF while
  decoding the guest's result. Guests pick this up when rebuilt.

- `wafer-ffi`: every accepted async call invokes its callback exactly once.
  A call still pending at `wafer_free` gets a `Cancelled` error before
  `wafer_free` returns (dropping the tokio runtime used to discard it, so the
  Go SDK's `Run` blocked forever; the docs claimed the drop waited for it). A
  panic inside spawned work (e.g. a block panicking in `lifecycle(Stop)`)
  gets an `Internal` error naming it; tokio used to swallow it with the
  callback. `wafer_stop` waits for every `wafer_run` accepted before it,
  including one accepted but not yet running, which could otherwise run
  after the blocks' Stop; a second `wafer_stop` no longer runs the blocks'
  Stop again. `wafer_free` called from inside a callback no longer
  panics shutting down the runtime it runs on.
- `wafer-run/network` starts when `WAFER_RUN__NETWORK__STREAM_TIMEOUT_SECS`
  is unset. The key was declared with an empty default and without
  `.optional()`, which the config resolver reads as required, so every
  embedder whose `ConfigSource` did not set it had the network block fail
  Init permanently. Unset means "no total", so the key is now optional. A new
  test boots every in-tree block that declares config over an empty
  `ConfigSource` and allows only a genuinely required key
  (`WAFER_RUN__POSTGRES__DATABASE_URL`) to be missing.
- `wafer-client-js`: the request timeout covers reading the response body;
  it used to stop once headers arrived, so a body that never finished hung
  the call. A timeout is reported as `'timeout'` — fetch rejects with the
  abort reason, so the old `AbortError` check reported it as
  `'network_error'`. The caller's `signal` also aborts the body read, and
  the listener the client adds to that signal is removed after each request
  instead of accumulating on a reused signal. On timeout or abort the body
  stream is cancelled and released, including the body of a response that
  arrives after the client gave up, so a `fetch` that ignores the signal
  leaves no locked body behind.

- `wafer login --registry <url>` stores its token for that registry only.
  It used to replace the token of whichever registry was logged in first,
  so `wafer publish` against that registry failed with "No token" after a
  login elsewhere. `~/.wafer/credentials.toml` now holds one token per
  registry URL under `[tokens]`; a file in the earlier layout (`[default]`
  plus `[registries.<name>]`) keeps working and is rewritten in the new
  layout the next time `wafer login` or `wafer logout` changes it. An
  older `wafer` binary does not read the new layout.
- A registry URL that names its scheme's default port
  (`https://wafer.run:443`, `http://host:80`, or an empty `host:`) is the
  same registry as the URL without it: `Registry::new` drops the default
  port and renders any other port as its number (`:08080` is `:8080`). A
  `wafer login --registry https://wafer.run:443` used to store a second
  token beside the one for `https://wafer.run`, which `wafer publish`
  against the other spelling did not find. Entries already on disk under
  both spellings load as one; the token of the spelling that sorts last
  (`https://wafer.run:443` after `https://wafer.run`) is kept.
- `wafer test` answers a guest's `lookup_attachment` with the runtime's
  `NotFound` sentinel, which the Rust SDK reads as `Ok(None)`. The stub
  returned 0, which the SDK unpacked as a null buffer and passed to
  `Vec::from_raw_parts` (undefined behaviour). The Rust SDK now refuses a
  null buffer from `lookup_attachment` and from a stream's response or
  error read with an `Internal` error instead of reclaiming it.
- The embedder contract (`embed::output_to_json`, `wafer-ffi`'s docs and
  `wafer.h`, `wafer-run-node`'s docs and `index.d.ts`, the Go SDK's
  `Result.Meta` and `wafer.h`) named cookies `resp.cookie.*`; they arrive as
  `resp.set_cookie.{id}`, each value one whole `Set-Cookie` directive. The
  docs now list every `meta` key form, and say `{id}` (the cookie's
  `name[;Domain=..][;Path=..]` when a block uses the cookie helpers) carries
  nothing a host should read. `crates/wafer-ffi/wafer.h` still documented
  `"meta":{"key":"val"}` messages and no `halt` action; the Node usage
  example sent `meta: {}`, which `Message` rejects; `index.d.ts` cited the
  removed `start_without_bind()`. `scripts/check.sh bindings` now fails when
  the Go copy of `wafer.h` differs from the crate's, when the header does not
  declare exactly the `wafer_*` symbols `libwafer_ffi` exports, or when
  `index.d.ts` is not what napi generates, and the Node test runs the usage
  example as written.

- `wafer-run/ip-rate-limit` charges an IPv6 client per /64 (the new
  `ipv6_prefix` flow config, 1 to 128) instead of per address, which a host
  rotating its own interface id used to get a fresh budget per request;
  an IPv4-mapped address (`::ffff:a.b.c.d`) is charged as the IPv4 address
  it carries. A client's count is kept per (budget, window) pair, so a
  request through one flow step no longer resets its window on another
  step with different limits. When a new bucket finds its shard full, the
  block drops expired buckets, then trims to 90% of capacity cheapest first
  (under budget before throttled, lower count, older window). It used to drop the oldest
  windows, which after the expiry sweep are the live clients closest to
  their limit, and judged expiry by the triggering request's window.

- A table another process creates is visible to a database service that
  saw it missing. The schema cache memoized "missing" for its lifetime, so
  on a non-strict backend shared by several processes (replicas, an
  out-of-band migration) `list` stayed empty and `count` zero until the
  process restarted. A missing table now costs one existence probe per
  operation. `sum` and `aggregate` on a missing table answer `0` and no
  groups, as `count` does, instead of failing in the backend.
- Postgres introspection resolves a table name through the session's
  `search_path`, as the (unqualified) statements do, instead of looking
  only in `public`: with another schema first, every existence, column and
  key check missed the table the statements wrote to.
  `introspect::build_list_tables[_like]` list the tables an unqualified
  name reaches,
  and `build_table_info`'s Postgres arm no longer matches the name in
  every schema.
- A table that numbers its own rows gets its id from the database on every
  create path (`create`, `create_many`, `batch`, `insert_guarded`). On
  Postgres a `pk_int` (`SERIAL`) or identity key was never detected, so
  `create` bound a minted UUID string into it and every insert without an
  id failed. On SQLite any `id` key whose type contained `INT` (`INT`,
  `BIGINT`, a composite key) was taken for the rowid alias, so the row was
  stored with a `NULL` id and `create` returned SQLite's rowid as its id.
  The new `introspect::build_id_policy` answers for both dialects
  (SQLite: the rowid alias only; Postgres: identity or `nextval` default),
  cached per table; such a `create` runs `INSERT … RETURNING *`. The
  Postgres binder accepts a string spelling a decimal integer for an
  integer parameter (one past `i64` is "integer out of range"), so
  `get`/`update`/`delete` by such a table's id work.
- A SQLite vector op that meets another connection's lock past the busy
  timeout fails as `Unavailable` (retryable), not `Internal`.

- `LlmError::Network` and `ImageError::Network` map to `Unavailable`, not
  `Internal`, so an unreachable model provider surfaces as a 503 rather
  than a 500, as `network.*` errors already did.
- The S3 storage block's `delete_folder` returns `Err` when S3 did not
  delete every object. S3 answers `DeleteObjects` with `200 OK` and lists
  the keys it did not delete under `Errors`; that list was ignored, so a
  partial delete reported success and a caller removed its records of the
  folder while objects survived. Keys that failed with a transient code
  (`InternalError`, `ServiceUnavailable`, `SlowDown`) are retried, three
  attempts in all; the error names up to ten keys still not deleted, with
  their codes, and counts the rest. The other pages are still deleted
  first, so deleting again finishes the folder.
- The S3 storage block's buffered `get` has a size cap. It collected the
  whole body with no limit; it now refuses an advertised `Content-Length`
  over the cap and bounds the running total, as `get_streaming` did. Both
  S3 reads and local-storage's share
  `wafer_core::interfaces::storage::service::DEFAULT_MAX_OBJECT_BYTES`
  (100 MiB) and fail with `StorageError::TooLarge` (`ResourceExhausted`
  on the wire; local-storage and S3 `get_streaming` used to return
  `Internal`). The S3 cap is set with `WAFER_RUN__S3__MAX_OBJECT_BYTES`
  (a positive integer; anything else fails Init) or
  `S3StorageService::with_max_object_bytes`.

- `wafer-run/http-listener` reads every `X-Forwarded-For` field line when
  it resolves the client IP behind `trusted_proxies`. It read only the
  first line, so behind a trusted proxy that appends its hop as a separate
  line (HAProxy `option forwardfor`) the client-written first line became
  the client IP used for rate limiting and audit. The lines are now one
  list in wire order, walked right to left as before; a line that is not
  text is a malformed hop and stops the walk at the peer address. The peer,
  every entry and every exact `trusted_proxies` entry are compared in
  canonical form, so a listener on `[::]` that sees an IPv4 proxy as
  `::ffff:10.0.0.1` still trusts `10.0.0.1` (every client collapsed into the
  proxy's address before), and the recorded client IP is plain IPv4.

- The SQLite `VectorService` (`wafer-block-sqlite`, `vectors` feature)
  returns the top `top_k` entries that match a metadata filter. The filter
  used to run after the ranking query's `LIMIT` (`top_k` for vector search,
  at least 50 for keyword and hybrid), so matches ranked below that many
  non-matching entries were dropped: a tenant-filtered query on a shared
  index came back short or empty as the index grew. The filter now runs
  inside each ranking query, before its `LIMIT`, through a SQL function
  (`wafer_sql_utils::vector::METADATA_FILTER_FN`, registered by
  `SqliteVecService::new` and used by the new
  `VectorIndexSchema::build_vec_knn_select_filtered` and
  `build_fts_bm25_select_filtered`) that evaluates `MetadataFilter::matches`
  itself, so filtered results keep the filter's typed, dot-path semantics.
  Its vector writes take the write lock up front
  (`BEGIN IMMEDIATE`): `upsert` and `delete` read before writing inside a
  deferred transaction, so while another connection to the same file (the
  database service) held or had just committed a write, they failed at once
  with `SQLITE_BUSY` instead of waiting through the busy handler. `delete`
  no longer skips a rowid it cannot read (which deleted the entry's
  metadata and left its vector orphaned), `upsert` no longer treats a failed
  rowid lookup as "new entry", and a query no longer drops metadata rows it
  cannot read; each is now an `Internal` error. Stored metadata that is not
  JSON now fails the query with `Internal` instead of reading back as `null`.
- PostgreSQL binds every parameter by the type the server infers for it (the
  column an `INSERT`/`UPDATE` writes, the operand a comparison is against),
  not by the JSON value's type. A `null` bound as `text` could not be written
  to an `INTEGER`, `BIGINT`, `BOOLEAN` or `JSONB` column at all (`column "n"
  is of type integer but expression is of type text`); and because sqlx
  caches a prepared statement per connection by its SQL text, the first
  execution's value types fixed the parameter types for every later one —
  once `1.5` had prepared an `INSERT` with a `float8` parameter, a later `2`
  was sent as `int8` bytes and stored as `1e-323`. A value that does not fit
  its parameter's type is `InvalidArgument` (a fractional number for an
  integer column, a string for a boolean). An RFC3339 string now binds into a
  real `TIMESTAMPTZ` column (the stamped `created_at`/`updated_at` included)
  and stays text for a TEXT one. `aggregate::build_sum` casts the sum to
  `DOUBLE PRECISION` in the SQL, and `guard::build_guard_probe` uses inline
  `1`/`0` literals, since a bound fallback no longer widens the result type.
- A database fault that may clear is `Unavailable`, not `Internal`. SQLite
  `SQLITE_BUSY`/`SQLITE_LOCKED`; a Postgres I/O error, pool timeout, or
  SQLSTATE class `08` (not `08P01`), `40001`, `40P01`, `53300`, `55P03`, `57P01`–`57P03` are
  `DatabaseError::Unavailable`, which the database handler answers with
  `ErrorCode::Unavailable` ("database temporarily unavailable"; the driver's
  message is logged). The Init-time schema migration (`handle_lifecycle`),
  the schema steps of `ensure_schema_table`, and the Postgres block's
  connect keep the code, so a block whose Init hit a busy database or a
  server that was still starting is retried instead of failed for good.

- A failed Init is retried when the failure was transient. Every
  `lifecycle(Init)` error used to be cached as permanent for the life of the
  process, so one bad moment at boot (a backend `Unavailable`, a spent
  deadline) disabled a block until restart under a tolerant boot. An Init
  error coded `Unavailable`, `DeadlineExceeded`, `Cancelled` or `Aborted` is
  now `InitError::Transient` (`InitError::from_lifecycle_error`): not cached,
  retried by the first dispatch after a backoff of 100 ms that doubles per
  consecutive failure up to 30 s; every other code stays permanent, including
  `ResourceExhausted`, which the runtime's own deterministic limits raise
  (call depth, wasm host-memory budget, flow `max_steps`). Init also runs on a context of
  its own (`RuntimeContext::for_init`) on every path — eager `init_block`,
  `run_block`, flow steps and `call_block` — with a fresh cancellation flag,
  no deadline, call depth 0, no caller and the block's own `requires`: a
  block first reached at depth 16 or late in a flow's timeout no longer fails
  its Init on the caller's budget, and Init reached through `run_block` or a
  flow step is gated by `requires` as eager init was. A panic in
  `lifecycle(Init)` is caught on native targets and cached as a permanent
  failure instead of unwinding into the request task. Two blocks whose Inits
  call each other, first reached by two concurrent requests, used to
  deadlock both blocks for the life of the process; the wait that would
  close an init cycle — within one dispatch or across concurrent ones — is
  now refused with `InitError::Cycle`. Concurrent calls from one frame into
  one uninitialized block are no longer mistaken for a cycle.

- `__wafer_host_stream_init` charges the target name and the message it
  copies out of guest memory against the per-call host-byte budget
  (`ResourceLimits::max_host_bytes`) before copying. Each open stream holds
  its decoded message until it closes, and only request-body chunks and
  attachments were charged, so a guest could hold up to the live-stream cap
  of full-size messages in host memory.
- CI now builds the feature and target shapes downstream embedders ship,
  which no workspace member enables: `wafer-block --features json-schema`
  and `wafer-block-sqlite --features vectors` are linted with clippy, the
  `vectors` tests run, and `wafer-block-crypto` is built for
  `wasm32-unknown-unknown` through a consumer fixture
  (`crates/wafer-block-crypto/tests/wasm32_consumer`). Every cargo command in
  `scripts/check.sh`, `scripts/build-fixtures.sh` and the coverage job passes
  `--locked`, so a lockfile that no longer matches its manifests fails CI
  instead of being re-resolved. The first `vectors` lint surfaced three
  clippy errors in `wafer-block-sqlite`'s `vector.rs`, fixed here, and
  `wafer-block-crypto` drops its unused `uuid` dependency.
  The fixture crates' own Cargo.locks are seeded from the root one
  (`scripts/fixture-locks.sh sync`), and `check.sh fixtures` fails when a
  crate a fixture shares with the root lock resolves to a version the root
  lock does not pin; run `sync` after changing the root lock.

- `database.aggregate`'s `CaseWhenSum` counts `0`, not `NULL`, in an
  ungrouped query over no rows: it renders
  `COALESCE(SUM(CASE WHEN <when> THEN 1 ELSE 0 END), 0)`
  (`AggregateColumn::case_when_sum`). `SUM` over an empty set is `NULL`, so
  a "how many rows match" read of an empty table or window answered `null`
  instead of a count.

- Discovery documents no longer publish a rest parameter's `...` marker.
  A route whose pattern ends in a trailing-rest placeholder
  (`/b/storage/api/buckets/{name}/objects/{key...}`) had the raw marker
  copied into the OpenAPI `paths` key while `parameters` declared the plain
  name `key`, so the document disagreed with itself in both directions — an
  expression no parameter fills and a parameter no expression consumes,
  which OpenAPI 3.1 does not allow and which makes a generated client build
  the literal three dots into its URL. The same template now renders through
  one function for both projections, so the published path is
  `.../objects/{key}` in the OpenAPI document and in a WebMCP tool's
  `invocation.path`. As a consequence such an endpoint is no longer refused
  from the WebMCP manifest as `PathParamsDisagreeWithTemplate`: the
  placeholder census reads `key` too. Segments the template parser refuses
  (`*`, `**`, malformed braces) still reach the OpenAPI `paths` map
  unchanged — that projection has no refusal channel, and dropping a
  documented route silently would be worse than publishing it as it was.
- HTTP error bodies now carry the application-level detail code. A
  `WaferError` built with `with_detail_code("auth.invalid_email")` was
  rendered by `http_codec::collect_http_response` as `{"error","message"}`
  only, so the code reached a client solely through adapters that added it
  themselves. The codec now emits `{"error": <ErrorCode>, "message": <msg>,
  "code": <detail code>}`, omitting `code` when no detail code is set. The
  Error arm lives in a new public `http_codec::error_to_http_response`, for
  adapters that hold a `WaferError` rather than an `OutputStream`.
  `wafer-client-js` exposes the field as `WaferError.detailCode`.
- The embedder wire format (`embed::output_to_json`, used by `wafer-ffi` and
  `wafer-run-node`) carries the detail code as `detail_code` in an `error`
  result: `{"error": {"code", "message", "detail_code"}}`, omitted when
  unset. The `error` object never carries the error's meta; only its
  response-meta projection crosses, as the result's top-level `meta` (see
  **Added**). The Go SDK's `WaferError` gains `DetailCode` and drops
  `Meta`, a field no producer filled.
- Request meta no longer leaves the runtime through an embedder. PR #338
  closed the `error` arm of `embed::output_to_json`; the `respond`, `halt`
  and `continue` arms still emitted the terminal's whole meta, and a
  terminal legitimately carries the request message's meta — a block builds
  it from the request to keep the CORS and security headers a middleware set
  there. `wafer-run/cors`'s OPTIONS preflight `Halt` did exactly that, so a
  `wafer-ffi` / `wafer-run-node` / Go host received
  `http.header.authorization`, `http.header.cookie`, `auth.user_email`,
  `auth.user_roles`, `req.client.ip` and the decoded query alongside the
  `Access-Control-*` headers it was meant to apply. Every arm now runs its
  meta through the new `wafer_block::http_codec::response_meta_entries` —
  the same projection the native HTTP boundary has always applied — so no
  transport sees a key another would not. `tests/embed_meta_projection.rs`
  pins each arm, twice over: on hand-built terminals and on the real
  `wafer-run/cors` block dispatched through a real `Wafer`.

- `wafer-run/ip-rate-limit` built its 429 error from the whole request
  message, so the error's meta held the request's headers (including
  `Authorization` and `Cookie`), caller identity and client IP next to the
  `Retry-After` / `X-RateLimit-*` headers. The error now carries only those
  three `resp.header.*` entries.

- `jwt_sign` panicked on an expiry that `chrono::Duration::from_std` admits
  but that lands past chrono's last date (year 262143, about 8.2e12 s from
  now); `crypto.sign` takes `expiry_secs` from the wire, so one request
  panicked the handler, which aborts an embedder built with
  `panic = "abort"`. It returns `SignError` now.
- The native crypto handler's Argon2 offload released its semaphore permit
  when the caller's future was dropped (a client disconnect) while the
  blocking job kept running, so the concurrency cap stopped holding. The
  permit now moves into the blocking job and is released when it returns.

### Refactored

- `wafer-block-crypto` is built on argon2 0.6 (was 0.5). argon2 0.5 pulled
  in blake2 0.10 and with it a second copy of `digest` (0.10) beside the
  0.11 line the rest of the crate's crypto uses, in every build that links
  it, wasm32 Worker builds included. The crate now depends on getrandom 0.4
  (argon2's salts and `primitives::random_bytes`) and selects its JS
  backend (`wasm_js`) on wasm32-unknown-unknown itself, as `wafer-block`
  does for getrandom 0.2, so an embedder adds nothing. Hashes are
  byte-identical: the libargon2 known-answer vectors verify under both
  versions, so stored credentials stay valid.

- `wafer-block-sqlite`'s private `apply_filter` and `wafer-core`'s private
  `auth_rank` are deleted; both are now the shared APIs above.

- WRAP grant collection moved from `resolve()`-time to `register_block()`-time
  (per-block validation against the admin block). Typed grants
  (Network/Storage/Crypto) declared by a block that is registered before
  `set_admin_block` is called are deferred (logged + dropped at
  registration), then re-collected when `set_admin_block` runs a rescan
  of every registered block. External grants added via
  `Wafer::add_wrap_grants` are tracked separately and preserved across
  rescans. This accommodates the linkme registration order used by
  `WaferBuilder::build()`, where blocks are auto-registered before the
  embedder gets a chance to call `set_admin_block`.

### Migration

See `docs/migrations/lazy-block-init.md`.
