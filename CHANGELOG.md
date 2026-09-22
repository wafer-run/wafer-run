# Changelog

## Unreleased

### Breaking changes

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
- `add_block_config()` JSON no longer flows into a block's `lifecycle(Init)`
  payload. It still participates in composite/uses expansion and is
  surfaced via `RuntimeContext::block_configs()`. Block init config comes
  from the registered `ConfigSource`.
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

### Added

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

- `InputStream::from_stream` and `from_stream_with_cancel` take
  `S: Stream<Item = Vec<u8>> + MaybeSend + 'static` instead of requiring
  `Send` outright, and `InputStream` boxes its inner stream as a
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
  let input = InputStream::from_stream(body.map(|chunk| chunk.unwrap_or_default()));
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
  exists because argon2id's default memory cost is unaffordable in
  single-threaded wasm, where it takes minutes per hash.

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

### Fixed

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

### Refactored

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
