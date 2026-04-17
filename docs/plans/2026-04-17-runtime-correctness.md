# Runtime correctness implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship two runtime validators — action-vs-interface at `call_block` dispatch and required-config presence at `Wafer::start()` — without changing public types or breaking existing consumers.

**Architecture:** One new module `crates/wafer-run/src/runtime/validation.rs` holds pure validator functions. Two integration points: `Wafer::resolve()` calls the config validator before the existing configs drain; `RuntimeContext::call_block()` calls the interface-action validator after block resolution, before `block.handle()`. A pre-landing audit pass fixes any existing block whose empty-default `ConfigVar` was silently treated as optional, and any `call_block` call site whose action is absent from the target interface's action map.

**Tech Stack:** Rust, `wafer-run` workspace, `tracing`, `tracing-test` (new dev-dep for log capture).

**Spec:** `docs/specs/2026-04-17-runtime-correctness-design.md`

---

## Task 1: Scaffold the `validation` module

**Files:**
- Create: `crates/wafer-run/src/runtime/validation.rs`
- Modify: `crates/wafer-run/src/runtime/mod.rs`

- [ ] **Step 1: Create the empty module file**

```rust
//! Runtime validators: block interface action checks and required-config presence checks.
//!
//! Pure functions — no mutation of runtime state. Called from `Wafer::resolve()`
//! (config presence) and `RuntimeContext::call_block()` (interface action).
```

Write this as the entire contents of `crates/wafer-run/src/runtime/validation.rs`.

- [ ] **Step 2: Register the module**

Open `crates/wafer-run/src/runtime/mod.rs`. Add `pub mod validation;` alongside the existing module declarations. Keep alphabetical order with the existing `mod` items.

- [ ] **Step 3: Verify it compiles**

Run: `cd /home/joris/Programs/suppers-ai/workspace/wafer-run && cargo check -p wafer-run`
Expected: clean build, no warnings introduced.

- [ ] **Step 4: Commit**

```bash
cd /home/joris/Programs/suppers-ai/workspace/wafer-run
git add crates/wafer-run/src/runtime/validation.rs crates/wafer-run/src/runtime/mod.rs
git commit -m "feat(wafer-run): scaffold runtime validation module"
```

---

## Task 2: Implement `validate_config_presence` (TDD)

**Files:**
- Modify: `crates/wafer-run/src/runtime/validation.rs`

- [ ] **Step 1: Write failing test — single required key missing**

Append to `crates/wafer-run/src/runtime/validation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wafer_block::types::{BlockInfo, ConfigVar};

    fn mk_block(name: &str, cfg_vars: Vec<ConfigVar>) -> BlockInfo {
        let mut info = BlockInfo::new(name, "0.1.0", "test@v1", "test");
        info.config_keys = cfg_vars;
        info
    }

    #[test]
    fn config_required_empty_no_default_missing() {
        let info = mk_block(
            "org/a",
            vec![ConfigVar::new("ORG__A__KEY", "desc", "")],
        );
        let cfg = serde_json::json!({});
        let missing = collect_missing_config(&[(info, &cfg)]);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].block_name, "org/a");
        assert_eq!(missing[0].key, "ORG__A__KEY");
    }
}
```

- [ ] **Step 2: Run test — expect fail**

Run: `cd /home/joris/Programs/suppers-ai/workspace/wafer-run && cargo test -p wafer-run runtime::validation::tests::config_required_empty_no_default_missing`
Expected: FAIL — `collect_missing_config` and `MissingConfig` not defined.

- [ ] **Step 3: Implement the validator**

Add **above** the `#[cfg(test)]` block in `validation.rs`:

```rust
use wafer_block::types::BlockInfo;

/// A single `(block, key)` pair whose required config value was not provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingConfig {
    pub block_name: String,
    pub key: String,
}

/// Collect every required config key that is missing or empty, across the given blocks.
///
/// A `ConfigVar` is "required" when `default.is_empty() && !auto_generate`.
/// Config is passed as a `serde_json::Value` object (the shape stored in
/// `Wafer::block_configs`); presence is checked by reading the string-coerced
/// value for the declared key.
pub fn collect_missing_config<'a>(
    blocks: &'a [(BlockInfo, &'a serde_json::Value)],
) -> Vec<MissingConfig> {
    let mut out = Vec::new();
    for (info, cfg) in blocks {
        for cv in &info.config_keys {
            if !cv.default.is_empty() || cv.auto_generate {
                continue;
            }
            let provided = cfg
                .get(&cv.key)
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if !provided {
                out.push(MissingConfig {
                    block_name: info.name.clone(),
                    key: cv.key.clone(),
                });
            }
        }
    }
    out
}

/// Format a list of `MissingConfig` entries into a single multi-block error message.
///
/// Output shape: `"missing required config: [block-1: KEY_A, KEY_B; block-2: KEY_C]"`.
pub fn format_missing_config(missing: &[MissingConfig]) -> String {
    use std::collections::BTreeMap;
    let mut by_block: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for m in missing {
        by_block.entry(&m.block_name).or_default().push(&m.key);
    }
    let parts: Vec<String> = by_block
        .into_iter()
        .map(|(block, keys)| format!("{block}: {}", keys.join(", ")))
        .collect();
    format!("missing required config: [{}]", parts.join("; "))
}
```

- [ ] **Step 4: Re-run test — expect pass**

Run: `cargo test -p wafer-run runtime::validation::tests::config_required_empty_no_default_missing`
Expected: PASS.

- [ ] **Step 5: Add the remaining config tests**

Append inside the existing `tests` module:

```rust
    #[test]
    fn config_required_with_default_present() {
        let info = mk_block(
            "org/a",
            vec![ConfigVar::new("ORG__A__KEY", "desc", "fallback")],
        );
        let cfg = serde_json::json!({});
        assert!(collect_missing_config(&[(info, &cfg)]).is_empty());
    }

    #[test]
    fn config_auto_generate_skipped() {
        let mut cv = ConfigVar::new("ORG__A__SECRET", "desc", "");
        cv.auto_generate = true;
        let info = mk_block("org/a", vec![cv]);
        let cfg = serde_json::json!({});
        assert!(collect_missing_config(&[(info, &cfg)]).is_empty());
    }

    #[test]
    fn config_value_provided_passes() {
        let info = mk_block(
            "org/a",
            vec![ConfigVar::new("ORG__A__KEY", "desc", "")],
        );
        let cfg = serde_json::json!({ "ORG__A__KEY": "supplied" });
        assert!(collect_missing_config(&[(info, &cfg)]).is_empty());
    }

    #[test]
    fn config_multiple_missing_aggregated() {
        let a = mk_block(
            "org/a",
            vec![
                ConfigVar::new("ORG__A__K1", "desc", ""),
                ConfigVar::new("ORG__A__K2", "desc", ""),
            ],
        );
        let b = mk_block(
            "org/b",
            vec![ConfigVar::new("ORG__B__K1", "desc", "")],
        );
        let empty = serde_json::json!({});
        let missing = collect_missing_config(&[(a, &empty), (b, &empty)]);
        assert_eq!(missing.len(), 3);

        let rendered = format_missing_config(&missing);
        assert!(rendered.contains("org/a: ORG__A__K1, ORG__A__K2"));
        assert!(rendered.contains("org/b: ORG__B__K1"));
    }
```

- [ ] **Step 6: Run all validation tests — expect pass**

Run: `cargo test -p wafer-run runtime::validation::tests`
Expected: all five config tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/wafer-run/src/runtime/validation.rs
git commit -m "feat(wafer-run): collect_missing_config validator + tests"
```

---

## Task 3: Implement `validate_action_interface` (TDD — known interfaces)

**Files:**
- Modify: `crates/wafer-run/src/runtime/validation.rs`

- [ ] **Step 1: Write failing test — valid action passes**

Append inside the existing `tests` module:

```rust
    use std::collections::HashMap;
    use wafer_block::types::{ActionSpec, InterfaceSpec};

    fn db_interface() -> InterfaceSpec {
        let mut actions = HashMap::new();
        actions.insert(
            "retrieve".into(),
            ActionSpec { description: "".into(), message_schema: None, response_schema: None },
        );
        actions.insert(
            "list".into(),
            ActionSpec { description: "".into(), message_schema: None, response_schema: None },
        );
        InterfaceSpec {
            name: "database@v1".into(),
            description: "".into(),
            actions,
        }
    }

    #[test]
    fn interface_valid_action() {
        let specs = vec![db_interface()];
        let result = check_action_interface("org/sqlite", "database@v1", "retrieve", &specs);
        assert!(matches!(result, ActionCheck::Valid));
    }
```

- [ ] **Step 2: Run test — expect fail**

Run: `cargo test -p wafer-run runtime::validation::tests::interface_valid_action`
Expected: FAIL — `check_action_interface` and `ActionCheck` not defined.

- [ ] **Step 3: Implement the validator**

Add **above** the `#[cfg(test)]` block in `validation.rs` (keep alongside the config helpers):

```rust
use wafer_block::types::InterfaceSpec;

/// Result of checking whether an action is valid for a block's declared interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionCheck {
    /// Action is valid for the block's interface.
    Valid,
    /// The action is not listed in the interface's action map.
    ///
    /// Message is pre-formatted for use in a `WaferError::invalid_argument`.
    Invalid { message: String },
    /// The block's interface string does not match any registered `InterfaceSpec`.
    ///
    /// Caller should warn-once and then treat the call as valid (backward compat
    /// for custom interfaces).
    UnknownInterface,
}

/// Check whether `action` is part of the action map for the block's declared interface.
///
/// Rules:
/// - If the interface has an **empty** action map, it is action-agnostic
///   (e.g., `middleware@v1`): any action is valid.
/// - If the interface has a non-empty action map, `action` must be a key in it.
/// - If the interface name matches no registered `InterfaceSpec`, return
///   `UnknownInterface` so the caller can warn-once and proceed.
pub fn check_action_interface(
    block_name: &str,
    interface_name: &str,
    action: &str,
    specs: &[InterfaceSpec],
) -> ActionCheck {
    let Some(spec) = specs.iter().find(|s| s.name == interface_name) else {
        return ActionCheck::UnknownInterface;
    };
    if spec.actions.is_empty() {
        return ActionCheck::Valid;
    }
    if spec.actions.contains_key(action) {
        return ActionCheck::Valid;
    }
    ActionCheck::Invalid {
        message: format!(
            "block '{block_name}' with interface '{interface_name}' does not expose action '{action}'"
        ),
    }
}
```

- [ ] **Step 4: Run test — expect pass**

Run: `cargo test -p wafer-run runtime::validation::tests::interface_valid_action`
Expected: PASS.

- [ ] **Step 5: Add remaining action-check tests**

Append inside the existing `tests` module:

```rust
    #[test]
    fn interface_unknown_action_rejected() {
        let specs = vec![db_interface()];
        let result = check_action_interface("org/sqlite", "database@v1", "publish", &specs);
        match result {
            ActionCheck::Invalid { message } => {
                assert!(message.contains("org/sqlite"));
                assert!(message.contains("database@v1"));
                assert!(message.contains("publish"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn interface_action_agnostic_interface_passes_any() {
        let mw = InterfaceSpec {
            name: "middleware@v1".into(),
            description: "".into(),
            actions: HashMap::new(),
        };
        let specs = vec![mw];
        assert_eq!(
            check_action_interface("org/cors", "middleware@v1", "anything", &specs),
            ActionCheck::Valid
        );
    }

    #[test]
    fn interface_unknown_interface_returns_unknown() {
        let specs = vec![db_interface()];
        assert_eq!(
            check_action_interface("org/x", "my-org/custom@v1", "retrieve", &specs),
            ActionCheck::UnknownInterface
        );
    }
```

- [ ] **Step 6: Run all validation tests — expect pass**

Run: `cargo test -p wafer-run runtime::validation::tests`
Expected: every test in the module passes.

- [ ] **Step 7: Commit**

```bash
git add crates/wafer-run/src/runtime/validation.rs
git commit -m "feat(wafer-run): check_action_interface validator + tests"
```

---

## Task 4: Add `tracing-test` dev-dep and warn-once state machine

**Files:**
- Modify: `crates/wafer-run/Cargo.toml`
- Modify: `crates/wafer-run/src/runtime/validation.rs`

- [ ] **Step 1: Add the dev-dep**

Open `crates/wafer-run/Cargo.toml`. Under `[dev-dependencies]`, add:

```toml
tracing-test = "0.2"
```

Run: `cargo check -p wafer-run --tests`
Expected: clean.

- [ ] **Step 2: Write the warn-once test**

Append inside the existing `tests` module in `validation.rs`:

```rust
    use std::sync::Mutex;
    use std::collections::HashSet;

    #[test]
    #[tracing_test::traced_test]
    fn warn_once_unknown_interface_emits_exactly_one_line() {
        let warned: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
        warn_once_unknown_interface(&warned, "org/weird", "my-org/custom@v1");
        warn_once_unknown_interface(&warned, "org/weird", "my-org/custom@v1");
        warn_once_unknown_interface(&warned, "org/weird", "my-org/custom@v1");

        // Logs contain a single occurrence of the block name + interface
        assert!(logs_contain("org/weird"));
        let line_count = logs_contain_count("org/weird");
        assert_eq!(line_count, 1, "expected exactly one warning, got {line_count}");
    }

    /// Helper — count lines matching `needle` in the captured log buffer.
    fn logs_contain_count(needle: &str) -> usize {
        // tracing_test exposes logs_contain via a macro at test scope.
        // Fallback: the test body above uses `logs_contain` for presence and this helper
        // iterates captured events; if tracing_test's API differs, adapt here.
        use tracing_test::internal::*;
        global_buf()
            .lock()
            .unwrap()
            .lines()
            .filter(|l| l.contains(needle))
            .count()
    }
```

- [ ] **Step 3: Run — expect fail**

Run: `cargo test -p wafer-run runtime::validation::tests::warn_once_unknown_interface`
Expected: FAIL — `warn_once_unknown_interface` not defined.

- [ ] **Step 4: Implement**

Add **above** the `#[cfg(test)]` block in `validation.rs`:

```rust
use std::collections::HashSet;
use std::sync::Mutex;

/// Emit a `WARN`-level log line exactly once per `(block_name)` for the
/// lifetime of the `warned` set.
///
/// Called from `RuntimeContext::call_block()` when a target block declares
/// an interface name that isn't in the runtime's registered `InterfaceSpec`
/// set. Preserves backward compatibility for custom interfaces while
/// signalling to the block author that action validation isn't catching
/// mistakes for them.
pub fn warn_once_unknown_interface(
    warned: &Mutex<HashSet<String>>,
    block_name: &str,
    interface_name: &str,
) {
    let mut guard = warned.lock().expect("warn-once mutex poisoned");
    if guard.insert(block_name.to_string()) {
        tracing::warn!(
            block = %block_name,
            interface = %interface_name,
            "block declares unknown interface; skipping action validation"
        );
    }
}
```

- [ ] **Step 5: Re-run — expect pass**

Run: `cargo test -p wafer-run runtime::validation::tests::warn_once_unknown_interface`
Expected: PASS.

If the `tracing_test::internal::global_buf` API differs in the installed version, replace the `logs_contain_count` helper body with a straightforward `logs_assert` call (`tracing_test` 0.2 exposes `logs_contain(needle) -> bool`; use `logs_assert(|lines| lines.iter().filter(...).count() == 1)` or the crate's preferred API). Keep the test assertion: exactly one matching line.

- [ ] **Step 6: Commit**

```bash
git add crates/wafer-run/Cargo.toml crates/wafer-run/src/runtime/validation.rs
git commit -m "feat(wafer-run): warn_once_unknown_interface + tracing-test dev-dep"
```

---

## Task 5: Audit existing `ConfigVar` declarations

**Files:**
- Create: `docs/plans/2026-04-17-runtime-correctness-audit.md`

This task produces an inventory. No code changes yet.

- [ ] **Step 1: Survey empty-default declarations**

Run: `cd /home/joris/Programs/suppers-ai/workspace/wafer-run && rg --line-number 'ConfigVar::new\("[A-Z_]+",\s*"[^"]*",\s*""' crates/ sdks/ examples/`

For each hit, open the file and read 10 lines of context. Record the block that owns it.

- [ ] **Step 2: Survey `default_value("")` and structural empty defaults**

Also run: `rg --line-number 'default_value\(""\)' crates/ sdks/ examples/`
And: `rg --line-number 'default:\s*"",?\s*$' crates/ sdks/ examples/`

Add any new hits to the inventory.

- [ ] **Step 3: Classify each declaration**

Create `docs/plans/2026-04-17-runtime-correctness-audit.md`. For each declaration, record:

```markdown
## {block-name} / {KEY}

**File:** `{path:line}`
**auto_generate:** {yes|no}
**Current behavior:** {empty default; defaults to "" at runtime}
**Intended behavior:** {genuinely required | silently optional}
**Decision:** {keep as required | set default to "{value}" | set auto_generate = true}
**Justification:** {one line}
```

Use the block's rustdoc and any README, plus a quick look at how the key is consumed (`rg 'config_get\("{KEY}"\)'`), to decide intended behavior.

- [ ] **Step 4: Commit the inventory**

```bash
git add docs/plans/2026-04-17-runtime-correctness-audit.md
git commit -m "docs(plans): audit of ConfigVar declarations with empty defaults"
```

---

## Task 6: Apply ConfigVar fixes from the audit

**Files:**
- Modify: Each source file recorded in the audit that needs a change.

- [ ] **Step 1: Apply each fix**

For each entry in the audit with `Decision != "keep as required"`:
1. Open the file.
2. Either:
   - change the default from `""` to the explicit non-empty value, **or**
   - set `auto_generate = true` (via the builder method `.auto_generate()`), **or**
   - edit the block's rustdoc to document that the key is required and why.
3. Run `cargo check -p <affected-crate>` after each edit.

Commit each block's fixes individually with message `fix(<crate>): explicit default/auto-generate for <KEY>`.

- [ ] **Step 2: Run the full test suite**

Run: `cd /home/joris/Programs/suppers-ai/workspace/wafer-run && cargo test --workspace`
Expected: all tests still pass; no behavioral change at this point (validator isn't wired yet).

- [ ] **Step 3: Verify solobase still starts**

In the sibling solobase repo (`../solobase`), run its normal startup command (the README or `Cargo.toml` `[[bin]]` default — e.g., `cargo run --bin solobase` with the usual dev env file).
Expected: solobase reaches its "ready" state. No config error at startup. Tear down after confirming.

If solobase fails to start, the audit missed a silently-optional key. Add it to the audit doc, apply a fix, re-verify.

---

## Task 7: Audit `call_block` action usage vs interface action maps

**Files:**
- Modify: `docs/plans/2026-04-17-runtime-correctness-audit.md` (append a new section)

This task produces a second inventory. No code changes yet.

- [ ] **Step 1: Survey call sites**

Run: `cd /home/joris/Programs/suppers-ai/workspace/wafer-run && rg --line-number 'call_block\(' crates/ sdks/ examples/`
Also survey solobase: `cd ../solobase && rg --line-number 'call_block\('`

For each call site:
- identify the target block name (literal string or constant)
- identify the `msg.action()` / `Action::...` / `MessageAction::...` used
- look up the target block's declared `interface` in its `BlockInfo::new(..., "X@v1", ...)` call
- look up the action map for that interface in `crates/wafer-block/src/interfaces.rs`

- [ ] **Step 2: Record mismatches**

Append to `docs/plans/2026-04-17-runtime-correctness-audit.md`:

```markdown
## Call-site / interface mismatches

| Call site (file:line) | Target block | Target interface | Action used | In action map? | Resolution |
|---|---|---|---|---|---|
| ... | ... | ... | ... | yes/no | fix call / add action / N/A |
```

Fill the table for every call site. "Resolution":
- **fix call:** the action is wrong; change the caller to use the correct action.
- **add action:** the action is legitimate; extend the `InterfaceSpec` to include it.
- **N/A:** interface has empty action map (action-agnostic), or action is present.

- [ ] **Step 3: Commit**

```bash
git add docs/plans/2026-04-17-runtime-correctness-audit.md
git commit -m "docs(plans): audit of call_block action vs interface action maps"
```

---

## Task 8: Apply call-site / interface fixes from the audit

**Files:**
- Modify: Source files with mismatches (callers and/or `crates/wafer-block/src/interfaces.rs`).

- [ ] **Step 1: Fix each mismatch**

For each row with **Resolution = fix call**: edit the caller to use the correct action.

For each row with **Resolution = add action**: open `crates/wafer-block/src/interfaces.rs` and add the missing entry to the `actions` map of the relevant interface constructor (e.g., `database_v1()`). Add a short description.

After each edit, run `cargo check -p <affected-crate>`.

- [ ] **Step 2: Run full tests**

Run: `cargo test --workspace`
Expected: no regressions.

- [ ] **Step 3: Commit**

```bash
git add crates/
git commit -m "fix(wafer-run): reconcile call_block actions with interface action maps"
```

---

## Task 9: Wire `collect_missing_config` into `Wafer::resolve()`

**Files:**
- Modify: `crates/wafer-run/src/runtime/resolver.rs`

- [ ] **Step 1: Locate the injection point**

Open `crates/wafer-run/src/runtime/resolver.rs`. Find `pub async fn resolve(&mut self)` (around line 124). The validator call goes **after** `self.gather_uses_configs();` and **before** `self.block_configs_snapshot = Arc::new(self.block_configs.clone());`.

- [ ] **Step 2: Add the validation call**

Insert at that position:

```rust
        // Validate required config presence across all registered blocks.
        // Runs after composite/uses expansion so all configs are final.
        {
            let empty_cfg = serde_json::Value::Object(Default::default());
            let owned: Vec<(wafer_block::types::BlockInfo, serde_json::Value)> = self
                .blocks
                .iter()
                .map(|(name, block)| {
                    let cfg = self
                        .block_configs
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| empty_cfg.clone());
                    (block.info(), cfg)
                })
                .collect();
            let borrowed: Vec<(wafer_block::types::BlockInfo, &serde_json::Value)> = owned
                .iter()
                .map(|(info, cfg)| (info.clone(), cfg))
                .collect();
            let missing = super::validation::collect_missing_config(&borrowed);
            if !missing.is_empty() {
                return Err(crate::error::RuntimeError::Config(
                    super::validation::format_missing_config(&missing),
                ));
            }
        }
```

(The cloning + borrow dance avoids extending the lifetime of `self.block_configs` across the validator call. Keep the `Value::Object(Default::default())` fallback: a block with no registered config is still checked — its required keys will all be reported as missing.)

- [ ] **Step 3: Run wafer-run tests**

Run: `cargo test -p wafer-run`
Expected: pass. If any integration test registers a block with required config and no value, update that test to supply the value. Do **not** weaken the validator.

- [ ] **Step 4: Run the full workspace**

Run: `cargo test --workspace`
Expected: pass.

- [ ] **Step 5: Verify solobase still starts**

Same as Task 6 Step 3 — solobase must reach "ready" state. If it fails, either:
- the audit missed a key (add it to the audit, fix, retry), or
- solobase's deployment config is genuinely missing a required value (document and fix on the solobase side).

- [ ] **Step 6: Commit**

```bash
git add crates/wafer-run/src/runtime/resolver.rs
git commit -m "feat(wafer-run): validate required config presence during resolve()"
```

---

## Task 10: Add warn-once state to `Wafer` and `RuntimeContext`

**Files:**
- Modify: `crates/wafer-run/src/runtime.rs`
- Modify: `crates/wafer-run/src/context.rs`

This task adds the field but does not yet enforce action checks — keeps diffs small.

- [ ] **Step 1: Extend `Wafer`**

In `crates/wafer-run/src/runtime.rs`, inside the `pub struct Wafer` block, alongside `interface_specs_snapshot` (around line 166), add:

```rust
    /// Block names that have already produced an "unknown interface" warning.
    /// Process-local; used by the call_block interface-action validator to
    /// emit the warning at most once per block.
    pub(crate) warned_unknown_interfaces: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
```

In `impl Wafer::new()` (around line 178), alongside `interface_specs_snapshot: Arc::new(Vec::new())`, add:

```rust
            warned_unknown_interfaces: Arc::new(std::sync::Mutex::new(Default::default())),
```

- [ ] **Step 2: Propagate to `RuntimeContext`**

In `crates/wafer-run/src/context.rs`, inside the `pub struct RuntimeContext` block, alongside `interface_specs_snapshot` (around line 39), add:

```rust
    /// Warn-once tracking for unknown interfaces. Shared Arc with the Wafer.
    pub warned_unknown_interfaces: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
```

In `Wafer::make_context` (runtime.rs, around line 239), alongside `interface_specs_snapshot: self.interface_specs_snapshot.clone()`, add:

```rust
            warned_unknown_interfaces: self.warned_unknown_interfaces.clone(),
```

In the `sub_ctx` construction inside `RuntimeContext::call_block` (context.rs, around line 213), alongside `interface_specs_snapshot: self.interface_specs_snapshot.clone()`, add:

```rust
            warned_unknown_interfaces: self.warned_unknown_interfaces.clone(),
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check --workspace`
Expected: clean. All `RuntimeContext` constructions are now through `make_context` or `sub_ctx`, so adding the field should require no other changes. If a test constructs `RuntimeContext` directly, update it to include the new field (set to `Arc::new(Mutex::new(HashSet::new()))`).

- [ ] **Step 4: Run tests**

Run: `cargo test --workspace`
Expected: no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-run/src/runtime.rs crates/wafer-run/src/context.rs
git commit -m "feat(wafer-run): thread warn-once state through Wafer and RuntimeContext"
```

---

## Task 11: Wire `check_action_interface` into `RuntimeContext::call_block`

**Files:**
- Modify: `crates/wafer-run/src/context.rs`

- [ ] **Step 1: Add the validation call**

Open `crates/wafer-run/src/context.rs`. Find the section after block resolution — the `let block = match ... { Some(b) => b.clone(), None => { return err_output(NOT_FOUND, ...); } };` (around line 200). **After** that `let block = ...;` line and **before** the `// Derive the called block's requires for its own sub-context` comment (around line 203), insert:

```rust
        // Interface action validation: verify the message action is part of the
        // target block's declared interface. Skipped for action-agnostic
        // interfaces (empty action map) and for interfaces the runtime does
        // not recognize (warn-once, then proceed).
        {
            let info = block.info();
            let action = msg.action();
            match crate::runtime::validation::check_action_interface(
                &info.name,
                &info.interface,
                action,
                &self.interface_specs_snapshot,
            ) {
                crate::runtime::validation::ActionCheck::Valid => {}
                crate::runtime::validation::ActionCheck::Invalid { message } => {
                    return err_output(ErrorCode::INVALID_ARGUMENT, message);
                }
                crate::runtime::validation::ActionCheck::UnknownInterface => {
                    crate::runtime::validation::warn_once_unknown_interface(
                        &self.warned_unknown_interfaces,
                        &info.name,
                        &info.interface,
                    );
                }
            }
        }
```

Notes:
- `msg.action()` returns `&str` per `Message` API — confirm by `rg 'pub fn action' crates/wafer-block/src/types.rs`. If it returns a different type, adapt the call to produce a `&str`.
- `ErrorCode::INVALID_ARGUMENT` matches the constant used elsewhere in this file. If the constant has a different spelling (e.g., `InvalidArgument`), use the one in use.

- [ ] **Step 2: Run wafer-run tests**

Run: `cargo test -p wafer-run`
Expected: pass. If any existing test calls a block with an action not in its declared interface, the call will now fail. Fix the test or the interface spec — whichever reflects correct intent.

- [ ] **Step 3: Run the full workspace**

Run: `cargo test --workspace`
Expected: pass.

- [ ] **Step 4: Verify solobase still works**

Run solobase's normal startup path and exercise a representative HTTP endpoint (e.g., its login or health check).
Expected: no `INVALID_ARGUMENT` errors from dispatch.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-run/src/context.rs
git commit -m "feat(wafer-run): validate action against interface in call_block"
```

---

## Task 12: Integration tests

**Files:**
- Create: `crates/wafer-run/tests/validation_test.rs`

- [ ] **Step 1: Write the integration tests**

Create `crates/wafer-run/tests/validation_test.rs` with this content:

```rust
//! Integration tests for runtime validation: required-config presence at
//! start(), and action-vs-interface at call_block() dispatch.

use std::sync::Arc;
use wafer_block::{
    async_trait::async_trait,
    streams::{input::InputStream, output::OutputStream},
    types::{BlockInfo, ConfigVar, InterfaceSpec, LifecycleEvent, Message},
    Context,
};
use wafer_run::{Block, Wafer};

struct MinimalBlock {
    info: BlockInfo,
}

#[async_trait]
impl Block for MinimalBlock {
    fn info(&self) -> BlockInfo {
        self.info.clone()
    }
    async fn handle(
        &self,
        _ctx: &dyn Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::complete()
    }
    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn mk_block(name: &str, interface: &str, cfg_keys: Vec<ConfigVar>) -> Arc<MinimalBlock> {
    let mut info = BlockInfo::new(name, "0.1.0", interface, "test block");
    info.config_keys = cfg_keys;
    Arc::new(MinimalBlock { info })
}

#[tokio::test]
async fn start_fails_on_missing_required_config() {
    let mut w = Wafer::new();
    w.register_block(
        "test/needs-config".into(),
        mk_block(
            "test/needs-config",
            "database@v1",
            vec![ConfigVar::new("TEST__NEEDS_CONFIG__DB_URL", "required", "")],
        ),
    )
    .expect("register");

    let err = w.start().await.expect_err("start should fail");
    let msg = format!("{err}");
    assert!(msg.contains("test/needs-config"), "msg: {msg}");
    assert!(msg.contains("TEST__NEEDS_CONFIG__DB_URL"), "msg: {msg}");
}

#[tokio::test]
async fn start_succeeds_when_all_required_present() {
    let mut w = Wafer::new();
    w.register_block(
        "test/ok".into(),
        mk_block(
            "test/ok",
            "database@v1",
            vec![ConfigVar::new("TEST__OK__K", "required", "")],
        ),
    )
    .expect("register");
    w.add_block_config(
        "test/ok",
        serde_json::json!({ "TEST__OK__K": "value" }),
    );

    w.start().await.expect("start should succeed");
}

#[tokio::test]
async fn call_block_rejects_wrong_action_for_interface() {
    let mut w = Wafer::new();
    w.register_block(
        "test/dbish".into(),
        mk_block("test/dbish", "database@v1", vec![]),
    )
    .expect("register");
    let started = w.start().await.expect("start");

    // Build a minimal message whose action is NOT in database@v1's action map.
    let msg = Message::new("publish", "");
    let stream = started.run_block("test/dbish", msg, InputStream::empty()).await;
    // Collect the stream into a terminal event. run_block returns OutputStream;
    // inspect its final event for an error with code INVALID_ARGUMENT.
    let terminal = wafer_run::test_support::drain_for_terminal(stream).await;
    let err = terminal.expect_error();
    assert_eq!(err.code, wafer_block::types::ErrorCode::INVALID_ARGUMENT);
    assert!(err.message.contains("test/dbish"), "msg: {}", err.message);
    assert!(err.message.contains("publish"), "msg: {}", err.message);
}

#[tokio::test]
async fn call_block_allows_custom_interface_with_warning() {
    let mut w = Wafer::new();
    w.register_block(
        "test/custom".into(),
        mk_block("test/custom", "my-org/custom@v1", vec![]),
    )
    .expect("register");
    let started = w.start().await.expect("start");

    let msg = Message::new("anything", "");
    let stream = started.run_block("test/custom", msg, InputStream::empty()).await;
    let terminal = wafer_run::test_support::drain_for_terminal(stream).await;
    // Custom interface → unknown, warn-once, proceed. Block returns Complete.
    terminal.expect_complete();
}
```

Two test utilities referenced above may not yet exist:
- `Message::new(action, data)` — check `crates/wafer-block/src/types.rs` for the correct constructor; if it takes different args, adapt the call.
- `wafer_run::test_support::drain_for_terminal` and the `expect_error`/`expect_complete` helpers — if no such module exists, add a thin `test_support` module exported behind `#[cfg(any(test, feature = "test-support"))]` in `crates/wafer-run/src/lib.rs` that:
  - drains an `OutputStream` into its terminal `StreamEvent`, and
  - provides `expect_error()` / `expect_complete()` shortcuts.

Keep that helper in its own commit if you add it (`feat(wafer-run): test_support drain_for_terminal helper`).

- [ ] **Step 2: Run the new tests**

Run: `cargo test -p wafer-run --test validation_test`
Expected: all four pass.

- [ ] **Step 3: Run the full workspace**

Run: `cargo test --workspace`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-run/tests/validation_test.rs
# If test_support was added:
git add crates/wafer-run/src/
git commit -m "test(wafer-run): integration tests for config + interface validation"
```

---

## Task 13: Document the new behaviors

**Files:**
- Modify: `crates/wafer-run/src/runtime/lifecycle.rs` (rustdoc on `start`)
- Modify: `crates/wafer-run/src/context.rs` (rustdoc on `call_block`)

- [ ] **Step 1: Document `start()`**

Find `pub async fn start(mut self)` in `crates/wafer-run/src/runtime/lifecycle.rs` (around line 85). Replace its doc comment with:

```rust
    /// Start the runtime, wrap in `Arc`, and call `bind()` on all blocks.
    ///
    /// # Validation
    ///
    /// Before any block lifecycle event is dispatched, runs:
    /// - **Config presence**: every registered block's declared `config_keys`
    ///   that have no default and no `auto_generate` must be provided, or
    ///   `start()` returns `RuntimeError::Config` with all missing
    ///   `(block, key)` pairs aggregated into a single message. Lifecycle
    ///   events are not dispatched on failure.
    ///
    /// See `crates/wafer-run/src/runtime/validation.rs`.
```

- [ ] **Step 2: Document `call_block`**

Find the `impl Context for RuntimeContext` block's `call_block` method in `crates/wafer-run/src/context.rs` (around line 72). Add a rustdoc comment above the method signature:

```rust
    /// Dispatch a message to another registered block.
    ///
    /// # Checks
    ///
    /// Runs in order, returning an error event on failure:
    /// 1. Call-depth limit (default 16).
    /// 2. Cancellation / deadline.
    /// 3. Caller `requires` allowlist.
    /// 4. WRAP resource access (`META_WRAP_RESOURCE`).
    /// 5. Caller capability check (WASM capability model).
    /// 6. **Interface action**: `msg.action()` must be in the target block's
    ///    declared interface action map, unless the interface is
    ///    action-agnostic (empty map) or unknown to the runtime. Unknown
    ///    interfaces produce a one-time `WARN` log per block.
    ///
    /// See `crates/wafer-run/src/runtime/validation.rs`.
```

- [ ] **Step 3: Run doc tests**

Run: `cargo test --doc -p wafer-run`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-run/src/runtime/lifecycle.rs crates/wafer-run/src/context.rs
git commit -m "docs(wafer-run): document validation steps on start and call_block"
```

---

## Task 14: Regression gate

No code changes — verification only.

- [ ] **Step 1: Full workspace tests**

Run: `cd /home/joris/Programs/suppers-ai/workspace/wafer-run && cargo test --workspace`
Expected: all pass.

- [ ] **Step 2: Streaming e2e suite**

Run: `cargo test -p wafer-run --test streaming_e2e`
Expected: pass (unchanged by this work).

- [ ] **Step 3: WRAP tests**

Run: `cargo test -p wafer-block wrap`
Expected: the 15 WRAP tests in `crates/wafer-block/src/wrap.rs:232-683` all pass.

- [ ] **Step 4: Solobase startup + smoke**

In the sibling solobase repo, run its normal startup command plus a representative request against the running process. Expected: startup succeeds, the request succeeds.

- [ ] **Step 5: Cargo clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no new warnings.

If any step fails, stop and fix the root cause before declaring the plan complete. Do not paper over failures.
