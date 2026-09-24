# Changelog

## Unreleased

### Breaking changes

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
  refuses. `ddl::build_drop_table`, `build_add_column`,
  `build_add_column_with_type` and `build_add_column_for_value` return
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
  path) is removed — `build_add_column_with_type` covers it.
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
  `CAP_HEADERS`). Note that `Wafer::seal` currently replaces the
  capabilities passed to `WasmiBlock::load_with_capabilities*` with the
  guest's declared ones (∩ config), so a loader-supplied cap does not bound
  the declaration yet; WR-09 fixes that.
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
  `resp.header.*`, `resp.cookie.*`, `resp.content_type` — under their own
  names. A host that read any other key (`http.header.*`, `http.method`,
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

### Added

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
  touches nothing. One call carries at most `wire::database::MAX_BATCH_WRITES`
  (1000) ops or rows — Cloudflare's per-invocation D1 query limit on Workers
  Paid (the Free plan allows 50), which also bounds how long one call holds
  SQLite's single write connection; a larger call is `InvalidArgument`. An
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
