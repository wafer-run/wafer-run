#!/usr/bin/env bash
# Structural checks on CI configuration that actionlint does not make.
# CI runs this in the `workflows` job (ci-jobs.yml); it needs `yq`
# (mikefarah/yq v4, preinstalled on GitHub's ubuntu runners — the Python
# `yq` wrapper is a different tool and is refused).
#
# 1. Every `uses:` in .github/workflows is a local workflow (`./…`) or an
#    action pinned by a full 40-hex commit SHA. A tag or branch ref (`@v4`,
#    `@main`) can be moved by the action's owner to code this repo never
#    reviewed.
# 2. `ci-ok` in ci-jobs.yml needs exactly the other jobs in that file and
#    runs under `if: always()`. It is the one required status check, so a
#    job missing from its `needs` would not gate a merge, and without
#    always() a failed dependency skips it instead of turning it red.
# 3. Every advisory ignored in .cargo/audit.toml has its own line, directly
#    below a comment block holding a `REASON:` and a `REMOVE WHEN:` line.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if ! yq_version=$(yq --version 2> /dev/null) || [[ "$yq_version" != *mikefarah* ]]; then
    echo "error: mikefarah/yq v4 is required (found: ${yq_version:-no yq on PATH})" >&2
    exit 2
fi

status=0

# ── 1. action pins ────────────────────────────────────────────────────
shopt -s nullglob
workflows=(.github/workflows/*.yml .github/workflows/*.yaml)
shopt -u nullglob
if [ "${#workflows[@]}" -eq 0 ]; then
    echo "error: no workflow files found under .github/workflows" >&2
    exit 2
fi
for wf in "${workflows[@]}"; do
    # Captured, not streamed through `< <(…)`: a failing yq inside a
    # process substitution does not trip `set -e`, and would read as
    # "no uses: lines".
    refs=$(yq -r '.. | select(tag == "!!map" and has("uses")) | .uses' "$wf")
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        case "$ref" in
            ./*) continue ;;
        esac
        if ! [[ "$ref" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$ ]]; then
            echo "error: $wf: '$ref' is not pinned by a full commit SHA" >&2
            status=1
        fi
    done <<< "$refs"
done

# ── 2. ci-ok covers every job ─────────────────────────────────────────
jobs_file=.github/workflows/ci-jobs.yml
expected=$(yq -r '.jobs | keys | .[] | select(. != "ci-ok")' "$jobs_file" | sort)
actual=$(yq -r '.jobs."ci-ok".needs[]' "$jobs_file" | sort)
if [ "$expected" != "$actual" ]; then
    echo "error: $jobs_file: ci-ok.needs must list every other job exactly once" >&2
    diff <(echo "$expected") <(echo "$actual") | sed 's/^/  /' >&2 || true
    status=1
fi
ci_ok_if=$(yq -r '.jobs."ci-ok".if' "$jobs_file")
if [ "$ci_ok_if" != "always()" ]; then
    echo "error: $jobs_file: ci-ok must run with 'if: always()'" >&2
    status=1
fi

# ── 3. every audit ignore is justified ────────────────────────────────
audit_file=.cargo/audit.toml
# What cargo-audit reads: the parsed ignore list.
parsed=$(yq -p toml -o yaml -r '.advisories.ignore[]' "$audit_file" | sort)
# What the comment check sees: one quoted id per line, each paired with
# the comment block directly above it. The two lists must agree, so an
# id this line-based reading misses (two on a line, a trailing comment)
# fails instead of escaping the check.
documented=$(awk '
    /^[[:space:]]*#/ { block = block $0 "\n"; next }
    /^[[:space:]]*"[^"]+",?[[:space:]]*$/ {
        id = $0; gsub(/[[:space:]",]/, "", id)
        ok = (block ~ /REASON:/ && block ~ /REMOVE WHEN:/) ? "ok" : "missing"
        print id, ok
        block = ""; next
    }
    { block = "" }
' "$audit_file")
if [ "$parsed" != "$(awk '{ print $1 }' <<< "$documented" | sort)" ]; then
    echo "error: $audit_file: put each ignored advisory id on its own line (\"ID\",) below its comment block" >&2
    status=1
fi
while read -r id ok; do
    [ -n "$id" ] || continue
    if [ "$ok" != ok ]; then
        echo "error: $audit_file: $id needs a comment block above it with a REASON: and a REMOVE WHEN: line" >&2
        status=1
    fi
done <<< "$documented"

if [ "$status" -eq 0 ]; then
    echo "Workflows OK."
fi
exit "$status"
