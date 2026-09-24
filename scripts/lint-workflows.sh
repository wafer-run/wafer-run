#!/usr/bin/env bash
# Structural checks on .github/workflows that actionlint does not make.
# CI runs this in the `workflows` job (ci-jobs.yml); it needs `yq`
# (mikefarah/yq v4, preinstalled on GitHub's ubuntu runners).
#
# 1. Every `uses:` is a local workflow (`./…`) or an action pinned by a
#    full 40-hex commit SHA. A tag or branch ref (`@v4`, `@main`) can be
#    moved by the action's owner to code this repo never reviewed.
# 2. `ci-ok` in ci-jobs.yml needs exactly the other jobs in that file and
#    runs under `if: always()`. It is the one required status check, so a
#    job missing from its `needs` would not gate a merge, and without
#    always() a failed dependency skips it instead of turning it red.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

command -v yq > /dev/null || {
    echo "error: yq (mikefarah/yq v4) is required" >&2
    exit 2
}

status=0

for wf in .github/workflows/*.yml; do
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        case "$ref" in
            ./*) continue ;;
        esac
        if ! [[ "$ref" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$ ]]; then
            echo "error: $wf: '$ref' is not pinned by a full commit SHA" >&2
            status=1
        fi
    done < <(yq -r '.. | select(tag == "!!map" and has("uses")) | .uses' "$wf")
done

jobs_file=.github/workflows/ci-jobs.yml
expected=$(yq -r '.jobs | keys | .[] | select(. != "ci-ok")' "$jobs_file" | sort)
actual=$(yq -r '.jobs."ci-ok".needs[]' "$jobs_file" | sort)
if [ "$expected" != "$actual" ]; then
    echo "error: $jobs_file: ci-ok.needs must list every other job exactly once" >&2
    diff <(echo "$expected") <(echo "$actual") | sed 's/^/  /' >&2 || true
    status=1
fi
if [ "$(yq -r '.jobs."ci-ok".if' "$jobs_file")" != "always()" ]; then
    echo "error: $jobs_file: ci-ok must run with 'if: always()'" >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    echo "Workflows OK."
fi
exit "$status"
