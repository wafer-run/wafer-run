#!/usr/bin/env bash
# Single definition of "green" for wafer-run — run this before opening a PR.
#
# Mirrors .github/workflows/ci.yml (the `check`, `test`, and `wasm` jobs).
# If CI changes, this script must change too, and vice versa.
#
# The `audit` job is advisory: CI runs `cargo audit` with
# continue-on-error, so a failure here warns but does not fail the script.
#
# Usage: ./scripts/check.sh
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

echo "==> Build wasm test fixtures"
./scripts/build-fixtures.sh

echo "==> Format (nightly rustfmt)"
cargo +nightly fmt --all -- --check

echo "==> Clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> Tests"
cargo test --workspace

echo "==> Guest SDK builds to wasm32-wasip1"
cargo build -p wafer-block -p wafer-sdk --target wasm32-wasip1

echo "==> Runtime builds with --no-default-features (wasmi off)"
cargo check -p wafer-run --no-default-features

echo "==> Guest service-client path (wafer-core wasm-component → wasm32)"
cargo clippy -p wafer-core --features wasm-component --target wasm32-wasip1 -- -D warnings

echo "==> Security audit (advisory — CI runs this continue-on-error)"
if ! cargo audit; then
    echo "warning: cargo audit reported issues (non-blocking, matches CI)" >&2
fi

echo "==> All checks passed."
