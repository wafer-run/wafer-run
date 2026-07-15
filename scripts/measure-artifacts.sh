#!/usr/bin/env bash
# PERF-06: measure clean release-build time and shipped binary size per
# deliverable artifact, plus per-artifact duplicate-dependency trees.
#
# Motivation: `cargo tree -d` on the whole workspace shows many duplicate
# major versions, but some duplication is dev-only or feature-only. Before
# aligning versions, measure what actually lands in each shipped artifact.
# Results feed docs/perf/2026-07-15-artifact-size-and-compile-time.md.
#
# Usage:
#   ./scripts/measure-artifacts.sh            # trees + builds (everything)
#   ./scripts/measure-artifacts.sh trees      # only the cargo-tree -d runs (fast)
#   ./scripts/measure-artifacts.sh builds     # only the timed clean builds (slow)
#
# Output: target-measure/results/
#   summary.tsv          one row per artifact (wall / user / sys / rss / size)
#   <name>.log           full cargo output + `ls -l` of the artifact
#   <name>.time          raw `/usr/bin/time -v` output
#   tree-<name>.txt      `cargo tree -d -e normal` scoped to that artifact
#
# Methodology notes:
# - Each build gets a FRESH per-artifact CARGO_TARGET_DIR under
#   ./target-measure/<name> (never a shared target dir), so every timing is
#   a true clean release build. Builds run strictly serialized.
# - Dependencies are prefetched (`cargo fetch`) before any timed build so
#   network time is not counted. Exception: ort-sys (fastembed) may download
#   the ONNX Runtime binary from its build script; that is part of its real
#   build cost and is noted in the doc.
# - `uptime` is recorded before each build: this box runs other cargo builds
#   concurrently in other worktrees, so wall-clock is noisy. user+sys CPU
#   seconds are the more comparable numbers. Re-run on a quiet machine for
#   publishable wall times.
# - The root workspace release profile already has `strip = true`, so native
#   sizes are as-shipped. The examples/wasmi-block standalone workspace does
#   NOT strip; for .wasm artifacts we additionally record a
#   `wasm-opt -Oz --strip-debug` size (informational — wasm-opt is not part
#   of this repo's pipeline today).
# - Per-artifact target dirs are deleted right after sizes are recorded
#   (disk space). Set KEEP_TARGETS=1 to keep them for inspection.
#
# Requires: GNU time at /usr/bin/time, rustup targets wasm32-wasip1 and
# wasm32-unknown-unknown (both pinned in rust-toolchain.toml).

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

TIME_BIN=/usr/bin/time
MEASURE_ROOT=target-measure
RESULTS="$MEASURE_ROOT/results"
mkdir -p "$RESULTS"
SUMMARY="$RESULTS/summary.tsv"

# ---------------------------------------------------------------------------
# Artifact inventory. Format:  name | artifact path inside its target dir
#                              ("" = rlib-only, no shipped file) | build cmd...
#
# Deliverables named by the review and where they live in THIS repo:
#   minimal runtime    -> wafer-run --no-default-features (rlib; embedders set
#                         the floor — no standalone binary exists by design)
#   full native server -> no single in-repo binary bundles s3+postgres+
#                         fastembed (that composition lives downstream in
#                         impresspress); examples/hello-world (batteries) and
#                         examples/api-server (batteries + sqlite + inspector)
#                         are the in-repo server binaries, and the heavy
#                         optional blocks are measured as libs below.
#   CLI                -> wafer-cli => target/release/wafer
#   Node addon         -> wafer-run-node cdylib (napi renames the .so to
#                         wafer-run.node; `cargo build` produces the same file)
#   C FFI              -> wafer-ffi cdylib
#   wasm32 targets     -> examples/wasmi-block guest (wasm32-wasip1, its own
#                         workspace = the fixture/guest pipeline), and
#                         wafer-run on wasm32-unknown-unknown with
#                         --features wasmi (the gizza/browser embed combo)
# ---------------------------------------------------------------------------
ARTIFACTS=(
    "runtime-min-lib||cargo build --release --locked -p wafer-run --no-default-features"
    "runtime-wasm32-wasmi-lib||cargo build --release --locked -p wafer-run --target wasm32-unknown-unknown --no-default-features --features wasmi"
    "server-hello-world|release/hello-world|cargo build --release --locked -p hello-world"
    "server-api-sqlite|release/api-server|cargo build --release --locked -p api-server"
    "cli-wafer|release/wafer|cargo build --release --locked -p wafer-cli"
    "node-addon|release/libwafer_run_node.so|cargo build --release --locked -p wafer-run-node"
    "c-ffi|release/libwafer_ffi.so|cargo build --release --locked -p wafer-ffi"
    "wasm-guest-echo|wasm32-wasip1/release/wafer_example_wasmi_echo.wasm|cargo build --release --locked --manifest-path examples/wasmi-block/Cargo.toml --target wasm32-wasip1"
    "lib-s3||cargo build --release --locked -p wafer-block-s3"
    "lib-fastembed||cargo build --release --locked -p wafer-block-fastembed"
    "lib-postgres||cargo build --release --locked -p wafer-block-postgres"
)

# Per-artifact duplicate-dependency trees: name | cargo tree args...
# -e normal excludes dev/build edges => what actually links into the artifact.
TREES=(
    "runtime-min-lib|-p wafer-run --no-default-features"
    "runtime-wasm32-wasmi-lib|-p wafer-run --no-default-features --features wasmi --target wasm32-unknown-unknown"
    "server-hello-world|-p hello-world"
    "server-api-sqlite|-p api-server"
    "cli-wafer|-p wafer-cli"
    "node-addon|-p wafer-run-node"
    "c-ffi|-p wafer-ffi"
    "wasm-guest-echo|--manifest-path examples/wasmi-block/Cargo.toml --target wasm32-wasip1"
    "lib-s3|-p wafer-block-s3"
    "lib-fastembed|-p wafer-block-fastembed"
    "lib-postgres|-p wafer-block-postgres"
    "workspace-aggregate-normal|--workspace"
)

count_dupes() { # $1 = tree output file; counts top-level duplicated crates
    grep -c '^[a-zA-Z0-9_-]* v' "$1" || true
}

run_trees() {
    echo "==> cargo tree -d per artifact (normal edges only)"
    for spec in "${TREES[@]}"; do
        IFS='|' read -r name args <<<"$spec"
        out="$RESULTS/tree-$name.txt"
        # shellcheck disable=SC2086
        cargo tree -d -e normal $args >"$out" 2>&1 || true
        echo "    tree-$name.txt ($(count_dupes "$out") duplicated-crate entries)"
    done

    # Aggregate INCLUDING dev/build edges, for contrast with the normal-edge
    # trees above (this is the number the review's `cargo tree -d` quoted).
    out="$RESULTS/tree-workspace-aggregate-all-edges.txt"
    cargo tree -d --workspace >"$out" 2>&1 || true
    echo "    tree-workspace-aggregate-all-edges.txt ($(count_dupes "$out") duplicated-crate entries)"
}

extract_time() { # $1=timefile $2=pattern
    grep "$2" "$1" | sed 's/.*: //' | tr -d ' '
}

run_builds() {
    echo -e "artifact\twall_clock\tuser_s\tsys_s\tmax_rss_kb\tsize_bytes\twasm_opt_bytes" >"$SUMMARY"

    echo "==> Prefetching dependencies (untimed)"
    cargo fetch --locked
    cargo fetch --locked --manifest-path examples/wasmi-block/Cargo.toml

    for spec in "${ARTIFACTS[@]}"; do
        IFS='|' read -r name artifact_rel cmd <<<"$spec"
        tdir="$MEASURE_ROOT/$name"
        rm -rf "$tdir"
        log="$RESULTS/$name.log"
        timefile="$RESULTS/$name.time"
        : >"$log"

        echo "==> [$name] $cmd"
        {
            echo "=== $name ==="
            echo "cmd: $cmd"
            echo "toolchain: $(rustc --version)"
            echo "load-before: $(uptime)"
        } >>"$log"

        # shellcheck disable=SC2086
        CARGO_TARGET_DIR="$tdir" "$TIME_BIN" -v -o "$timefile" $cmd >>"$log" 2>&1

        wall=$(extract_time "$timefile" 'Elapsed (wall clock)')
        user=$(extract_time "$timefile" 'User time (seconds)')
        sys=$(extract_time "$timefile" 'System time (seconds)')
        rss=$(extract_time "$timefile" 'Maximum resident set size')

        size="-"
        wasmopt="-"
        if [ -n "$artifact_rel" ]; then
            f="$tdir/$artifact_rel"
            size=$(stat -c %s "$f")
            ls -l "$f" >>"$log"
            case "$f" in
            *.wasm)
                if command -v wasm-opt >/dev/null 2>&1; then
                    wasm-opt -Oz --strip-debug "$f" -o "$f.opt" 2>>"$log" || true
                    [ -f "$f.opt" ] && wasmopt=$(stat -c %s "$f.opt")
                fi
                ;;
            esac
        fi

        echo -e "$name\t$wall\t$user\t$sys\t$rss\t$size\t$wasmopt" >>"$SUMMARY"
        echo "    wall=$wall user=${user}s sys=${sys}s size=$size"

        if [ "${KEEP_TARGETS:-0}" != "1" ]; then
            rm -rf "$tdir"
        fi
    done

    echo "==> Summary ($SUMMARY):"
    column -t -s $'\t' "$SUMMARY" || cat "$SUMMARY"
}

case "${1:-all}" in
trees) run_trees ;;
builds) run_builds ;;
all)
    run_trees
    run_builds
    ;;
*)
    echo "usage: $0 [trees|builds|all]" >&2
    exit 2
    ;;
esac
