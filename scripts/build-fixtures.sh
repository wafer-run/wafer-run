#!/usr/bin/env bash
# Build wasm test fixtures consumed by wafer-run integration tests.
# Idempotent — each fixture is rebuilt only if missing.
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
# - If the destination already exists, skip entirely.
# - If the build artifact and the destination are the same path
#   (fixtures 2 and 3 — built in-place), skip the copy step. `cp` would
#   error with "same file" otherwise.
build_if_missing() {
    local dest="$1"
    local manifest="$2"
    local artifact="$3"

    if [ -f "$dest" ]; then
        return 0
    fi

    echo "building $dest"
    cargo build --release --target wasm32-wasip1 --manifest-path "$manifest" >&2

    if [ "$artifact" != "$dest" ]; then
        mkdir -p "$(dirname "$dest")"
        cp "$artifact" "$dest"
    fi
}

# echo_block.wasm — consumed by wasmi_block_test.rs via include_bytes!
# Built in examples/wasmi-block, copied to testdata.
build_if_missing \
    crates/wafer-run/testdata/echo_block.wasm \
    examples/wasmi-block/Cargo.toml \
    examples/wasmi-block/target/wasm32-wasip1/release/wafer_example_wasmi_echo.wasm

# attachment_dispatch_guest.wasm — consumed by attachment_e2e_wasmi.rs
# at runtime via Path. Built in place; no copy needed.
build_if_missing \
    crates/wafer-run/tests/attachment_dispatch/target/wasm32-wasip1/release/attachment_dispatch_guest.wasm \
    crates/wafer-run/tests/attachment_dispatch/Cargo.toml \
    crates/wafer-run/tests/attachment_dispatch/target/wasm32-wasip1/release/attachment_dispatch_guest.wasm

# dispatch_guest.wasm — consumed by dispatch_streaming.rs at runtime
# via Path. Built in place; no copy needed.
build_if_missing \
    crates/wafer-run/tests/dispatch_guest/target/wasm32-wasip1/release/dispatch_guest.wasm \
    crates/wafer-run/tests/dispatch_guest/Cargo.toml \
    crates/wafer-run/tests/dispatch_guest/target/wasm32-wasip1/release/dispatch_guest.wasm

# service_client_guest.wasm — consumed by service_client_e2e.rs at runtime
# via Path. The first fixture built against `wafer-core --features
# wasm-component`, exercising TODO #103's call_service. Built in place.
build_if_missing \
    crates/wafer-run/tests/service_client_guest/target/wasm32-wasip1/release/service_client_guest.wasm \
    crates/wafer-run/tests/service_client_guest/Cargo.toml \
    crates/wafer-run/tests/service_client_guest/target/wasm32-wasip1/release/service_client_guest.wasm

# hostile_db_guest.wasm — consumed by wrap_hostile_guest_e2e.rs at runtime
# via Path. An ordinary, unprivileged public-SDK guest (SP-A Stage 1 task 8
# hostile-guest end-to-end regression test — no WRAP meta at all). Built in
# place.
build_if_missing \
    crates/wafer-run/tests/hostile_db_guest/target/wasm32-wasip1/release/hostile_db_guest.wasm \
    crates/wafer-run/tests/hostile_db_guest/Cargo.toml \
    crates/wafer-run/tests/hostile_db_guest/target/wasm32-wasip1/release/hostile_db_guest.wasm
