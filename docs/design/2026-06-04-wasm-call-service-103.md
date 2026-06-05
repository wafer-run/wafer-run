# Design: WASM typed service clients (`call_service`) — TODO #103

**Status:** **PR 1 implemented (2026-06-05).** Phased follow-up to the 2026-06-04 code review
(see `docs/reviews/2026-06-04-code-review.md`, item **R1**). PR 2 (integration fixture +
test) and PR 3 (Go SDK) remain.

## PR 1 — implementation notes (2026-06-05)

Landed the two `#[cfg(feature = "wasm-component")]` stubs as a synchronous driver over the
existing streaming ABI, exactly as designed — **the "no host change required" assumption was
validated**: `wafer-core --features wasm-component --target wasm32-wasip1` compiles and the
guest-initiated `init → write_chunk → finish → read* → close` cycle reuses the host's existing
resume loop (`call_guest_resumable_with_attachments`) unchanged. Key decisions:

- **Self-contained extern decls** in a `clients::wasm_streaming` module (no `wafer-core →
  wafer-sdk` dependency — that edge would be backwards). The driver mirrors `wafer-sdk`'s
  `stream.rs`. Block-level errors surface via the read path's `take_error` (full `WaferError`);
  dispatch-level failures use the negative-ordinal sentinel.
- **`ErrorCode` ordinal mapping centralised** in `wafer-block` (`ErrorCode::to_ordinal` /
  `from_ordinal`) as the single source of truth. The host encoder (`error_code_to_neg_i32/i64`)
  and both guest decoders (`wafer-sdk`, `wafer-core`) now route through it, replacing two
  hand-rolled "keep in sync" copies.
- **Shared `build_service_message` / `apply_wrap_meta`** helpers dedupe the request-message
  construction across the native and wasm paths (unit-tested on the host).
- **CI gate added** so this previously-uncompiled path stops rotting: `cargo check -p
  wafer-core --features wasm-component --target wasm32-wasip1` in the `wasm` job. Fixing the 8
  latent `missing_docs` it surfaced (wasm twins in image/llm/network) was part of the work.
  (Started as a `clippy -D warnings` gate, but that surfaced ~11 pre-existing
  `arc_with_non_send_sync` lints in `service_blocks/{vector,storage}` on the single-threaded
  wasm32 target — unrelated to `call_service`; downgraded to `cargo check` rather than expand
  PR1 scope. **Follow-up:** clean those up + decide whether the native `service_blocks` should
  even be part of the wasm32 `wasm-component` lib, then promote the gate back to clippy.)

## Problem

When `wafer-core` is compiled with `feature = "wasm-component"`, the typed service-client
entry points are stubbed:

- `crates/wafer-core/src/clients/mod.rs` — `call_service` (≈L154-170) and
  `call_service_with_msg` (≈L254-269) return
  `ErrorCode::UNIMPLEMENTED` ("wasm-component call_service not yet implemented for
  streaming protocol"), behind a `// TODO(#103)`.

The `dual_api!` macro generates **both** a native-async path and a wasm-sync path for every
client (`database`, `auth`, `crypto`, `network`, `storage`, `vector`, `image`, `llm`, …),
but the wasm path always errors. **Net effect: a WASM guest block cannot call any typed
service client** — only host-native blocks can. This gates the entire "write feature blocks
in WASM" story.

## Key insight: the hard part already exists

The streaming ABI + sync-guest/async-host bridge that a `call_block` from inside WASM needs
is **already built and battle-tested** by the guest-facing streaming API:

- Host imports in `crates/wafer-run/src/wasm/wasmi_loader/imports.rs`:
  `__wafer_host_stream_init`, `_write_chunk`, `_attach`, `_finish` (traps),
  `_read_chunk` (traps), `_take_error`, `_close`.
- The resume loop in `crates/wafer-run/src/wasm/wasmi_loader/mod.rs`
  (`call_guest_resumable_with_attachments`) already handles the traps: on
  `pending_stream_finish` it `await`s `ctx.call_block(target, msg, input)` and installs the
  resulting `OutputStream`; on `pending_stream_read` it drives `OutputStream::next()`,
  allocates guest memory, and resumes with a packed `(ptr,len)`.

So the missing piece is **not** new host machinery — it is the **wasm-side client wrapper**
in `wafer-core` that drives this existing ABI from `call_service`, mirroring what the native
path does via `ctx.call_block(...).await` + `collect_buffered()`.

## Native path (the behaviour to mirror)

`call_service` (native) → `call_service_streaming` → builds a `Message` (sets
`META_REQ_ACTION`, and when a WRAP resource is supplied, `META_WRAP_RESOURCE`/`_ACCESS`/
`_RESOURCE_TYPE`) → `ctx.call_block(block, msg, InputStream::from_bytes(payload)).await` →
`out.collect_buffered().await` → returns the concatenated body bytes. `collect_buffered`
also validates the stream terminal.

## Proposed design

Implement the two wasm-component stubs as a **synchronous** wrapper over the existing
streaming ABI, self-contained in `wafer-core` (raw `extern "C"` host-import declarations) so
`wafer-core` stays runtime-portable and no SDK dependency is pulled into it:

```
call_service(block, kind, data, resource, is_write, resource_type):
    payload = codec::encode(data)
    msg     = Message{kind}; set META_REQ_ACTION + WRAP meta (as native does)
    h = __wafer_host_stream_init(block, encode(msg))      // <0 ⇒ ErrorCode sentinel, return
    __wafer_host_stream_write_chunk(h, payload)           // <0 ⇒ error
    rc = __wafer_host_stream_finish(h)                    // traps → host awaits ctx.call_block
    if rc < 0: err = __wafer_host_stream_take_error(h); return err
    loop:                                                 // drain response
        packed = __wafer_host_stream_read_chunk(h)        // traps → host drives OutputStream
        if packed == 0: break                             // end of stream
        if packed < 0: return take_error(h)
        buf.extend(read(ptr,len))
    __wafer_host_stream_close(h)
    return buf
```

`call_service_with_msg` is the same minus the `Message` construction.

### Why this is correct
- The sync-guest → async-host boundary is handled by wasmi's `TypedResumableCall`: the guest
  call to `_finish`/`_read_chunk` traps, the host resume loop runs the `async` dispatch /
  stream drive, then resumes the guest with the result. This is exactly the mechanism the
  guest-facing streaming API already uses — no new bridging, no `block_on`.
- Error semantics use the existing negative-`i64`/`i32` ErrorCode sentinels + `_take_error`
  for the full `WaferError`.

## Scope of changes

- **wafer-core** (`clients/mod.rs`): implement the two `#[cfg(feature = "wasm-component")]`
  variants + a small `wasm_streaming` module with the `extern "C"` host-import decls and the
  drive loop. (Primary deliverable.)
- **wafer-run** (wasm host): expected **no change** — the host imports + resume loop already
  exist. Confirm during implementation that a guest-initiated `stream_init/finish/read`
  cycle from `call_service` exercises the same path as the guest-facing API.
- **Guest SDK glue**: the Rust-guest path uses `wafer-core`'s `extern` decls directly. The Go
  SDK (`sdks/go/`) would need matching `//go:wasmimport` declarations if Go guests are to use
  typed clients (separate, SDK-side follow-up).
- **Tests**: a compiled guest fixture that calls a service client (e.g. `crypto`/`auth`) plus
  an integration test in `crates/wafer-run/tests/wasmi_block_test.rs` with a mock context
  providing the target service. **This fixture is new work** (there is no existing guest that
  calls a typed client).

## Phasing

1. **PR 1 — wafer-core wasm client + extern decls.** Implement the drive loop; unit-test the
   message/meta construction in isolation.
2. **PR 2 — integration fixture + test.** New compiled-guest fixture that calls a service
   client; `wasmi_block_test.rs` integration test through a mock service context.
3. **PR 3 (optional, SDK) — Go `//go:wasmimport` wrappers** if Go guests need typed clients.

## Risks / open questions

- **Multi-frame services.** `network`/`storage GET` responses use a header frame + body
  chunks; `call_service` must reproduce the native `collect_buffered`/header handling.
  Recommendation: land single-frame services (auth/crypto/database/config) first; treat
  header-framed services as a follow-up, or expose the raw streaming ABI for those.
- **Error encoding on `stream_init` denial.** A capability/WRAP denial returns a negative
  sentinel from `_init` (no handle allocated) → return immediately; confirm the guest decodes
  it to the right `ErrorCode`.
- **FFI memory marshalling.** Reuse the existing `__wafer_alloc` + `codec::encode` patterns;
  validate pointer/len handling (the review's `unpack_ptr_len` sign-check hardening applies).
- **Feature gating.** Keep behind `#[cfg(feature = "wasm-component")]` — Cloudflare Workers
  use wasm-bindgen, not this WIT-component path, so they must not activate it.

## Recommendation

Tractable but **medium-large** (multi-PR, needs a new compiled-guest fixture); the
sync/async bridge is the main risk and is already proven by the guest-facing streaming API.
**Land design-first (this doc), then the phased PRs above.** Validate the "no host change
required" assumption in PR 1 before writing the fixture.
