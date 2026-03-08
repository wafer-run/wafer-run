# Development Guidelines

- Always fix the real issue. No code smells, no shortcuts, no workarounds.
- If the right fix requires touching many files, touch many files.
- No sync bridges (`poll_once`, `block_on`) to avoid propagating async. If something is async, callers must be async.
