# Block capabilities implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship guest-declared block capabilities with operator narrowing via config, and replace `sanitize_guest_meta`'s unconditional substring strip with a `HeaderPolicy` (readable/writable allowlists + masked denylist) enforced bidirectionally on WASM blocks.

**Architecture:** Data model lives in `wafer-block` (`HeaderPolicy`, `BlockCapabilities::headers`, `BlockCapabilities::intersect`, `BlockInfo::capabilities`). Enforcement lives in `wafer-run/src/wasm/wasmi_loader.rs` (inbound + outbound meta sanitizers using exact header-name matching) and `wafer-run/src/runtime/resolver.rs` (config parsing + intersection at load time). Ergonomic declaration via an extended `#[wafer_block]` macro accepting `capabilities(...)`. Native blocks may declare caps (stored for docs/inspector) but the runtime does not enforce — documented explicitly.

**Tech Stack:** Rust, Cargo workspaces, `syn`/`quote` for proc macro, `serde_json` for config parsing, `wafer-test-support` (from Spec 2A) for integration tests.

**Spec:** `docs/specs/2026-04-18-block-capabilities-design.md`

---

## Task 1: `HeaderPolicy` struct + `headers` field on `BlockCapabilities`

**Files:**
- Modify: `crates/wafer-block/src/capabilities.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/wafer-block/src/capabilities.rs` (create a `#[cfg(test)] mod tests` at end of file if absent; otherwise add to the existing one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_policy_defaults_empty() {
        let p = HeaderPolicy::default();
        assert!(p.readable.is_empty());
        assert!(p.writable.is_empty());
        assert!(p.masked.is_empty());
    }

    #[test]
    fn block_capabilities_default_has_empty_header_policy() {
        let caps = BlockCapabilities::default();
        assert!(caps.headers.readable.is_empty());
        assert!(caps.headers.writable.is_empty());
        assert!(caps.headers.masked.is_empty());
    }

    #[test]
    fn header_policy_roundtrips_through_json() {
        let p = HeaderPolicy {
            readable: vec!["authorization".into()],
            writable: vec!["set-cookie".into()],
            masked: vec!["x-internal".into()],
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: HeaderPolicy = serde_json::from_str(&j).unwrap();
        assert_eq!(back.readable, p.readable);
        assert_eq!(back.writable, p.writable);
        assert_eq!(back.masked, p.masked);
    }
}
```

- [ ] **Step 2: Run test — expect fail**

Run: `cd /home/joris/Programs/suppers-ai/workspace/wafer-run-capabilities && cargo test -p wafer-block capabilities`
Expected: FAIL (`HeaderPolicy` not defined, `BlockCapabilities::default()` may not compile, `headers` field missing).

- [ ] **Step 3: Add `HeaderPolicy` struct and `headers` field**

Edit `crates/wafer-block/src/capabilities.rs`. Near the top, before `pub struct BlockCapabilities`, add:

```rust
/// Policy for which headers a block may read, write, or which should be masked.
///
/// Applied by the runtime only to WASM blocks. For native blocks, this is
/// documentation / inspector metadata only — enforcement is WASM-specific.
///
/// Default-denied sensitive header set (see
/// `wafer_run::wasm::wasmi_loader::default_sensitive_headers`) is masked
/// unless explicitly listed in `readable` (for inbound) or `writable`
/// (for outbound). `masked` adds extra headers to the deny set in both
/// directions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeaderPolicy {
    /// Sensitive inbound headers the block may READ.
    /// Example: `["authorization"]`.
    #[serde(default)]
    pub readable: Vec<String>,

    /// Sensitive outbound headers the block may WRITE.
    /// Example: `["set-cookie"]`.
    #[serde(default)]
    pub writable: Vec<String>,

    /// Additional headers to mask beyond the default sensitive set.
    /// Applies to both directions. Operator extension for app-specific
    /// sensitive headers.
    /// Example: `["x-internal-token"]`.
    #[serde(default)]
    pub masked: Vec<String>,
}
```

Add the `headers` field to `BlockCapabilities`:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlockCapabilities {
    // ... existing fields ...
    /// Per-header read/write/mask policy.
    #[serde(default)]
    pub headers: HeaderPolicy,
}
```

Add `Default` derive to `BlockCapabilities` if not already present (check existing derives first).

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p wafer-block capabilities`
Expected: 3 new tests pass. Existing `BlockCapabilities` tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/capabilities.rs
git commit -m "feat(wafer-block): add HeaderPolicy to BlockCapabilities"
```

---

## Task 2: `BlockCapabilities::intersect`

**Files:**
- Modify: `crates/wafer-block/src/capabilities.rs`

- [ ] **Step 1: Write failing tests**

Append inside the existing `tests` module in `capabilities.rs`:

```rust
    use std::collections::HashSet;

    fn caps_with_collections(items: &[&str]) -> BlockCapabilities {
        let mut c = BlockCapabilities::default();
        c.collections = items.iter().map(|s| s.to_string()).collect();
        c
    }

    #[test]
    fn intersect_booleans_and() {
        let a = BlockCapabilities {
            crypto: true,
            network: true,
            raw_sql: false,
            ..Default::default()
        };
        let b = BlockCapabilities {
            crypto: true,
            network: false,
            raw_sql: true,
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert!(r.crypto);
        assert!(!r.network);
        assert!(!r.raw_sql);
    }

    #[test]
    fn intersect_collections_set_intersection() {
        let a = caps_with_collections(&["a", "b", "c"]);
        let b = caps_with_collections(&["b", "c", "d"]);
        let r = a.intersect(&b);
        let expected: HashSet<String> = ["b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(r.collections, expected);
    }

    #[test]
    fn intersect_wildcard_sentinel_left_yields_right() {
        let a = caps_with_collections(&["*"]);
        let b = caps_with_collections(&["users"]);
        let r = a.intersect(&b);
        let expected: HashSet<String> = ["users"].iter().map(|s| s.to_string()).collect();
        assert_eq!(r.collections, expected);
    }

    #[test]
    fn intersect_wildcard_sentinel_both_yields_wildcard() {
        let a = caps_with_collections(&["*"]);
        let b = caps_with_collections(&["*"]);
        let r = a.intersect(&b);
        let expected: HashSet<String> = ["*"].iter().map(|s| s.to_string()).collect();
        assert_eq!(r.collections, expected);
    }

    #[test]
    fn intersect_network_allow_vec_intersection() {
        let a = BlockCapabilities {
            network_allow: vec!["https://a.com/".into(), "https://b.com/".into()],
            ..Default::default()
        };
        let b = BlockCapabilities {
            network_allow: vec!["https://b.com/".into(), "https://c.com/".into()],
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert_eq!(r.network_allow, vec!["https://b.com/".to_string()]);
    }

    #[test]
    fn intersect_header_policy_readable_intersects() {
        let a = BlockCapabilities {
            headers: HeaderPolicy {
                readable: vec!["authorization".into(), "cookie".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let b = BlockCapabilities {
            headers: HeaderPolicy {
                readable: vec!["cookie".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let r = a.intersect(&b);
        assert_eq!(r.headers.readable, vec!["cookie".to_string()]);
    }

    #[test]
    fn intersect_header_policy_masked_unions() {
        let a = BlockCapabilities {
            headers: HeaderPolicy {
                masked: vec!["x-a".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let b = BlockCapabilities {
            headers: HeaderPolicy {
                masked: vec!["x-b".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let r = a.intersect(&b);
        let mut got = r.headers.masked.clone();
        got.sort();
        assert_eq!(got, vec!["x-a".to_string(), "x-b".to_string()]);
    }
```

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p wafer-block intersect`
Expected: FAIL — `intersect` method not defined.

- [ ] **Step 3: Implement `intersect`**

Append to the `impl BlockCapabilities` block (the existing one with `unrestricted()`, `none()`, `allows_*` methods) in `capabilities.rs`:

```rust
    /// Intersect two capability sets.
    ///
    /// Rules:
    /// - Booleans: logical AND (both must allow).
    /// - HashSet allowlists (collections, storage_folders, config_keys, callable_blocks):
    ///   set intersection. Wildcard sentinel `"*"` on one side yields the other side.
    /// - Vec allowlist (network_allow): set intersection, order not preserved.
    /// - HeaderPolicy readable / writable: intersection.
    /// - HeaderPolicy masked: UNION (denylists strengthen).
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            collections: intersect_wildcard_set(&self.collections, &other.collections),
            raw_sql: self.raw_sql && other.raw_sql,
            storage_folders: intersect_wildcard_set(&self.storage_folders, &other.storage_folders),
            crypto: self.crypto && other.crypto,
            network: self.network && other.network,
            network_allow: intersect_vec(&self.network_allow, &other.network_allow),
            config: self.config && other.config,
            config_keys: intersect_wildcard_set(&self.config_keys, &other.config_keys),
            callable_blocks: intersect_wildcard_set(&self.callable_blocks, &other.callable_blocks),
            headers: HeaderPolicy {
                readable: intersect_vec(&self.headers.readable, &other.headers.readable),
                writable: intersect_vec(&self.headers.writable, &other.headers.writable),
                masked: union_vec(&self.headers.masked, &other.headers.masked),
            },
        }
    }
```

Add these private helpers at the end of the file (above the `#[cfg(test)]` block):

```rust
fn intersect_wildcard_set(a: &HashSet<String>, b: &HashSet<String>) -> HashSet<String> {
    let a_any = a.contains("*");
    let b_any = b.contains("*");
    match (a_any, b_any) {
        (true, true) => {
            let mut r: HashSet<String> = HashSet::new();
            r.insert("*".into());
            r
        }
        (true, false) => b.clone(),
        (false, true) => a.clone(),
        (false, false) => a.intersection(b).cloned().collect(),
    }
}

fn intersect_vec(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| b.iter().any(|y| y == *x)).cloned().collect()
}

fn union_vec(a: &[String], b: &[String]) -> Vec<String> {
    let mut r: Vec<String> = a.to_vec();
    for v in b {
        if !r.iter().any(|x| x == v) {
            r.push(v.clone());
        }
    }
    r
}
```

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p wafer-block`
Expected: all existing + 7 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/capabilities.rs
git commit -m "feat(wafer-block): BlockCapabilities::intersect with header policy semantics"
```

---

## Task 3: `BlockInfo::capabilities` field

**Files:**
- Modify: `crates/wafer-block/src/types.rs`

- [ ] **Step 1: Write failing test**

Append to the existing `#[cfg(test)] mod tests` block in `crates/wafer-block/src/types.rs` (search for `mod tests` near the bottom of that file):

```rust
    #[test]
    fn block_info_capabilities_default_none() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary");
        assert!(info.capabilities.is_none());
    }

    #[test]
    fn block_info_capabilities_builder_sets_some() {
        let mut caps = crate::BlockCapabilities::default();
        caps.crypto = true;
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary")
            .capabilities(caps.clone());
        assert!(info.capabilities.is_some());
        assert!(info.capabilities.as_ref().unwrap().crypto);
    }

    #[test]
    fn block_info_capabilities_roundtrip_json() {
        let mut caps = crate::BlockCapabilities::default();
        caps.crypto = true;
        caps.headers.writable = vec!["set-cookie".into()];
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary")
            .capabilities(caps);
        let j = serde_json::to_string(&info).unwrap();
        let back: BlockInfo = serde_json::from_str(&j).unwrap();
        let caps_back = back.capabilities.expect("caps present");
        assert!(caps_back.crypto);
        assert_eq!(caps_back.headers.writable, vec!["set-cookie".to_string()]);
    }

    #[test]
    fn block_info_without_capabilities_serializes_without_key() {
        let info = BlockInfo::new("org/b", "0.1.0", "iface@v1", "summary");
        let j = serde_json::to_string(&info).unwrap();
        assert!(!j.contains("\"capabilities\""), "json should omit the field when None: {j}");
    }
```

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p wafer-block block_info_capabilities`
Expected: FAIL — `capabilities` field and builder method missing.

- [ ] **Step 3: Add field and builder**

In `crates/wafer-block/src/types.rs`, inside `pub struct BlockInfo { ... }`, add after the existing fields (and before the closing `}`):

```rust
    /// Capability declaration.
    ///
    /// For WASM blocks: carried in the JSON returned by `__wafer_info` and
    /// intersected with operator config at load time. Enforced at dispatch.
    ///
    /// For native blocks: documentation and inspector metadata only. Not
    /// enforced by the runtime. Native blocks continue to operate under
    /// the existing trust model.
    ///
    /// `None` means the block did not declare — the runtime applies the
    /// existing default for that block's runtime type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::BlockCapabilities>,
```

In `impl BlockInfo { ... }`, add after the existing builder methods:

```rust
    pub fn capabilities(mut self, caps: crate::BlockCapabilities) -> Self {
        self.capabilities = Some(caps);
        self
    }
```

If `BlockInfo::new` fully initializes fields (check for the `Self { name: ..., version: ..., ... }` struct literal), add `capabilities: None,` to the initializer. Likewise for `impl Default for BlockInfo`.

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p wafer-block`
Expected: 4 new tests pass. Workspace-wide run shows no regressions: `cargo test --workspace`.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block/src/types.rs
git commit -m "feat(wafer-block): BlockInfo::capabilities field and builder"
```

---

## Task 4: `header_name_from_meta_key` helper

**Files:**
- Modify: `crates/wafer-run/src/wasm/wasmi_loader.rs`

This helper parses a wafer meta key into the underlying HTTP header name, for use in sanitization (Task 5).

- [ ] **Step 1: Write failing tests**

In `crates/wafer-run/src/wasm/wasmi_loader.rs`, find the existing `fn sanitize_guest_meta` (around line 47). Below it, add a new inline test module just for the helper (don't worry about integrating with any existing test module in the file):

```rust
#[cfg(test)]
mod header_name_tests {
    use super::header_name_from_meta_key;

    #[test]
    fn req_header_prefix() {
        assert_eq!(
            header_name_from_meta_key("req.header.authorization"),
            Some("authorization".to_string())
        );
    }

    #[test]
    fn req_header_uppercase_lowercased() {
        assert_eq!(
            header_name_from_meta_key("req.header.Authorization"),
            Some("authorization".to_string())
        );
    }

    #[test]
    fn resp_header_prefix() {
        assert_eq!(
            header_name_from_meta_key("resp.header.x-custom"),
            Some("x-custom".to_string())
        );
    }

    #[test]
    fn legacy_resp_set_cookie_bare() {
        assert_eq!(
            header_name_from_meta_key("resp.set_cookie"),
            Some("set-cookie".to_string())
        );
    }

    #[test]
    fn legacy_resp_set_cookie_nested() {
        assert_eq!(
            header_name_from_meta_key("resp.set_cookie.session"),
            Some("set-cookie".to_string())
        );
    }

    #[test]
    fn internal_meta_key_is_none() {
        assert_eq!(header_name_from_meta_key("auth.user_id"), None);
        assert_eq!(header_name_from_meta_key("trace_id"), None);
        assert_eq!(header_name_from_meta_key(""), None);
    }
}
```

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p wafer-run header_name`
Expected: FAIL — function not defined.

- [ ] **Step 3: Add the helper**

In the same file, above the existing `fn sanitize_guest_meta`, add:

```rust
/// Extract the canonical (lowercase) HTTP header name from a wafer meta key,
/// or `None` if the key is not a header.
///
/// Matches three forms:
/// - `req.header.{name}` — inbound request header
/// - `resp.header.{name}` — outbound response header
/// - `resp.set_cookie` / `resp.set_cookie.*` — legacy cookie keys, mapped to `set-cookie`
pub(crate) fn header_name_from_meta_key(key: &str) -> Option<String> {
    let lower = key.to_lowercase();
    if let Some(rest) = lower.strip_prefix("req.header.") {
        return Some(rest.to_string());
    }
    if let Some(rest) = lower.strip_prefix("resp.header.") {
        return Some(rest.to_string());
    }
    if lower == "resp.set_cookie" || lower.starts_with("resp.set_cookie.") {
        return Some("set-cookie".to_string());
    }
    None
}
```

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p wafer-run header_name`
Expected: 6 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-run/src/wasm/wasmi_loader.rs
git commit -m "feat(wafer-run): header_name_from_meta_key helper"
```

---

## Task 5: Replace `sanitize_guest_meta` with policy-driven inbound + outbound sanitizers

**Files:**
- Modify: `crates/wafer-run/src/wasm/wasmi_loader.rs`

- [ ] **Step 1: Add the default sensitive header list**

Near the top of `crates/wafer-run/src/wasm/wasmi_loader.rs`, above `header_name_from_meta_key`, add:

```rust
/// Default set of HTTP header names that are considered security-sensitive.
/// WASM blocks cannot read or write these unless they declare them in their
/// `HeaderPolicy.readable` / `HeaderPolicy.writable` (and are granted the
/// cap after config intersection).
pub(crate) fn default_sensitive_headers() -> &'static [&'static str] {
    &[
        "authorization",
        "cookie",
        "set-cookie",
        "location",
        "access-control-allow-origin",
        "access-control-allow-credentials",
        "access-control-allow-methods",
        "access-control-allow-headers",
        "access-control-expose-headers",
        "access-control-max-age",
        "strict-transport-security",
        "x-frame-options",
        "content-security-policy",
        "content-security-policy-report-only",
    ]
}

fn is_sensitive_header(name: &str, policy_masked: &[String]) -> bool {
    let n = name.to_lowercase();
    default_sensitive_headers().iter().any(|h| *h == n)
        || policy_masked.iter().any(|m| m.eq_ignore_ascii_case(&n))
}
```

- [ ] **Step 2: Write failing tests for the sanitizers**

Append below the existing `header_name_tests` module:

```rust
#[cfg(test)]
mod sanitize_tests {
    use super::*;
    use wafer_block::types::MetaEntry;
    use wafer_block::capabilities::{BlockCapabilities, HeaderPolicy};

    fn meta(key: &str, value: &str) -> MetaEntry {
        MetaEntry {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn outbound_strips_default_sensitive_when_empty_policy() {
        let caps = BlockCapabilities::default();
        let input = vec![
            meta("resp.header.content-type", "text/plain"),
            meta("resp.header.set-cookie", "s=1"),
            meta("resp.set_cookie", "legacy"),
            meta("resp.header.x-frame-options", "DENY"),
            meta("resp.header.x-safe", "ok"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_outbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"resp.header.content-type"));
        assert!(keys.contains(&"resp.header.x-safe"));
        assert!(!keys.iter().any(|k| k.contains("set-cookie") || k.contains("set_cookie")));
        assert!(!keys.iter().any(|k| k.contains("x-frame-options")));
        assert!(stripped.contains(&"set-cookie".to_string()));
    }

    #[test]
    fn outbound_writable_allows_named_header() {
        let mut caps = BlockCapabilities::default();
        caps.headers.writable = vec!["set-cookie".into()];
        let input = vec![
            meta("resp.header.set-cookie", "s=1"),
            meta("resp.set_cookie", "legacy"),
            meta("resp.header.x-frame-options", "DENY"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_outbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"resp.header.set-cookie"));
        assert!(keys.contains(&"resp.set_cookie"));
        assert!(!keys.iter().any(|k| k.contains("x-frame-options")));
    }

    #[test]
    fn inbound_strips_default_sensitive_when_empty_policy() {
        let caps = BlockCapabilities::default();
        let input = vec![
            meta("req.header.accept", "text/plain"),
            meta("req.header.authorization", "Bearer abc"),
            meta("req.header.cookie", "a=1"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_inbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"req.header.accept"));
        assert!(!keys.iter().any(|k| k.contains("authorization")));
        assert!(!keys.iter().any(|k| k.contains("cookie")));
    }

    #[test]
    fn inbound_readable_allows_named_header() {
        let mut caps = BlockCapabilities::default();
        caps.headers.readable = vec!["authorization".into()];
        let input = vec![
            meta("req.header.authorization", "Bearer abc"),
            meta("req.header.cookie", "a=1"),
        ];
        let mut stripped = Vec::new();
        let out = sanitize_inbound_meta(input, &caps, &mut stripped);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"req.header.authorization"));
        assert!(!keys.iter().any(|k| k.contains("cookie")));
    }

    #[test]
    fn masked_extends_default_sensitive_both_directions() {
        let mut caps = BlockCapabilities::default();
        caps.headers.masked = vec!["x-internal".into()];
        let inbound = vec![meta("req.header.x-internal", "secret")];
        let outbound = vec![meta("resp.header.x-internal", "secret")];
        let mut s1 = Vec::new();
        let mut s2 = Vec::new();
        assert!(sanitize_inbound_meta(inbound, &caps, &mut s1).is_empty());
        assert!(sanitize_outbound_meta(outbound, &caps, &mut s2).is_empty());
    }

    #[test]
    fn non_header_keys_pass_through() {
        let caps = BlockCapabilities::default();
        let input = vec![
            meta("auth.user_id", "u1"),
            meta("trace_id", "abc"),
        ];
        let mut s = Vec::new();
        let out = sanitize_outbound_meta(input.clone(), &caps, &mut s);
        let keys: Vec<&str> = out.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"auth.user_id"));
        assert!(keys.contains(&"trace_id"));
    }
}
```

- [ ] **Step 3: Run — expect fail**

Run: `cargo test -p wafer-run sanitize_tests`
Expected: FAIL — `sanitize_outbound_meta`, `sanitize_inbound_meta` not defined.

- [ ] **Step 4: Add the new sanitizers**

In the same file, **REPLACE** the existing `fn sanitize_guest_meta(meta: Vec<MetaEntry>) -> Vec<MetaEntry>` with these two:

```rust
/// Strip outbound meta entries whose header name is in the default sensitive
/// set plus the block's `HeaderPolicy.masked`, unless explicitly in
/// `HeaderPolicy.writable`. Non-header meta entries pass through.
///
/// Stripped header names (deduped, lowercased) are appended to `stripped_names`
/// so the caller can issue a warn-once log.
pub(crate) fn sanitize_outbound_meta(
    meta: Vec<wafer_block::types::MetaEntry>,
    caps: &wafer_block::capabilities::BlockCapabilities,
    stripped_names: &mut Vec<String>,
) -> Vec<wafer_block::types::MetaEntry> {
    meta.into_iter()
        .filter(|e| {
            let Some(name) = header_name_from_meta_key(&e.key) else {
                return true; // Not a header — pass through.
            };
            if !is_sensitive_header(&name, &caps.headers.masked) {
                return true;
            }
            let allowed = caps.headers.writable.iter().any(|w| w.eq_ignore_ascii_case(&name));
            if !allowed {
                if !stripped_names.iter().any(|n| n == &name) {
                    stripped_names.push(name);
                }
                return false;
            }
            true
        })
        .collect()
}

/// Symmetric inbound sanitizer. Uses `HeaderPolicy.readable` as the allowlist.
pub(crate) fn sanitize_inbound_meta(
    meta: Vec<wafer_block::types::MetaEntry>,
    caps: &wafer_block::capabilities::BlockCapabilities,
    stripped_names: &mut Vec<String>,
) -> Vec<wafer_block::types::MetaEntry> {
    meta.into_iter()
        .filter(|e| {
            let Some(name) = header_name_from_meta_key(&e.key) else {
                return true;
            };
            if !is_sensitive_header(&name, &caps.headers.masked) {
                return true;
            }
            let allowed = caps.headers.readable.iter().any(|r| r.eq_ignore_ascii_case(&name));
            if !allowed {
                if !stripped_names.iter().any(|n| n == &name) {
                    stripped_names.push(name);
                }
                return false;
            }
            true
        })
        .collect()
}
```

- [ ] **Step 5: Wire the new outbound sanitizer at the call site**

Find the existing call `sanitize_guest_meta(r.meta)` (around line 828 per the earlier grep). It lives in `.map(|r| (r.data, sanitize_guest_meta(r.meta)))` inside the WASM call path. Replace with:

```rust
.map(|r| {
    let mut stripped: Vec<String> = Vec::new();
    let meta = sanitize_outbound_meta(r.meta, self.capabilities(), &mut stripped);
    if !stripped.is_empty() {
        self.warn_once_stripped_outbound(&stripped);
    }
    (r.data, meta)
})
```

where `self.capabilities()` returns a reference to the block's effective `BlockCapabilities` (already stored on `WasmiBlock` via `load_with_capabilities`). Look for the existing `capabilities` field in the `WasmiBlock` struct — it's there. Add an accessor if none exists:

```rust
impl WasmiBlock {
    pub(crate) fn capabilities(&self) -> &wafer_block::capabilities::BlockCapabilities {
        &self.capabilities
    }
}
```

(If the field is private, adapt; look for `WasmiBlock` struct definition in the file.)

- [ ] **Step 6: Add warn-once state and method**

On the `WasmiBlock` struct, add two flags:

```rust
use std::sync::atomic::{AtomicBool, Ordering};

pub struct WasmiBlock {
    // ... existing fields ...
    warned_outbound: AtomicBool,
    warned_inbound: AtomicBool,
}
```

Initialize both to `AtomicBool::new(false)` in every `WasmiBlock` constructor (probably inside `load_with_capabilities` and any other constructor — grep for `WasmiBlock {` instantiations).

Add methods:

```rust
impl WasmiBlock {
    fn warn_once_stripped_outbound(&self, names: &[String]) {
        if self.warned_outbound.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(
            block = %self.name(),
            direction = "outbound",
            stripped = ?names,
            "headers outside writable allowlist"
        );
    }

    fn warn_once_stripped_inbound(&self, names: &[String]) {
        if self.warned_inbound.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(
            block = %self.name(),
            direction = "inbound",
            stripped = ?names,
            "headers outside readable allowlist"
        );
    }
}
```

(If `self.name()` is not a method, use the existing field name — look at the struct definition; commonly `self.info.name` or similar.)

- [ ] **Step 7: Wire the inbound sanitizer**

Find where the WASM block receives the inbound `Message` — the point just before `__wafer_handle_message` or equivalent is invoked. Grep for `call_guest_resumable` or the method that accepts an inbound `Message` and enters the guest. Locate where `msg.meta` is passed to the guest.

Replace the existing `msg` with one whose meta has been sanitized:

```rust
let mut stripped_in: Vec<String> = Vec::new();
let msg_meta = sanitize_inbound_meta(msg.meta, self.capabilities(), &mut stripped_in);
if !stripped_in.is_empty() {
    self.warn_once_stripped_inbound(&stripped_in);
}
let msg = wafer_block::types::Message { meta: msg_meta, ..msg };
```

The exact location depends on the code structure — search for the `msg.meta` assignment or the call to the guest's `__wafer_handle_message` export. The test `inbound_readable_allows_named_header` is the functional check that this wiring works end-to-end.

- [ ] **Step 8: Run tests**

Run: `cargo test -p wafer-run`
Expected: all new sanitize tests pass; existing wasmi_block tests still pass. If any existing tests rely on the old unconditional strip behavior, they may need to provide an empty `BlockCapabilities` (which produces the same effective behavior).

- [ ] **Step 9: Commit**

```bash
git add crates/wafer-run/src/wasm/wasmi_loader.rs
git commit -m "feat(wafer-run): policy-driven header sanitization with warn-once logs"
```

---

## Task 6: Parse `capabilities` config subkey and compute effective caps at load

**Files:**
- Modify: `crates/wafer-run/src/runtime/resolver.rs`
- Possibly modify: `crates/wafer-run/src/wasm/wasmi_loader.rs` (to accept effective caps)

- [ ] **Step 1: Write the failing test**

Create `crates/wafer-run/tests/capabilities_config_test.rs`:

```rust
//! Test that the runtime parses the reserved `capabilities` subkey from
//! block config and intersects it with the block's declared caps.

use std::sync::Arc;
use serde_json::json;
use wafer_block::{
    capabilities::{BlockCapabilities, HeaderPolicy},
    streams::{input::InputStream, output::OutputStream},
    types::{BlockInfo, LifecycleEvent, Message},
    Block, Context, WaferError,
};
use wafer_run::Wafer;

/// A native block that declares caps via BlockInfo::capabilities.
/// Native blocks are not enforced, but we can still observe that the
/// runtime parses and stores the effective caps — visible via the
/// block's info, and the inspector.
struct DeclaringNative {
    info: BlockInfo,
}

#[async_trait::async_trait]
impl Block for DeclaringNative {
    fn info(&self) -> BlockInfo {
        self.info.clone()
    }
    async fn handle(&self, _: &dyn Context, _: Message, _: InputStream) -> OutputStream {
        OutputStream::respond(b"{}".to_vec())
    }
    async fn lifecycle(&self, _: &dyn Context, _: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}

#[tokio::test]
async fn config_capabilities_subkey_parsed_and_intersected() {
    let mut declared = BlockCapabilities::default();
    declared.collections = ["users", "sessions"].iter().map(|s| s.to_string()).collect();
    declared.network = true;
    declared.headers = HeaderPolicy {
        readable: vec!["authorization".into()],
        writable: vec!["set-cookie".into()],
        ..Default::default()
    };

    let info = BlockInfo::new("test/declaring", "0.1.0", "middleware@v1", "")
        .capabilities(declared.clone());
    let block = Arc::new(DeclaringNative { info });

    let mut w = Wafer::new();
    w.register_block("test/declaring", block).unwrap();
    w.add_block_config(
        "test/declaring",
        json!({
            "capabilities": {
                "collections": ["users"],   // narrows from {"users", "sessions"}
                "network": false,            // narrows from true
                "headers": {
                    "writable": []           // narrows, removing set-cookie
                }
            },
            "OTHER_KEY": "passthrough"
        }),
    );
    let wafer = w.start().await.expect("start");

    // The effective caps should be visible via the block info snapshot OR an
    // accessor on Wafer. Since the exact API surface depends on Spec 2B
    // implementation choices, this test accesses whichever is available:
    // `wafer.effective_capabilities("test/declaring")` if that method exists,
    // otherwise verify via the inspector info that capabilities were narrowed.
    let eff = wafer
        .effective_capabilities("test/declaring")
        .expect("effective caps stored for registered block");
    let expected_collections: std::collections::HashSet<String> =
        ["users"].iter().map(|s| s.to_string()).collect();
    assert_eq!(eff.collections, expected_collections);
    assert!(!eff.network);
    assert!(eff.headers.writable.is_empty());
    // The intersection narrower won over the block's own declared "set-cookie".
}
```

- [ ] **Step 2: Run — expect fail**

Run: `cargo test -p wafer-run --test capabilities_config_test`
Expected: FAIL — `Wafer::effective_capabilities` not defined; config `capabilities` subkey not parsed.

- [ ] **Step 3: Store effective caps on `Wafer` and add accessor**

In `crates/wafer-run/src/runtime.rs`, inside `pub struct Wafer { ... }`, add a field:

```rust
    /// Effective capabilities per block (after declared ∩ config ∩ host
    /// intersection). Computed at `resolve()` time.
    pub(crate) effective_capabilities: Arc<HashMap<String, wafer_block::BlockCapabilities>>,
```

Initialize in `Wafer::new()` to `Arc::new(HashMap::new())`.

Add a public accessor:

```rust
impl Wafer {
    /// Look up the effective (declared ∩ config ∩ host) capabilities for
    /// a registered block. Returns `None` if the block did not declare
    /// and no config/host caps were provided.
    pub fn effective_capabilities(&self, block_name: &str) -> Option<&wafer_block::BlockCapabilities> {
        self.effective_capabilities.get(block_name)
    }
}
```

- [ ] **Step 4: Parse the config subkey and compute intersection in `resolve()`**

Open `crates/wafer-run/src/runtime/resolver.rs`. Find `pub async fn resolve(&mut self)` and look for the section that iterates `self.block_configs`. (In Spec 1 we added a config-presence validator there — use the same injection region.)

Add this block **before** the snapshot line (`self.block_configs_snapshot = Arc::new(self.block_configs.clone());`):

```rust
        // Compute effective capabilities per block: declared ∩ config ∩ host.
        // Also strip the reserved `capabilities` subkey from the block config
        // so it doesn't leak into `ctx.config_get(...)`.
        {
            let mut eff: HashMap<String, wafer_block::BlockCapabilities> = HashMap::new();
            for (name, block) in &self.blocks {
                let declared = block
                    .info()
                    .capabilities
                    .unwrap_or_else(wafer_block::BlockCapabilities::unrestricted);

                // Strip + parse `capabilities` subkey.
                let config_caps = if let Some(cfg) = self.block_configs.get_mut(name) {
                    if let Some(obj) = cfg.as_object_mut() {
                        if let Some(raw) = obj.remove("capabilities") {
                            match serde_json::from_value::<wafer_block::BlockCapabilities>(raw) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::warn!(
                                        block = %name,
                                        error = %e,
                                        "failed to parse `capabilities` subkey — ignoring"
                                    );
                                    wafer_block::BlockCapabilities::unrestricted()
                                }
                            }
                        } else {
                            wafer_block::BlockCapabilities::unrestricted()
                        }
                    } else {
                        wafer_block::BlockCapabilities::unrestricted()
                    }
                } else {
                    wafer_block::BlockCapabilities::unrestricted()
                };

                let effective = declared.intersect(&config_caps);

                // Warn on any widening attempts (fields where config > declared).
                // We detect widening by checking if the intersection is STRICTLY
                // narrower than config on any field that config touched explicitly.
                log_widening_attempts(name, &declared, &config_caps, &effective);

                eff.insert(name.clone(), effective);
            }
            self.effective_capabilities = Arc::new(eff);
        }
```

Add the helper function at the end of `resolver.rs`:

```rust
fn log_widening_attempts(
    name: &str,
    _declared: &wafer_block::BlockCapabilities,
    config: &wafer_block::BlockCapabilities,
    effective: &wafer_block::BlockCapabilities,
) {
    // Booleans: if config asked for `true` but effective is `false`, declared denied.
    for (label, c, e) in [
        ("raw_sql", config.raw_sql, effective.raw_sql),
        ("crypto", config.crypto, effective.crypto),
        ("network", config.network, effective.network),
        ("config", config.config, effective.config),
    ] {
        if c && !e {
            tracing::warn!(
                block = %name,
                field = %label,
                "config widened capability beyond declared — narrower declaration wins"
            );
        }
    }

    // HashSet allowlists: items in config that did NOT survive intersection.
    let hash_fields: &[(&str, &std::collections::HashSet<String>, &std::collections::HashSet<String>)] = &[
        ("collections", &config.collections, &effective.collections),
        ("storage_folders", &config.storage_folders, &effective.storage_folders),
        ("config_keys", &config.config_keys, &effective.config_keys),
        ("callable_blocks", &config.callable_blocks, &effective.callable_blocks),
    ];
    for (label, c_set, e_set) in hash_fields {
        for item in c_set.iter() {
            if !e_set.contains(item) {
                tracing::warn!(
                    block = %name,
                    field = %label,
                    item = %item,
                    "config widened capability beyond declared — narrower declaration wins"
                );
            }
        }
    }

    // Vec allowlists: same shape.
    let vec_fields: &[(&str, &Vec<String>, &Vec<String>)] = &[
        ("network_allow", &config.network_allow, &effective.network_allow),
        ("headers.readable", &config.headers.readable, &effective.headers.readable),
        ("headers.writable", &config.headers.writable, &effective.headers.writable),
    ];
    for (label, c_vec, e_vec) in vec_fields {
        for item in c_vec.iter() {
            if !e_vec.iter().any(|x| x == item) {
                tracing::warn!(
                    block = %name,
                    field = %label,
                    item = %item,
                    "config widened capability beyond declared — narrower declaration wins"
                );
            }
        }
    }
}
```

Adapt imports (`use std::collections::HashMap;`, `use wafer_block::...;`) as needed — check the existing imports at the top of `resolver.rs` and extend.

- [ ] **Step 5: Run tests**

Run: `cargo test -p wafer-run --test capabilities_config_test`
Expected: the one new test passes.

Run: `cargo test --workspace`
Expected: no regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/wafer-run/src/runtime/resolver.rs crates/wafer-run/src/runtime.rs crates/wafer-run/tests/capabilities_config_test.rs
git commit -m "feat(wafer-run): parse capabilities config subkey and intersect at load"
```

---

## Task 7: Propagate effective caps into WASM blocks at load time

**Files:**
- Modify: `crates/wafer-run/src/runtime/resolver.rs`
- Modify: `crates/wafer-run/src/wasm/wasmi_loader.rs`

Currently, WASM blocks are loaded with a fixed `BlockCapabilities` via `load_with_capabilities`. This task makes the runtime use the *effective* caps computed in Task 6 for WASM blocks instead of whatever is currently hardcoded.

- [ ] **Step 1: Identify the WASM load call sites**

Run: `grep -n 'load_with_capabilities\|WasmiBlock' crates/wafer-run/src/runtime/resolver.rs`

The remote-WASM load path (per earlier exploration) uses `BlockCapabilities::none()`. There may be additional sites.

- [ ] **Step 2: Update the WASM load path to use effective caps**

For each call site that currently passes a hardcoded `BlockCapabilities`, replace with a lookup into the effective-caps map computed in Task 6:

```rust
// Before
let block = WasmiBlock::load_with_capabilities(&bytes, wafer_block::BlockCapabilities::none())?;

// After
let eff = self
    .effective_capabilities
    .get(&block_name)
    .cloned()
    .unwrap_or_else(wafer_block::BlockCapabilities::none);
let block = WasmiBlock::load_with_capabilities(&bytes, eff)?;
```

The key behavioral change: if a remote WASM block declared caps in its `__wafer_info`, those are the starting point; if it didn't declare, we fall back to `none()` (fully sandboxed) rather than `unrestricted()`. This is the "deny by default for WASM" invariant.

Since the effective-caps computation in Task 6 iterates `self.blocks`, but remote WASM blocks are loaded BEFORE they appear in `self.blocks`, you must either:

(a) Load the WASM block first into a temporary, extract its declared caps, compute the intersection with config, then re-load with the effective caps, OR
(b) Restructure the load to: (i) load the WASM into a pre-block object exposing `info()`, (ii) register it, (iii) compute effective caps, (iv) update the block's cap state.

Option (b) requires `WasmiBlock` to support updating its caps after construction; this is a nontrivial refactor.

Option (a) is simpler but loads the WASM twice. For remote blocks this is acceptable (loading happens at startup, not per-request).

Choose **option (a)** for now. Look at the existing remote-WASM load code in `resolver.rs` for the precise structure and reshape it to load declared → compute effective → reload.

- [ ] **Step 3: Write an integration test for WASM effective caps**

Append to `crates/wafer-run/tests/capabilities_config_test.rs`:

```rust
// Integration test placeholder for WASM caps:
// Requires a fixture WASM block (similar to echo_block.wasm) that declares
// capabilities in its __wafer_info export. If a suitable fixture is not yet
// available, this test is intentionally skipped with a NOTE so the functional
// equivalent is covered by the native-block test above plus the unit tests
// in Task 5 (sanitizers).
#[tokio::test]
#[ignore = "requires a WASM fixture that declares capabilities; see Task 8 notes"]
async fn wasm_block_effective_caps_match_declared_intersect_config() {
    // Intentionally left as a design placeholder — when a fixture exists,
    // implement: load the WASM, verify effective caps equal declared ∩ config.
}
```

The `#[ignore]` keeps the plan honest about what's testable now. Task 9 (integration tests) revisits this once a fixture can be produced.

- [ ] **Step 4: Run tests**

Run: `cargo test -p wafer-run`
Expected: pre-existing WASM tests still pass (the echo block continues to load correctly).

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-run/src/runtime/resolver.rs crates/wafer-run/src/wasm/wasmi_loader.rs crates/wafer-run/tests/capabilities_config_test.rs
git commit -m "feat(wafer-run): WASM blocks use declared ∩ config effective caps"
```

---

## Task 8: Extend `#[wafer_block]` macro with `capabilities(...)` argument

**Files:**
- Modify: `crates/wafer-block-macro/src/lib.rs`
- Create: `crates/wafer-block-macro/tests/capabilities_macro.rs`

- [ ] **Step 1: Inspect the current arg parser**

Read `crates/wafer-block-macro/src/lib.rs`, specifically the section where `args.get_str`, `args.get_str_list` are called (around lines 140-200 per the earlier grep). Locate the `args` type definition — it's likely a simple `HashMap<String, ArgValue>` populated from a `syn::punctuated::Punctuated<MetaNameValue, Token![,]>` parse pass. Note the pattern.

- [ ] **Step 2: Add parsing for nested `capabilities(...)`**

The `syn::Meta::List` variant handles paren-nested attribute args. Extend the attribute parser to recognize `capabilities(...)` alongside the existing `name = "..."` style, and extract each inner arg into a `CapabilitiesArgs` struct:

```rust
#[derive(Default)]
struct CapabilitiesArgs {
    crypto: bool,
    network: bool,
    raw_sql: bool,
    config: bool,
    collections: Vec<String>,
    storage_folders: Vec<String>,
    network_allow: Vec<String>,
    config_keys: Vec<String>,
    callable_blocks: Vec<String>,
    headers_readable: Vec<String>,
    headers_writable: Vec<String>,
    headers_masked: Vec<String>,
}

fn parse_capabilities(meta: &syn::MetaList) -> CapabilitiesArgs {
    let mut out = CapabilitiesArgs::default();
    let nested: syn::punctuated::Punctuated<syn::Meta, syn::Token![,]> =
        meta.parse_args_with(
            syn::punctuated::Punctuated::parse_terminated
        ).expect("#[wafer_block]: failed to parse capabilities(...)");
    for item in nested {
        match item {
            syn::Meta::Path(p) => {
                // Bare ident = true for bool fields.
                let ident = p.get_ident().expect("expected identifier").to_string();
                match ident.as_str() {
                    "crypto" => out.crypto = true,
                    "network" => out.network = true,
                    "raw_sql" => out.raw_sql = true,
                    "config" => out.config = true,
                    other => panic!("#[wafer_block]: unknown bool capability '{other}'"),
                }
            }
            syn::Meta::NameValue(nv) => {
                let ident = nv.path.get_ident().expect("expected identifier").to_string();
                // NameValue is `field = <expr>`. We support `field = ["a", "b"]` via the expr.
                let list = parse_string_list(&nv.value);
                match ident.as_str() {
                    "collections" => out.collections = list,
                    "storage_folders" => out.storage_folders = list,
                    "network_allow" => out.network_allow = list,
                    "config_keys" => out.config_keys = list,
                    "callable_blocks" => out.callable_blocks = list,
                    other => panic!("#[wafer_block]: unknown list capability '{other}'"),
                }
            }
            syn::Meta::List(inner) if inner.path.is_ident("headers") => {
                parse_headers_nested(&inner, &mut out);
            }
            other => panic!(
                "#[wafer_block]: unexpected token in capabilities(...): {other:?}"
            ),
        }
    }
    out
}

fn parse_headers_nested(inner: &syn::MetaList, out: &mut CapabilitiesArgs) {
    let nested: syn::punctuated::Punctuated<syn::Meta, syn::Token![,]> =
        inner
            .parse_args_with(syn::punctuated::Punctuated::parse_terminated)
            .expect("#[wafer_block]: failed to parse headers(...)");
    for item in nested {
        if let syn::Meta::NameValue(nv) = item {
            let ident = nv.path.get_ident().expect("expected ident").to_string();
            let list = parse_string_list(&nv.value);
            match ident.as_str() {
                "readable" => out.headers_readable = list,
                "writable" => out.headers_writable = list,
                "masked" => out.headers_masked = list,
                other => panic!("#[wafer_block]: unknown headers field '{other}'"),
            }
        }
    }
}

fn parse_string_list(expr: &syn::Expr) -> Vec<String> {
    if let syn::Expr::Array(arr) = expr {
        arr.elems
            .iter()
            .map(|e| match e {
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => s.value(),
                other => panic!("#[wafer_block]: expected string literal, got {other:?}"),
            })
            .collect()
    } else {
        panic!("#[wafer_block]: expected array expression `[...]`, got {expr:?}");
    }
}
```

Integrate `parse_capabilities` into the main attribute parser. After existing arg parsing, check whether a `capabilities(...)` group appeared; if so, call `parse_capabilities` and store the result. Generate the `BlockCapabilities` construction in the `info()` output:

```rust
// Inside the generated info() method, where the BlockInfo is constructed:
let capabilities_construction = if let Some(caps_args) = capabilities_args {
    let collections = caps_args.collections.iter().map(|s| quote! { #s.to_string() });
    // ... similar for other fields
    let headers_readable = caps_args.headers_readable.iter().map(|s| quote! { #s.to_string() });
    let headers_writable = caps_args.headers_writable.iter().map(|s| quote! { #s.to_string() });
    let headers_masked = caps_args.headers_masked.iter().map(|s| quote! { #s.to_string() });
    let crypto = caps_args.crypto;
    let network = caps_args.network;
    let raw_sql = caps_args.raw_sql;
    let config_cap = caps_args.config;
    quote! {
        Some(wafer_block::BlockCapabilities {
            collections: [ #(#collections),* ].into_iter().collect(),
            storage_folders: [ /* ... */ ].into_iter().collect(),
            // ...
            headers: wafer_block::HeaderPolicy {
                readable: vec![ #(#headers_readable),* ],
                writable: vec![ #(#headers_writable),* ],
                masked: vec![ #(#headers_masked),* ],
            },
            ..Default::default()
        })
    }
} else {
    quote! { None }
};
```

Attach this to the `info()` method's returned `BlockInfo` via `.capabilities(...)` when present.

- [ ] **Step 3: Write a macro-expansion test**

Create `crates/wafer-block-macro/tests/capabilities_macro.rs`:

```rust
//! Tests that the #[wafer_block(capabilities(...))] syntax produces a
//! BlockInfo whose .capabilities field is Some with the expected contents.

use wafer_block::{
    capabilities::{BlockCapabilities, HeaderPolicy},
    streams::{input::InputStream, output::OutputStream},
    types::{BlockInfo, LifecycleEvent, Message},
    Block, Context, WaferError,
};
use wafer_block_macro::wafer_block;

struct FullyDeclared;

#[wafer_block(
    name = "test/fully-declared",
    version = "0.1.0",
    interface = "middleware@v1",
    summary = "test",
    capabilities(
        crypto,
        network,
        collections = ["users", "sessions"],
        callable_blocks = ["wafer-run/crypto"],
        headers(
            readable = ["authorization"],
            writable = ["set-cookie"],
            masked = ["x-internal"],
        ),
    )
)]
impl FullyDeclared {
    async fn handle(_ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"{}".to_vec())
    }
}

#[test]
fn fully_declared_caps_present() {
    let info = <FullyDeclared as Block>::info(&FullyDeclared);
    let caps = info.capabilities.expect("caps present");
    assert!(caps.crypto);
    assert!(caps.network);
    assert!(!caps.raw_sql);
    assert!(caps.collections.contains("users"));
    assert!(caps.collections.contains("sessions"));
    assert!(caps.callable_blocks.contains("wafer-run/crypto"));
    assert_eq!(caps.headers.readable, vec!["authorization".to_string()]);
    assert_eq!(caps.headers.writable, vec!["set-cookie".to_string()]);
    assert_eq!(caps.headers.masked, vec!["x-internal".to_string()]);
}

struct Undeclared;

#[wafer_block(
    name = "test/undeclared",
    version = "0.1.0",
    interface = "middleware@v1",
    summary = "test"
)]
impl Undeclared {
    async fn handle(_ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(b"{}".to_vec())
    }
}

#[test]
fn undeclared_has_no_capabilities() {
    let info = <Undeclared as Block>::info(&Undeclared);
    assert!(info.capabilities.is_none());
}
```

Add `wafer-block-macro` and `wafer-block` and `async-trait` to the crate's `[dev-dependencies]` in `crates/wafer-block-macro/Cargo.toml` if not present. The proc-macro crate's own integration tests live under `tests/`.

- [ ] **Step 4: Run the macro tests**

Run: `cargo test -p wafer-block-macro`
Expected: 2 tests pass. If the macro emits `Option<BlockCapabilities>` construction correctly, both pass.

- [ ] **Step 5: Commit**

```bash
git add crates/wafer-block-macro/
git commit -m "feat(wafer-block-macro): accept capabilities(...) attribute arg"
```

---

## Task 9: Integration test with full wafer-test-support pipeline

**Files:**
- Create: `crates/wafer-run/tests/capabilities_e2e.rs`

- [ ] **Step 1: Write the integration tests**

Create `crates/wafer-run/tests/capabilities_e2e.rs`:

```rust
//! End-to-end integration: declared capabilities are read from BlockInfo,
//! intersected with operator config, and applied at dispatch / sanitization.
//!
//! Uses wafer-test-support::WaferBuilder for runtime setup.

use std::sync::Arc;

use serde_json::json;
use wafer_block::{
    capabilities::{BlockCapabilities, HeaderPolicy},
    streams::{input::InputStream, output::OutputStream},
    types::{BlockInfo, LifecycleEvent, Message, MetaEntry},
    Block, Context, WaferError,
};
use wafer_run::Wafer;
use wafer_test_support::builder::WaferBuilder;

/// A native block that declares capabilities via BlockInfo::capabilities.
/// Native blocks do not enforce caps, but they should be stored for
/// inspector visibility.
struct DeclaringNative {
    info: BlockInfo,
}

#[async_trait::async_trait]
impl Block for DeclaringNative {
    fn info(&self) -> BlockInfo {
        self.info.clone()
    }
    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // Echo whatever meta reaches us into the response body as JSON.
        let keys: Vec<String> = msg.meta().iter().map(|e| e.key.clone()).collect();
        let body = serde_json::to_vec(&json!({"received_keys": keys})).unwrap();
        OutputStream::respond(body)
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _event: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}

fn make_declaring(name: &str, caps: BlockCapabilities) -> Arc<DeclaringNative> {
    let info = BlockInfo::new(name, "0.1.0", "middleware@v1", "declares caps").capabilities(caps);
    Arc::new(DeclaringNative { info })
}

#[tokio::test]
async fn declared_caps_applied_when_no_config() {
    let mut declared = BlockCapabilities::default();
    declared.collections = ["users"].iter().map(|s| s.to_string()).collect();
    declared.crypto = true;

    let block = make_declaring("test/declaring-a", declared.clone());
    let wafer = WaferBuilder::new()
        .with_block("test/declaring-a", block)
        .build()
        .await
        .expect("build");

    let eff = wafer
        .effective_capabilities("test/declaring-a")
        .expect("effective caps stored");
    assert!(eff.crypto);
    assert!(eff.collections.contains("users"));
}

#[tokio::test]
async fn operator_config_narrows_declared() {
    let mut declared = BlockCapabilities::default();
    declared.network = true;
    declared.network_allow = vec!["https://a.com/".into(), "https://b.com/".into()];

    let block = make_declaring("test/declaring-b", declared);
    let wafer = WaferBuilder::new()
        .with_block("test/declaring-b", block)
        .with_config(
            "test/declaring-b",
            json!({
                "capabilities": {
                    "network_allow": ["https://a.com/"]
                }
            }),
        )
        .build()
        .await
        .expect("build");

    let eff = wafer.effective_capabilities("test/declaring-b").unwrap();
    assert!(eff.network);
    assert_eq!(eff.network_allow, vec!["https://a.com/".to_string()]);
}

#[tokio::test]
async fn operator_config_cannot_widen_declared() {
    let mut declared = BlockCapabilities::default();
    declared.network = false; // Explicitly denied.

    let block = make_declaring("test/declaring-c", declared);
    let wafer = WaferBuilder::new()
        .with_block("test/declaring-c", block)
        .with_config(
            "test/declaring-c",
            json!({
                "capabilities": {
                    "network": true   // Attempts to widen.
                }
            }),
        )
        .build()
        .await
        .expect("build");

    let eff = wafer.effective_capabilities("test/declaring-c").unwrap();
    // Narrower wins.
    assert!(!eff.network);
    // (The warn log is a side effect — not asserted here; covered by
    // dedicated unit test if needed.)
}

#[tokio::test]
async fn native_block_declares_but_not_enforced() {
    // Native block declares collections = ["users"] but the runtime does NOT
    // enforce this — the test just verifies the declaration is stored and
    // that dispatch still succeeds regardless.
    let mut declared = BlockCapabilities::default();
    declared.collections = ["users"].iter().map(|s| s.to_string()).collect();

    let block = make_declaring("test/native-declared", declared);
    let wafer = WaferBuilder::new()
        .with_block("test/native-declared", block)
        .build()
        .await
        .expect("build");

    // Declaration is observable.
    let eff = wafer
        .effective_capabilities("test/native-declared")
        .expect("stored");
    assert!(eff.collections.contains("users"));

    // Dispatch succeeds even though the block would be "restricted" if enforced.
    let msg = Message::new("http.request");
    let out = wafer
        .run_block("test/native-declared", msg, InputStream::empty())
        .await;
    match out.collect_buffered().await {
        Ok(buf) => {
            let resp: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
            assert!(resp.get("received_keys").is_some());
        }
        other => panic!("expected Respond from native block, got {other:?}"),
    }
}
```

- [ ] **Step 2: Add wafer-test-support as dev-dep**

Open `crates/wafer-run/Cargo.toml`. Confirm `wafer-test-support = { path = "../wafer-test-support" }` is present under `[dev-dependencies]`. It should already be there from Spec 2A (commit history on main).

- [ ] **Step 3: Run the integration tests**

Run: `cargo test -p wafer-run --test capabilities_e2e`
Expected: 4 tests pass.

Run: `cargo test --workspace` to confirm no regressions.

- [ ] **Step 4: Commit**

```bash
git add crates/wafer-run/tests/capabilities_e2e.rs
git commit -m "test(wafer-run): capabilities e2e — declared + config + native asymmetry"
```

---

## Task 10: Documentation — wafer-site page and solobase pointers

**Files:**
- Create: `crates/wafer-site/content/docs/block-capabilities.md` (or adapt to the site's actual content layout)
- Modify: `crates/wafer-block/src/capabilities.rs` (rustdoc cross-references)
- Modify: `crates/wafer-block/src/types.rs` (rustdoc on `BlockInfo::capabilities`)

Note: `solobase` lives in a sibling repository (`/home/joris/Programs/suppers-ai/workspace/solobase`). Making commits there from this branch is out of scope for a single PR — instead, this task produces a **documentation placeholder** in the wafer-run repo that references solobase's eventual page, and a README-level bullet reminding the solobase maintainer to add a parallel page. The solobase-side doc happens as a follow-up.

- [ ] **Step 1: Identify the wafer-site content location**

Run: `find crates/wafer-site -name '*.md' -path '*content*' | head -5`

Also inspect `crates/wafer-site/Cargo.toml` and `crates/wafer-site/src/` to understand how pages are authored. If the site uses maud templates or a typed content model, adapt the following step to match — the content itself is what matters.

- [ ] **Step 2: Write the wafer-site documentation page**

Create a new content file (exact path depends on the site's layout). Content:

```markdown
---
title: Block capabilities
---

# Block capabilities

Blocks declare what platform services they need; operators narrow those
declarations via config; the runtime enforces the intersection — but
only on WASM blocks.

## Declaring capabilities

A block declares what it needs using the `#[wafer_block(capabilities(...))]`
attribute:

\`\`\`rust
#[wafer_block(
    name = "suppers-ai/auth",
    version = "0.1.0",
    interface = "middleware@v1",
    summary = "Authentication middleware",
    capabilities(
        crypto,
        config,
        collections = ["users", "sessions", "api_keys"],
        config_keys = ["SUPPERS_AI__AUTH__JWT_SECRET"],
        callable_blocks = ["wafer-run/crypto", "wafer-run/database"],
        headers(
            readable = ["authorization", "cookie"],
            writable = ["set-cookie"],
        ),
    )
)]
\`\`\`

Bare identifiers (e.g., `crypto`) are boolean fields set to true. Array-valued
fields accept string literals. Nested `headers(...)` maps to `HeaderPolicy`.

## Native vs WASM — enforcement asymmetry

**WASM blocks: declarations are enforced at dispatch.** Attempts to access
collections, network URLs, or headers outside the declared set fail loudly.

**Native Rust blocks: declarations are documentation only.** Native blocks
run in-process with full trust; adding a `capabilities(...)` block to a
native `#[wafer_block]` records the intent and surfaces it in the inspector,
but the runtime does not enforce the restrictions at call time. The
declaration's value is for audit trails and inspector UIs, not sandboxing.

This asymmetry is intentional. WASM is the untrusted boundary; native is the
trusted runtime. Applying the same enforcement layer to native would add
runtime cost to the trust-by-default path.

## Operator narrowing via config

Operators narrow a block's declared capabilities via a reserved
`capabilities` subkey in block config. The runtime intersects declared
caps with config caps. Narrowing only — config cannot grant more than
the block declared.

\`\`\`json
{
  "SUPPERS_AI__AUTH__JWT_SECRET": "...",
  "capabilities": {
    "network": false,
    "collections": ["users"],
    "headers": {
      "writable": []
    }
  }
}
\`\`\`

If config asks for more than the block declared (e.g., `network: true`
when the block declared `network: false`), the narrower declaration wins
and a warning log surfaces:

\`\`\`
WARN block=suppers-ai/auth: config widened capability beyond declared — narrower declaration wins
\`\`\`

## Header policy

Headers are handled separately via `HeaderPolicy`:

- **`readable`** — sensitive inbound headers the block may read. Default-
  denied set (Authorization, Cookie, Set-Cookie, Location, CORS headers,
  HSTS, X-Frame-Options, CSP) is masked unless explicitly allowed.
- **`writable`** — sensitive outbound headers the block may write. Same
  default-deny set; allowlist-override per header.
- **`masked`** — additional headers to mask. Applies to both directions.
  Operator extension for app-specific sensitive headers (e.g.,
  `x-internal-token`).

Intersection rules: allowlists intersect (narrower wins), masked unions
(stricter wins).

## Inspecting effective capabilities

The existing `/blocks` inspector endpoint exposes each registered block's
`BlockInfo`. After Spec 2B, the `capabilities` field appears there, showing
the block's declared caps. For WASM blocks, the *effective* (post-intersection)
caps are also available via the runtime's block-info API for operators
auditing trust.

---

See also: `solobase/docs/block-capabilities.md` (parallel page in solobase's
docs tree — same model applied to the solobase-specific blocks).
```

- [ ] **Step 3: Add rustdoc pointers**

At the top of `crates/wafer-block/src/capabilities.rs`, add a module-level doc:

```rust
//! Block capability declarations and enforcement policy.
//!
//! See the wafer-site docs page "Block capabilities" for the high-level
//! model: declare → narrow → enforce. The TL;DR:
//!
//! - Blocks declare required capabilities in `BlockInfo::capabilities`.
//! - Operators narrow via a `capabilities` subkey in block config.
//! - The runtime intersects declared ∩ config and enforces on WASM blocks.
//! - Native blocks' declarations are documentation-only.
```

On `BlockInfo::capabilities` in `types.rs`, the rustdoc already added in Task 3 is sufficient; optionally cross-reference the wafer-site page. On `HeaderPolicy`, the rustdoc added in Task 1 is sufficient.

- [ ] **Step 4: Add a follow-up note for solobase docs**

Append to the wafer-site page:

```markdown
### Solobase-specific guidance

The solobase repository maintains its own parallel documentation page
describing how each solobase block (auth, iam-guard, readonly-guard,
ip-rate-limit, products, files, etc.) declares and narrows capabilities.
That page is authored in the solobase repo, not here. When Spec 2B adoption
rolls out to solobase blocks, the solobase docs get the parallel update.
```

- [ ] **Step 5: Verify docs build / compile**

If wafer-site has a build step (e.g., `cargo run -p wafer-site -- build` or similar), run it to confirm the new page renders. If the site is static markdown with no build, just verify the file exists and renders in a markdown preview.

Run: `cargo test --doc -p wafer-block` to confirm the new rustdoc links / code blocks in docs don't break.

- [ ] **Step 6: Commit**

```bash
git add crates/wafer-site/ crates/wafer-block/src/capabilities.rs crates/wafer-block/src/types.rs
git commit -m "docs: block-capabilities page on wafer-site + rustdoc pointers"
```

---

## Post-implementation checklist

- [ ] `cargo test --workspace` — all tests pass; 25+ new tests across Tasks 1–9.
- [ ] `cargo clippy --workspace --no-deps -- -D warnings` — clean on new code.
- [ ] Existing `echo_block.wasm` tests still pass (the WASM loader still works with its current default `BlockCapabilities::none()` when no declaration exists).
- [ ] The wafer-site page is reachable and explicit about the native-vs-WASM asymmetry.
- [ ] `solobase` docs follow-up ticket filed (can be a simple reminder in the final PR description — the actual solobase doc lives in that repo).
