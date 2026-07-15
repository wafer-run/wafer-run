#!/usr/bin/env bash
# Single definition of "green" for wafer-run.
#
# CI (.github/workflows/ci-jobs.yml) runs these same steps as parallel
# jobs by invoking this script with step names; running it with no
# arguments runs everything in sequence — do that before opening a PR.
#
# Usage:
#   ./scripts/check.sh              # run all steps
#   ./scripts/check.sh <step>...    # run only the named steps
#
# Steps: fixtures fmt clippy test wasm audit
#
# The audit step is advisory in the full run (CI marks its audit job
# continue-on-error; a local full run mirrors that and only warns). When
# named explicitly, audit propagates cargo-audit's exit code so the CI
# job surfaces failures.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

run_fixtures() {
    echo "==> Build wasm test fixtures"
    ./scripts/build-fixtures.sh
}

run_fmt() {
    echo "==> Format (nightly rustfmt)"
    cargo +nightly fmt --all -- --check
}

run_clippy() {
    echo "==> Clippy"
    cargo clippy --workspace --all-targets -- -D warnings
}

run_test() {
    echo "==> Tests"
    cargo test --workspace

    # SEC-09: the registry-download SSRF e2e has an allow-private-network
    # half (a local wiremock registry served end-to-end) that only compiles
    # under the escape-hatch feature — no other job enables it.
    echo "==> Registry SSRF escape-hatch e2e (allow-private-network)"
    cargo test -p wafer-run --features allow-private-network --test registry_ssrf
}

run_wasm() {
    echo "==> Guest SDK builds to wasm32-wasip1"
    cargo build -p wafer-block -p wafer-sdk --target wasm32-wasip1

    echo "==> Runtime builds with --no-default-features (wasmi off)"
    cargo check -p wafer-run --no-default-features

    echo "==> Guest service-client path (wafer-core wasm-component → wasm32)"
    cargo check -p wafer-core --features wasm-component --target wasm32-wasip1

    # Guards the gizza consumer combo (default-features=false, features=["wasmi"]
    # on wasm32-unknown-unknown), which no other job covers — run_wasm's
    # --no-default-features check has wasmi OFF. This is the combo that
    # regressed silently in #234 (embed::register_path reading from disk).
    echo "==> Runtime builds on wasm32-unknown-unknown with --features wasmi (gizza combo)"
    cargo build -p wafer-run --target wasm32-unknown-unknown --no-default-features --features wasmi
}

run_audit() {
    echo "==> Security audit"
    cargo audit
}

if [ "$#" -eq 0 ]; then
    run_fixtures
    run_fmt
    run_clippy
    run_test
    run_wasm
    if ! run_audit; then
        echo "warning: cargo audit reported issues (non-blocking, matches CI)" >&2
    fi
    echo "==> All checks passed."
else
    for step in "$@"; do
        case "$step" in
            fixtures) run_fixtures ;;
            fmt) run_fmt ;;
            clippy) run_clippy ;;
            test) run_test ;;
            wasm) run_wasm ;;
            audit) run_audit ;;
            *)
                echo "error: unknown step '$step' (valid: fixtures fmt clippy test wasm audit)" >&2
                exit 2
                ;;
        esac
    done
fi
