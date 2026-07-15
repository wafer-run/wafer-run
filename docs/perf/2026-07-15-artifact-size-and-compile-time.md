# PERF-06 — release size and clean compile time per deliverable artifact

**Date:** 2026-07-15 · **Source finding:** deep-dive review 2026-07-14, "PERF-06 —
measure dependency and binary-size cost by feature set" · **Status:** measurement
+ recommendation only; zero dependency changes in this PR.

Reproduce with `./scripts/measure-artifacts.sh` (writes `target-measure/results/`).
Toolchain: rustc 1.97.0 (pinned via `rust-toolchain.toml`).

## Why

`cargo tree -d` on the whole workspace shows ~40 crates with multiple resolved
major versions (AWS Smithy/HTTP, `hyper`, `rustls`, `ureq`, `digest`, `rand`,
`dirs`, `toml`, ...). That aggregate number is misleading: it mixes dev-only,
build-script-only, and feature-gated duplication with duplication that actually
links into shipped artifacts. This doc measures per deliverable so any future
version-alignment work optimizes something real.

## Artifact inventory

The review names five deliverable shapes. Where they live in this repo:

| Deliverable (review) | In this repo | Build |
|---|---|---|
| Minimal runtime | `wafer-run` lib, `--no-default-features` (rlib; embedders set the floor — no standalone binary exists by design) | `cargo build --release -p wafer-run --no-default-features` |
| wasm32 runtime embed | `wafer-run` lib on `wasm32-unknown-unknown`, `--no-default-features --features wasmi` (the gizza/browser combo guarded by `scripts/check.sh wasm`) | see script |
| Native server | `examples/hello-world` (runtime + the 7 battery blocks) and `examples/api-server` (+ sqlite + inspector). **A "full" native server bundling s3 + postgres + fastembed does not exist as a single binary in this repo** — that composition lives downstream (impresspress). The heavy optional blocks are measured as libs instead (below). | `cargo build --release -p hello-world` / `-p api-server` |
| CLI | `wafer-cli` → `wafer` binary | `cargo build --release -p wafer-cli` |
| Node addon | `wafer-run-node` cdylib (`libwafer_run_node.so`; `napi build` ships the same object renamed to `wafer-run.node`) | `cargo build --release -p wafer-run-node` |
| C FFI | `wafer-ffi` cdylib (bonus: also a shipped embedding surface) | `cargo build --release -p wafer-ffi` |
| wasm32 guest block | `examples/wasmi-block` → `wafer_example_wasmi_echo.wasm` on `wasm32-wasip1` (standalone workspace; same pipeline as the test/bench guest fixtures) | see script |
| Heavy optional blocks | `wafer-block-s3`, `wafer-block-fastembed`, `wafer-block-postgres` as libs — the compile-time cost a consumer pays for opting in | see script |

## Measured results

Clean release builds, fresh per-artifact target dir, serialized, deps prefetched.
Sizes are as-shipped: the workspace release profile already sets `strip = true`
(plus `lto = true`, `codegen-units = 1`), so native binaries need no post-strip.
The wasm32-wasip1 guest builds in its own workspace **without** lto/strip and the
repo has no wasm-opt step; the wasm-opt column is what `wasm-opt -Oz
--strip-debug` *would* save (informational, not part of the pipeline).

<!-- RESULTS TABLE — filled from target-measure/results/summary.tsv -->

### Measurement caveats

- This 24-core box hosts other worktrees with concurrent cargo builds; the
  script records `uptime` before each build (see per-artifact `.log` files).
  Wall-clock is therefore indicative; **user+sys CPU seconds are the more
  comparable numbers**. Re-run the script on a quiet machine for publishable
  wall times.
- `~/.cargo` registry cache was warm and deps were prefetched, so timings
  exclude network — except any build-script downloads (`ort-sys` for
  fastembed), which are part of that artifact's real cost and noted below.
- lib-only rows (`runtime-*`, `lib-*`) have no shipped file: the compile time
  is the cost an embedder pays; "size" is not applicable to an rlib.

## Duplicate-dependency attribution per artifact

`cargo tree -d -e normal` scoped to each artifact (normal edges only = what
actually links), then filtered to crates with **more than one resolved
version** — `cargo tree -d` also lists same-version host/proc-macro rebuilds
(e.g. `memchr`, `serde_json` built once for target and once for the
`wafer-block-macro` host build), which cost some compile time but zero binary
size and are not version skew.

| Artifact | Real multi-version duplicates (normal edges) |
|---|---|
| runtime-min-lib | **none** |
| runtime-wasm32-wasmi-lib | **none** |
| server-hello-world | **none** |
| server-api-sqlite | `getrandom` 0.2/0.4, `hashbrown` 0.14/0.17 |
| cli-wafer | `bitflags` 1/2, `mio` 0.8/1, `rustix` 0.38/1, `linux-raw-sys` 0.4/0.12, `getrandom` 0.2/0.4 |
| node-addon | **none** |
| c-ffi | **none** |
| wasm-guest-echo | **none** |
| lib-s3 | 22 crates — two full HTTP+TLS stacks: `hyper` 0.14+1.9, `rustls` 0.21+0.23, `h2` 0.3+0.4, `http` 0.2+1, `http-body`, `hyper-rustls`, `tokio-rustls`, `rustls-webpki`, plus RustCrypto 0.10/0.11 pairs (`digest`, `sha2`, `hmac`, `block-buffer`, `crypto-common`, ...), `aws-smithy-http` ×2, `aws-smithy-json` ×2, `rand_core` 0.6/0.9 |
| lib-fastembed | `base64` 0.13/0.22, `nom` 7/8, `getrandom` ×3, `hashbrown` 0.16/0.17, `webpki-roots` 0.26/1 |
| lib-postgres | `getrandom` 0.2/0.4, `hashbrown` 0.15/0.17, `heck` 0.4/0.5 |
| workspace aggregate (`-e normal`) | 36 crates |
| workspace aggregate (all edges) | 44 crates |

### Where the review's headline duplicates actually come from

- **hyper / rustls / http / h2 / smithy (the big one).** Entirely internal to
  the AWS SDK graph, and only in artifacts that link `wafer-block-s3`.
  Root cause: `aws-sdk-s3`'s **default features enable both HTTP stacks** —
  `rustls` (= `aws-smithy-runtime/tls-rustls`, the legacy hyper 0.14 +
  rustls 0.21 + ring connector) *and* `default-https-client` (the modern
  hyper 1 + rustls 0.23 + aws-lc connector). `aws-config` itself only asks
  for the modern client; the legacy stack rides in solely via `aws-sdk-s3`'s
  `default`. No workspace version bump can fix this; a feature selection can
  (see recommendation 1).
- **`ureq` 2 vs 3.** Never links into anything. `ureq` 2 is `hf-hub`'s
  (fastembed) download client; `ureq` 3 is a **build-dependency of `ort-sys`**
  (fetches the ONNX Runtime binary at compile time). Build-edge only.
- **`toml` 0.8 vs 1.** Dev-only: `toml` 1 comes from `trybuild`
  (wafer-block-macro's compile-fail tests). Shipped artifacts contain only
  `toml` 0.8 (from `wafer-run`).
- **`rand` 0.8 vs 0.9.** `rand` 0.8 ← `sqlx-postgres`; `rand` 0.9 ←
  `aws-smithy-checksums`/`crc-fast`, `hf-hub`, `tokenizers`. The two versions
  co-link only in a binary that combines postgres with s3/fastembed — i.e.
  nothing in this repo, but the downstream impresspress full server does.
  Not alignable from this workspace (all transitive).
- **`dirs` 5 vs 6.** `dirs` 5 is a *direct* dependency of `wafer-run` and
  `wafer-cli`; `dirs` 6 comes via `hf-hub` (fastembed). Co-links downstream
  wherever the runtime meets fastembed. This one **is** alignable here — a
  two-line bump (see recommendation 2).
- **`digest`/`sha2`/`hmac` 0.10 vs 0.11-pre.** The 0.11 line rides the AWS
  checksum/signing graph; the workspace's own crypto is uniformly 0.10. Not
  alignable until RustCrypto 0.11 is stable and AWS + the workspace move
  together. s3-only artifacts pay it; falls out anyway if recommendation 1
  drops ring-based legacy TLS (partially) — otherwise cost is a few small
  crates compiled twice.
- **cli-wafer's `mio`/`rustix`/`bitflags` skew.** Comes from `notify` 6
  (file watcher, old mio 0.8) vs tokio (mio 1); `notify` 8 would align mio.
  These are small crates; measurable but minor (see recommendation 4).
- **`hashbrown`/`getrandom` multi-version.** Ubiquitous tiny crates dragged
  by unrelated majors (`sqlx`, `rusqlite`/`hashlink`, `lru`, `safetensors`,
  wasmi). Compile-time noise, negligible size; not worth chasing directly —
  they collapse on their own as the big libraries converge.

## Analysis

1. **The runtime core is clean.** The minimal runtime, the wasm32 runtime
   embed, the node addon, the C FFI cdylib, the batteries server
   (hello-world), and the wasm guest ship with **zero** multi-version
   duplicates. The workspace-aggregate "~40 duplicated crates" picture is
   driven almost entirely by three opt-in leaf blocks (s3, fastembed,
   postgres) plus dev/build edges. Blanket version-alignment work targeting
   the aggregate tree would be effort spent on artifacts that are already
   clean.
2. **S3 is the only artifact with a structurally heavy duplication problem**,
   and it is a *feature-selection* problem (double HTTP+TLS stack by
   default), not a version-alignment problem.
3. **Heavy optional blocks are already isolated.** s3/fastembed/postgres are
   separate crates that consumers opt into; the review's suggested "isolate
   heavy optional features" is already the architecture. The remaining cost
   is paid only by binaries that link them (downstream impresspress), which
   is where the compile-time and size numbers below matter.
4. **fastembed's shipped-binary weight is dominated by ONNX Runtime**, which
   `ort-sys` downloads/links at build time — that cost shows up in its build
   time and in any consumer binary, and no cargo version alignment touches it.

## Recommendation

**Worth doing** (measured impact, small diffs — each is a follow-up PR, not
this one):

1. **Drop the legacy AWS HTTP stack from `wafer-block-s3`** — in workspace
   `Cargo.toml`:
   `aws-sdk-s3 = { version = "1", default-features = false, features = ["sigv4a", "default-https-client", "rt-tokio"] }`
   (aws-config already defaults to the modern client; verify `sigv4a` is
   still required by the deployment targets). Eliminates hyper 0.14,
   rustls 0.21 + ring, h2 0.3, http 0.2, hyper-rustls 0.24, tokio-rustls
   0.24, rustls-webpki 0.101 — an entire second HTTP+TLS stack — from every
   s3-linking binary. This is the single highest-leverage change the tree
   analysis found.
2. **Bump `dirs` 5 → 6 in `wafer-run` and `wafer-cli`.** Two lines; removes
   the only runtime-side duplicate that co-links downstream (against
   `hf-hub`'s dirs 6).
3. **Keep s3/fastembed/postgres as opt-in leaf crates** (status quo) and do
   not fold them behind features of a fatter crate; the per-artifact trees
   confirm isolation works.

**Explicitly skip** (measured cost does not justify the churn):

4. `notify` 6 → 8 in wafer-cli purely for mio/rustix alignment — small crates,
   CLI compile time is not a bottleneck; take it opportunistically when
   `notify` is bumped for features.
5. `ureq`, `toml`, `heck`, `itertools`, `der`, `winnow`, `serde_spanned`
   alignment — dev/build-edge only; never shipped.
6. `rand`, `getrandom`, `hashbrown`, `base64`, `nom` unification — all
   transitive under sqlx/aws/fastembed/rusqlite; not controllable from this
   workspace and individually tiny.
7. RustCrypto 0.10 → 0.11 alignment — blocked upstream (AWS uses 0.11
   pre-releases); revisit when 0.11 is stable across the ecosystem.

Decision input for any future dep-alignment work: re-run
`./scripts/measure-artifacts.sh` and diff `summary.tsv` + the per-artifact
`tree-*.txt` files before/after.
