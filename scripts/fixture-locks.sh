#!/usr/bin/env bash
# Keeps the lockfiles of the out-of-workspace fixture crates on the
# workspace's dependency versions.
#
# Each fixture (wasm32 compile fixtures, wasm guest fixtures, the wafer-cli
# dev fixture, examples/wasmi-block) has its own `[workspace]` table and so
# its own committed Cargo.lock, which cargo resolves independently of the
# root one. Left alone, those locks pin whatever was newest on the day they
# were created: a fixture that exists to prove "this builds the way an
# embedder builds it" would then build different versions of wasm-bindgen,
# uuid, getrandom, … from the ones the workspace Cargo.lock pins.
#
# Usage:
#   ./scripts/fixture-locks.sh check   # fail on drift (check.sh fixtures runs this)
#   ./scripts/fixture-locks.sh sync    # re-seed every fixture lock from the root lock
#
# `check`: every crate that appears in both a fixture lock and the root
# Cargo.lock must resolve, in the fixture, to a version the root lock also
# has (the root lock may carry several versions of one crate; a fixture may
# use any of them). Crates only a fixture uses are not compared.
#
# `sync`: copies the root Cargo.lock over each fixture's and lets cargo
# prune it to the fixture's graph, so every shared crate keeps the root
# version. Run it after any change to the root Cargo.lock (a Dependabot
# bump included — Dependabot updates only the root lock, and `check` then
# turns that PR red until the fixtures follow).
#
# Fixtures are discovered, not listed: every tracked Cargo.lock other than
# the root one, so a new fixture cannot opt out by omission.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

mapfile -t fixture_locks < <(git ls-files -- '*Cargo.lock' | grep -v '^Cargo\.lock$')

# `name version` for every [[package]] in a Cargo.lock, one per line.
lock_packages() {
    awk '
        /^\[\[package\]\]/ { name = "" }
        /^name = "/ { name = $3; gsub(/"/, "", name) }
        /^version = "/ { v = $3; gsub(/"/, "", v); if (name != "") print name, v }
    ' "$1" | sort -u
}

check() {
    local root status=0 lock drift
    root=$(lock_packages Cargo.lock)
    for lock in "${fixture_locks[@]}"; do
        drift=$(lock_packages "$lock" | awk '
            NR == FNR { have[$1 " " $2] = 1; known[$1] = 1; next }
            ($1 in known) && !(($1 " " $2) in have) { print }
        ' <(printf '%s\n' "$root") -)
        if [ -n "$drift" ]; then
            status=1
            echo "error: $lock resolves crates to versions the root Cargo.lock does not pin:" >&2
            while read -r name version; do
                echo "  $name $version (root: $(printf '%s\n' "$root" | awk -v n="$name" '$1 == n { printf "%s ", $2 }'))" >&2
            done <<< "$drift"
        fi
    done
    if [ "$status" -ne 0 ]; then
        echo "fix: ./scripts/fixture-locks.sh sync" >&2
    else
        echo "Fixture lockfiles match the root Cargo.lock (${#fixture_locks[@]} checked)."
    fi
    return "$status"
}

sync() {
    local lock
    for lock in "${fixture_locks[@]}"; do
        echo "syncing $lock"
        cp Cargo.lock "$lock"
        cargo metadata --format-version 1 \
            --manifest-path "$(dirname "$lock")/Cargo.toml" > /dev/null
    done
    check
}

case "${1:-check}" in
    check) check ;;
    sync) sync ;;
    *)
        echo "usage: $0 [check|sync]" >&2
        exit 2
        ;;
esac
