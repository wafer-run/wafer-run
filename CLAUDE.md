# Development Guidelines

- Always fix the real issue. No code smells, no shortcuts, no workarounds.
- If the right fix requires touching many files, touch many files.
- No sync bridges (`poll_once`, `block_on`) to avoid propagating async. If something is async, callers must be async.
- No magic code or implicit mapping layers. Keep things explicit and easy to maintain. If a value has a prefix, it has the same prefix everywhere (env vars, D1, config API). No translation between representations.
