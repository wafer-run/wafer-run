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
# Steps: fixtures fmt clippy test postgres wasm bindings audit
#
# The postgres step runs the shared DatabaseService conformance suite
# against a live PostgreSQL server named by WAFER_CONFORMANCE_POSTGRES_URL
# (CI provides one as a service container). Named explicitly, it FAILS
# when the variable is unset — the test itself skips without it, so a
# missing URL must not read as a pass. In the no-argument full run it is
# skipped, loudly, when the variable is unset. To run it locally:
#
#   docker run --rm -d -p 5432:5432 -e POSTGRES_PASSWORD=pw postgres:16
#   WAFER_CONFORMANCE_POSTGRES_URL=postgres://postgres:pw@localhost:5432/postgres \
#     ./scripts/check.sh postgres
#
# The audit step is BLOCKING, here and in CI. It used to be advisory —
# CI's audit job carried continue-on-error and the full local run only
# warned to match. PR #287 made the CI job blocking but left this script
# warning, so `check.sh` printed "All checks passed." while CI went red.
# Both paths now propagate cargo-audit's exit code.
#
# Every cargo command that resolves dependencies passes `--locked`, here and
# in scripts/build-fixtures.sh: a Cargo.lock (the workspace's or a
# fixture's) that no longer matches its manifests fails the step instead of
# being silently re-resolved, so what CI tests is what the lockfile pins.
#
# The bindings step builds and tests the non-Rust embedder surfaces: the C
# ABI (wafer-ffi), the Go SDK linked against it, the Node addon
# (wafer-run-node) and packages/wafer-client-js. It needs `go` and `npm` on
# PATH and the wasm fixtures built. Named explicitly, it fails without
# them; the no-argument full run skips it, loudly, when either is missing.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

run_fixtures() {
    # The fixtures' own Cargo.locks are resolved separately from the root
    # one; this fails when a crate they share resolves to a version the root
    # lock does not pin (see the script's header).
    echo "==> Fixture lockfiles match the root Cargo.lock"
    ./scripts/fixture-locks.sh check

    echo "==> Build wasm test fixtures"
    ./scripts/build-fixtures.sh
}

run_fmt() {
    echo "==> Format (nightly rustfmt)"
    cargo +nightly fmt --all -- --check

    # The wasm compile fixtures carry their own `[workspace]` table so
    # `cargo build --workspace` does not pick them up — which also puts them
    # outside `fmt --all` above. They are ordinary hand-written Rust that
    # run_wasm builds, so format them by the same rule, named explicitly.
    for fixture in \
        crates/wafer-block/tests/wasm_static_blocks \
        crates/wafer-block/tests/wasm_local_input_stream \
        crates/wafer-block-crypto/tests/wasm32_consumer; do
        cargo +nightly fmt --all --manifest-path "$fixture/Cargo.toml" -- --check
    done
}

run_clippy() {
    echo "==> Clippy"
    cargo clippy --locked --workspace --all-targets -- -D warnings

    # Opt-in features no workspace member enables, so the workspace run above
    # never compiles the code behind them. Downstream embedders ship both:
    # `json-schema` gates the schemars derives on wafer-block's wire types,
    # `vectors` gates wafer-block-sqlite's sqlite-vec VectorService.
    echo "==> Clippy: wafer-block --features json-schema"
    cargo clippy --locked -p wafer-block --features json-schema --all-targets -- -D warnings
    echo "==> Clippy: wafer-block-sqlite --features vectors"
    cargo clippy --locked -p wafer-block-sqlite --features vectors --all-targets -- -D warnings
}

run_test() {
    echo "==> Tests"
    cargo test --locked --workspace

    # The sqlite-vec VectorService and its tests (unit tests in
    # src/vector.rs, tests/vector_sql_roundtrip.rs,
    # tests/vector_integration.rs) are all behind the `vectors` feature,
    # which the workspace run above does not enable.
    echo "==> wafer-block-sqlite vector service (vectors)"
    cargo test --locked -p wafer-block-sqlite --features vectors

    # SEC-09: the registry-download SSRF e2e has an allow-private-network
    # half (a local wiremock registry served end-to-end) that only compiles
    # under the escape-hatch feature — no other job enables it. The
    # lockfile-pinned download e2e (integrity, bounds, admission) uses the
    # same local registry.
    echo "==> Registry escape-hatch e2e (allow-private-network)"
    cargo test --locked -p wafer-run --features allow-private-network \
        --test registry_ssrf --test remote_integrity

    # The outbound-network redirect (per-hop grant check) and timeout e2es
    # are likewise only reachable under the escape-hatch feature (a local
    # server on loopback that the SSRF gate otherwise blocks).
    echo "==> Network redirect + timeout escape-hatch e2e (allow-private-network)"
    cargo test --locked -p wafer-block-network --features allow-private-network --test redirect_ssrf --test timeouts
}

run_postgres() {
    echo "==> PostgreSQL conformance (live server)"
    if [ -z "${WAFER_CONFORMANCE_POSTGRES_URL:-}" ]; then
        echo "error: WAFER_CONFORMANCE_POSTGRES_URL is not set; the postgres step needs a live server (see the header of this script)" >&2
        exit 1
    fi
    cargo test --locked -p wafer-block-postgres --test conformance
}

run_wasm() {
    echo "==> Guest SDK builds to wasm32-wasip1"
    cargo build --locked -p wafer-block -p wafer-sdk --target wasm32-wasip1

    echo "==> Runtime builds with --no-default-features (wasmi off)"
    cargo check --locked -p wafer-run --no-default-features

    echo "==> Guest service-client path (wafer-core wasm-component → wasm32)"
    cargo check --locked -p wafer-core --features wasm-component --target wasm32-wasip1

    # Guards the gizza consumer combo (default-features=false, features=["wasmi"]
    # on wasm32-unknown-unknown), which no other job covers — run_wasm's
    # --no-default-features check has wasmi OFF. This is the combo that
    # regressed silently in #234 (embed::register_path reading from disk).
    echo "==> Runtime builds on wasm32-unknown-unknown with --features wasmi (gizza combo)"
    cargo build --locked -p wafer-run --target wasm32-unknown-unknown --no-default-features --features wasmi

    # `register_static_block!` and `use_static_blocks!` each have a wasm32
    # arm that exists precisely because `linkme` has no link section there.
    # No native build expands either one, and no crate in the workspace both
    # invokes `use_static_blocks!` and compiles for wasm32 (the http-server
    # flow pulls tokio's net stack), so without this the arms are dead
    # source: a bad path or a silently empty list would only surface in a
    # downstream Worker build. The fixture asserts its own list length at
    # compile time.
    echo "==> Static block registration on wasm32 (macro arms)"
    cargo build --locked --target wasm32-unknown-unknown \
        --manifest-path crates/wafer-block/tests/wasm_static_blocks/Cargo.toml

    # `InputStream` boxes a `LocalBoxStream` on wasm32 so a JS-backed request
    # body (always `!Send`, it holds a `JsValue`) can be streamed to a block
    # instead of buffered. No native build can construct such a body, so a
    # bound that drifted back to `Send` would compile green in every other job
    # and strand Worker/service-worker adapters. The fixture typechecks
    # wrapping a `!Send` body, collecting it, and handing it to the streaming
    # storage client; nothing here runs, so it proves the bounds, not bytes.
    echo "==> Local (!Send) request bodies on wasm32 (InputStream inner type)"
    cargo build --locked --target wasm32-unknown-unknown \
        --manifest-path crates/wafer-block/tests/wasm_local_input_stream/Cargo.toml

    # wafer-block-crypto is linked by Worker and browser embedders on
    # wasm32-unknown-unknown, and nothing above builds it for that target
    # (the static-blocks fixture lists middleware blocks only). The fixture
    # is the embedding binary's stand-in: it picks the JS randomness source
    # and names the primitives and the service, so they are code-generated
    # for wasm32, not only typechecked.
    echo "==> wafer-block-crypto on wasm32-unknown-unknown"
    cargo build --locked --target wasm32-unknown-unknown \
        --manifest-path crates/wafer-block-crypto/tests/wasm32_consumer/Cargo.toml
}

run_bindings() {
    local lib_dir="${CARGO_TARGET_DIR:-$PWD/target}/debug"

    echo "==> C ABI: build libwafer_ffi and run its extern \"C\" smoke tests"
    cargo build --locked -p wafer-ffi
    cargo test --locked -p wafer-ffi

    echo "==> C header: the Go copy matches, and it declares every exported symbol"
    if ! cmp -s crates/wafer-ffi/wafer.h go/wafer-run-go/wafer.h; then
        echo "error: go/wafer-run-go/wafer.h differs from crates/wafer-ffi/wafer.h;" >&2
        echo "       copy the crate's header over it" >&2
        diff -u crates/wafer-ffi/wafer.h go/wafer-run-go/wafer.h >&2 || true
        exit 1
    fi
    local lib="$lib_dir/libwafer_ffi.so" nm_flags=(-D --defined-only) sym_prefix=""
    if [ "$(uname -s)" = Darwin ]; then
        lib="$lib_dir/libwafer_ffi.dylib" nm_flags=(-gU) sym_prefix="_"
    fi
    local exported declared
    exported="$(nm "${nm_flags[@]}" "$lib" \
        | awk -v p="${sym_prefix}wafer_" '$2 == "T" && index($3, p) == 1 { print substr($3, length(p) - 5) }' \
        | sort)"
    declared="$(grep -oE '^[A-Za-z].*[^A-Za-z_]wafer_[a-z_]+\(' crates/wafer-ffi/wafer.h \
        | grep -oE 'wafer_[a-z_]+\($' | tr -d '(' | sort)"
    if [ -z "$exported" ] || [ "$exported" != "$declared" ]; then
        echo "error: crates/wafer-ffi/wafer.h does not declare exactly the wafer_* symbols $lib exports" >&2
        diff -u <(echo "$declared") <(echo "$exported") >&2 || true
        exit 1
    fi

    echo "==> Go SDK (cgo, linked against libwafer_ffi)"
    local unformatted
    unformatted="$(gofmt -l go)"
    if [ -n "$unformatted" ]; then
        echo "error: gofmt would reformat:" >&2
        echo "$unformatted" >&2
        exit 1
    fi
    (
        cd go/wafer-run-go
        export CGO_LDFLAGS="-L$lib_dir" LD_LIBRARY_PATH="$lib_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        go vet ./...
        # -count=1: go's test cache keys on Go sources, not on the
        # libwafer_ffi it links, so a cached pass could hide an ABI change.
        go test -count=1 ./...
    )

    echo "==> npm workspaces install (package-lock.json)"
    npm ci

    echo "==> Node addon (wafer-run-node): build from source, load, run a flow"
    npm run build-test -w crates/wafer-run-node
    # napi generates the typings from the addon's Rust docs and signatures;
    # the committed index.d.ts must be that output (`npm run build` in
    # crates/wafer-run-node rewrites it).
    if ! diff -u crates/wafer-run-node/index.d.ts crates/wafer-run-node/test-build/index.d.ts; then
        echo "error: crates/wafer-run-node/index.d.ts is not what napi generates;" >&2
        echo "       copy crates/wafer-run-node/test-build/index.d.ts over it" >&2
        exit 1
    fi
    npm test -w crates/wafer-run-node

    echo "==> wafer-client-js: typecheck, test, build"
    npm run typecheck -w packages/wafer-client-js
    npm test -w packages/wafer-client-js
    npm run build -w packages/wafer-client-js
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
    if [ -n "${WAFER_CONFORMANCE_POSTGRES_URL:-}" ]; then
        run_postgres
    else
        echo "==> SKIPPED PostgreSQL conformance: WAFER_CONFORMANCE_POSTGRES_URL is not set (CI runs it)"
    fi
    run_wasm
    if command -v go >/dev/null && command -v npm >/dev/null; then
        run_bindings
    else
        echo "==> SKIPPED bindings: needs go and npm on PATH (CI runs it)"
    fi
    run_audit
    echo "==> All checks passed."
else
    for step in "$@"; do
        case "$step" in
            fixtures) run_fixtures ;;
            fmt) run_fmt ;;
            clippy) run_clippy ;;
            test) run_test ;;
            postgres) run_postgres ;;
            wasm) run_wasm ;;
            bindings) run_bindings ;;
            audit) run_audit ;;
            *)
                echo "error: unknown step '$step' (valid: fixtures fmt clippy test postgres wasm bindings audit)" >&2
                exit 2
                ;;
        esac
    done
fi
