#!/usr/bin/env bash
# Build wasm test fixtures consumed by wafer-run integration tests.
# Idempotent — cargo's own fingerprinting decides whether a fixture is
# actually recompiled, so repeat runs are fast no-ops when nothing changed.
#
# Run after a fresh `git clone` to seed the testdata directory and the
# per-fixture target/ output paths. The pre-commit hook calls this
# script before clippy so subagent-dispatched workflows succeed without
# a manual setup step.
#
# Failure modes:
# - wasm32-wasip1 rustup target missing → script exits with a helpful
#   message pointing at `rustup target add wasm32-wasip1`.
# - cargo build failure → propagates non-zero exit, caller decides.

set -e

cd "$(git rev-parse --show-toplevel)"

if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-wasip1$'; then
    echo "error: wasm32-wasip1 target not installed." >&2
    echo "       run: rustup target add wasm32-wasip1" >&2
    exit 1
fi

# Build (cargo) and optionally copy (mv-shaped) a wasm fixture into place.
#
# `cargo build` runs unconditionally — cargo's own fingerprint-based
# staleness detection (which covers the fixture's own sources *and* its
# path-dependencies, e.g. wafer-sdk/wafer-block) decides whether a
# recompile is actually needed, so this is fast when nothing changed. Do
# NOT reintroduce a "skip if $dest already exists" shortcut here: that
# check only proves the file is present, not that it reflects the current
# source — it previously let a wasm built before a guest-source change
# (e.g. new dispatch arms) sit stale indefinitely, both locally and via any
# CI cache that restores target/ keyed on Cargo.lock (which a source-only
# change doesn't invalidate), silently short-circuiting the tests that
# exercise the new code path.
#
# If the build artifact and the destination are the same path (fixtures 2-4
# — built in-place), skip the copy step. `cp` would error with "same file"
# otherwise.
build_fixture() {
    local dest="$1"
    local manifest="$2"
    local artifact="$3"
    shift 3
    # Any remaining args are extra cargo flags (e.g. `--features percall`
    # for a variant build of the same fixture crate). Variant builds share
    # the crate's artifact path, so each variant must be copied to its
    # variant-named destination before the next build overwrites the
    # artifact — order the calls accordingly, and keep any in-place
    # (artifact == dest) build LAST so the artifact ends up being it.

    echo "building $dest"
    cargo build --release --target wasm32-wasip1 --manifest-path "$manifest" "$@" >&2

    if [ "$artifact" != "$dest" ]; then
        mkdir -p "$(dirname "$dest")"
        cp "$artifact" "$dest"
    fi
}

# echo_block.wasm — consumed by wasmi_block_test.rs via include_bytes!
# Built in examples/wasmi-block, copied to testdata.
build_fixture \
    crates/wafer-run/testdata/echo_block.wasm \
    examples/wasmi-block/Cargo.toml \
    examples/wasmi-block/target/wasm32-wasip1/release/wafer_example_wasmi_echo.wasm

# attachment_dispatch_guest.wasm — consumed by attachment_e2e_wasmi.rs
# at runtime via Path. Built in place; no copy needed.
build_fixture \
    crates/wafer-run/tests/attachment_dispatch/target/wasm32-wasip1/release/attachment_dispatch_guest.wasm \
    crates/wafer-run/tests/attachment_dispatch/Cargo.toml \
    crates/wafer-run/tests/attachment_dispatch/target/wasm32-wasip1/release/attachment_dispatch_guest.wasm

# dispatch_guest.wasm — consumed by dispatch_streaming.rs at runtime
# via Path. Built in place; no copy needed.
build_fixture \
    crates/wafer-run/tests/dispatch_guest/target/wasm32-wasip1/release/dispatch_guest.wasm \
    crates/wafer-run/tests/dispatch_guest/Cargo.toml \
    crates/wafer-run/tests/dispatch_guest/target/wasm32-wasip1/release/dispatch_guest.wasm

# service_client_guest.wasm — consumed by service_client_e2e.rs at runtime
# via Path. The first fixture built against `wafer-core --features
# wasm-component`, exercising TODO #103's call_service. Built in place.
build_fixture \
    crates/wafer-run/tests/service_client_guest/target/wasm32-wasip1/release/service_client_guest.wasm \
    crates/wafer-run/tests/service_client_guest/Cargo.toml \
    crates/wafer-run/tests/service_client_guest/target/wasm32-wasip1/release/service_client_guest.wasm

# hostile_db_guest.wasm — consumed by wrap_hostile_guest_e2e.rs at runtime
# via Path. An ordinary, unprivileged public-SDK guest (SP-A Stage 1 task 8
# hostile-guest end-to-end regression test — no WRAP meta at all). Built in
# place.
build_fixture \
    crates/wafer-run/tests/hostile_db_guest/target/wasm32-wasip1/release/hostile_db_guest.wasm \
    crates/wafer-run/tests/hostile_db_guest/Cargo.toml \
    crates/wafer-run/tests/hostile_db_guest/target/wasm32-wasip1/release/hostile_db_guest.wasm

# json_host_guest.wasm — consumed by json_host_codec_e2e.rs at runtime via
# Path. The std-only, ZERO-dependency guest that negotiates the JSON host-call
# codec (`__wafer_host_codec() -> 1`) and drives database/storage/config over
# it. Deliberately has no `[dependencies]`: it is the compatibility fixture a
# dependency-free toolchain must be able to build. Built in place.
build_fixture \
    crates/wafer-run/tests/json_host_guest/target/wasm32-wasip1/release/json_host_guest.wasm \
    crates/wafer-run/tests/json_host_guest/Cargo.toml \
    crates/wafer-run/tests/json_host_guest/target/wasm32-wasip1/release/json_host_guest.wasm

# pool_guest_{singleton,percall}.wasm — consumed by wasm_instance_pooling.rs
# at runtime via Path. Two variant builds of one crate (PERF-01 Part B): the
# default build declares InstanceMode::Singleton (pool-eligible); the
# `percall` feature build declares PerExecution (the cold control). Both
# share the crate's artifact path, so each build is copied to its
# variant-named destination before the next build overwrites the artifact.
build_fixture \
    crates/wafer-run/tests/pool_guest/target/wasm32-wasip1/release/pool_guest_singleton.wasm \
    crates/wafer-run/tests/pool_guest/Cargo.toml \
    crates/wafer-run/tests/pool_guest/target/wasm32-wasip1/release/pool_guest.wasm

build_fixture \
    crates/wafer-run/tests/pool_guest/target/wasm32-wasip1/release/pool_guest_percall.wasm \
    crates/wafer-run/tests/pool_guest/Cargo.toml \
    crates/wafer-run/tests/pool_guest/target/wasm32-wasip1/release/pool_guest.wasm \
    --features percall

# bench_guest_singleton.wasm — the pooled-dispatch bench arm variant
# (PERF-01 Part B): identical guest code, but declares
# InstanceMode::Singleton so WasmiBlock's warm pool engages. Built BEFORE
# the default bench_guest.wasm below, which is consumed in place — the
# in-place build must run last so the artifact path holds the default
# (cold) build, not this variant.
build_fixture \
    crates/wafer-run/benches/fixtures/bench_guest/target/wasm32-wasip1/release/bench_guest_singleton.wasm \
    crates/wafer-run/benches/fixtures/bench_guest/Cargo.toml \
    crates/wafer-run/benches/fixtures/bench_guest/target/wasm32-wasip1/release/bench_guest.wasm \
    --features singleton

# bench_guest.wasm — consumed by the criterion benches (benches/wasm_guest.rs)
# at runtime via Path. Echo + nested call_block arms for the PERF-01
# measurement suite. Built in place; no copy needed. Keep this AFTER the
# singleton variant build above (see its comment).
build_fixture \
    crates/wafer-run/benches/fixtures/bench_guest/target/wasm32-wasip1/release/bench_guest.wasm \
    crates/wafer-run/benches/fixtures/bench_guest/Cargo.toml \
    crates/wafer-run/benches/fixtures/bench_guest/target/wasm32-wasip1/release/bench_guest.wasm

# tinygo_guest.wasm — consumed by the criterion benches (benches/wasm_guest.rs)
# for the cold/warm TinyGo comparison: a TinyGo wasi module exports `_start`,
# which the loader re-runs on every per-call instantiation. TinyGo is a local
# toolchain, not a CI dependency, so this fixture is best-effort: skipped with
# a note when `tinygo` is not on PATH (the benches skip the TinyGo group when
# the .wasm is absent).
if command -v tinygo >/dev/null 2>&1; then
    echo "building crates/wafer-run/benches/fixtures/tinygo_guest/target/tinygo_guest.wasm"
    mkdir -p crates/wafer-run/benches/fixtures/tinygo_guest/target
    (cd crates/wafer-run/benches/fixtures/tinygo_guest && \
        tinygo build -target wasip1 -o target/tinygo_guest.wasm .) >&2
    # Pooled-arm variant (PERF-01 Part B): same guest, but built with
    # `-tags singleton` so the declared instance_mode is "Singleton" and
    # WasmiBlock's warm pool engages — the arm that amortizes TinyGo's
    # per-call `_start`. (Build tags, not -ldflags -X: TinyGo's compile-time
    # interp folds the infoJSON initializer before link-time overrides.)
    echo "building crates/wafer-run/benches/fixtures/tinygo_guest/target/tinygo_guest_singleton.wasm"
    (cd crates/wafer-run/benches/fixtures/tinygo_guest && \
        tinygo build -target wasip1 -tags singleton \
            -o target/tinygo_guest_singleton.wasm .) >&2
else
    echo "note: tinygo not on PATH — skipping tinygo_guest.wasm (TinyGo benches will be skipped)" >&2
fi
