//! WRAP — WAFER Resource Access Policy.
//!
//! Enforces resource-level access control in the runtime's `call_block()` dispatch.
//! Client wrappers set `wrap.resource` meta on the message; the runtime reads it
//! and calls `check_access()` before dispatching to the handler.

use crate::{
    common::ServiceOp,
    types::{ResourceAccess, ResourceGrant},
    ErrorCode, WaferError,
};

/// Sentinel `wrap.resource` value for raw-SQL access. Admin-only.
///
/// Set by the client wrapper when a block issues a raw SQL statement and
/// matched here (and in the runtime's capability check) to gate the
/// privilege. Same literal on both sides — no translation.
pub const RAW_SQL_RESOURCE: &str = "__raw_sql__";

/// Sentinel `wrap.resource` value for DDL access. Open to any attributable
/// caller (convention: blocks DDL only their own tables; enforced by review +
/// the WRAP-grant audit script, not by parsing SQL).
///
/// Set by the client wrapper for `CREATE TABLE` / `ALTER TABLE` and matched
/// here (and in the runtime's capability check). Same literal on both sides.
pub const DDL_RESOURCE: &str = "__ddl__";

/// Sentinel `wrap.resource` value for the STRUCTURED schema ops
/// (`database.ensure_table` / `add_column` / `drop_table`). Open to any
/// attributable caller, exactly like [`DDL_RESOURCE`] (`check_access` rule
/// 1a) — the ops are already scoped by a second check on the table name, so
/// the sentinel only asks "is this caller attributable at all".
///
/// Deliberately NOT [`DDL_RESOURCE`]: the structured ops build their own SQL
/// from a validated `TableDef`, so a block that needs them does not thereby
/// need `db::ddl()`'s arbitrary-statement channel. A guest can hold
/// `schema: true, ddl: false` and still create its own tables.
pub const SCHEMA_RESOURCE: &str = "__schema__";

/// Reserved WRAP resource for `storage.list_folders`, which enumerates every
/// folder in the backend with no per-folder scope. Gated admin-only (like
/// [`RAW_SQL_RESOURCE`]): a global, privileged listing is not something an
/// untrusted block may perform. The `__`-reserved name cannot collide with a
/// real folder path.
pub const STORAGE_LIST_ALL_RESOURCE: &str = "__storage_list_all__";

/// WRAP resource ([`ResourceType::Auth`](crate::types::ResourceType::Auth))
/// of `auth.user_profile`, which returns any user's email, role and orgs
/// for a `user_id` the caller names. The auth service answers from its own
/// authority, so the resource sits in the auth block's `wafer_run__auth__`
/// namespace and takes the namespace rules of [`check_access`]: the auth
/// block and the admin block are admitted, any other caller needs a grant,
/// and only the auth block can declare one.
pub const AUTH_USER_PROFILE_RESOURCE: &str = "wafer_run__auth__user_profile";

/// WRAP resource of `auth.require_user`. One of the credential ops (see
/// [`is_auth_credential_resource`]).
pub const AUTH_REQUIRE_USER_RESOURCE: &str = "wafer_run__auth__require_user";

/// WRAP resource of `auth.require_token`. One of the credential ops (see
/// [`is_auth_credential_resource`]).
pub const AUTH_REQUIRE_TOKEN_RESOURCE: &str = "wafer_run__auth__require_token";

/// WRAP resource of `auth.require_role`. One of the credential ops (see
/// [`is_auth_credential_resource`]).
pub const AUTH_REQUIRE_ROLE_RESOURCE: &str = "wafer_run__auth__require_role";

/// Whether `resource` is one of the auth service's credential ops
/// (`require_user`, `require_token`, `require_role`). Each resolves the
/// credential the caller forwards in its own message, so it tells the caller
/// nothing the credential it already holds does not: [`check_access`] admits
/// any attributable caller to them, with no grant.
pub fn is_auth_credential_resource(resource: &str) -> bool {
    matches!(
        resource,
        AUTH_REQUIRE_USER_RESOURCE | AUTH_REQUIRE_TOKEN_RESOURCE | AUTH_REQUIRE_ROLE_RESOURCE
    )
}

/// WRAP resource ([`ResourceType::Llm`](crate::types::ResourceType::Llm) or
/// [`ResourceType::Image`](crate::types::ResourceType::Image)) of one model
/// a model-serving block serves: `{org}__{block}__{backend_id}/{model_id}`,
/// with `block` the serving block's registered name. The resource sits in
/// that block's namespace, so [`check_access`] admits the block itself, the
/// admin block, or a caller holding a grant — which only the serving block
/// can declare.
///
/// A `backend_id` containing `/` is refused with `InvalidArgument`: the `/`
/// that ends the backend would be ambiguous, and a grant on one backend's
/// models (`{prefix}openai/*`) would also match a backend named `openai/x`.
pub fn model_resource(block: &str, backend_id: &str, model_id: &str) -> Result<String, WaferError> {
    if backend_id.contains('/') {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            format!("backend_id `{backend_id}` must not contain `/`"),
        ));
    }
    Ok(format!("{}{backend_id}/{model_id}", resource_prefix(block)))
}

/// WRAP resource of a service operation that names no model:
/// `{org}__{block}__{name}`, where `name` is `op` after its family's `.`
/// (`llm.list_models` → `list_models`) and `block` is the serving block's
/// registered name. Namespaced like [`model_resource`], and never equal to
/// one: an op name has no `/`.
pub fn op_resource(block: &str, op: &str) -> String {
    let name = op.split_once('.').map_or(op, |(_, name)| name);
    format!("{}{name}", resource_prefix(block))
}

/// Extract the owning block ID from a namespaced resource name.
///
/// Convention: `my_org__auth__users` → `my-org/auth`
///
/// Splits on the first two `__` segments, lowercases, converts `__` → `/`
/// and `_` → `-`. Returns `None` if the name doesn't have at least two `__`
/// separators (i.e. `{org}__{block}__{resource}`).
///
/// # Invariant
///
/// The `__ ↔ /` and `_ ↔ -` round-trip with [`resource_prefix`] is only
/// lossless when each block-id segment matches `^[a-z0-9]+(?:-[a-z0-9]+)*$`
/// (lowercase alphanumeric, single internal hyphens). That pattern is
/// enforced at registration by `wafer_run::runtime::validate_block_name`, so
/// any block reaching this code already satisfies it. A segment containing a
/// literal `_` would collide with the separator encoding and break the
/// round-trip — registration rejects such names before they get here.
pub fn resource_owner(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let first = lower.find("__")?;
    let rest = &lower[first + 2..];
    let second = rest.find("__")?;
    // prefix = "my_org__auth"
    let prefix = &lower[..first + 2 + second];
    Some(prefix.replace("__", "/").replace('_', "-"))
}

/// Convert a block ID to its resource name prefix.
///
/// `my-org/auth` → `my_org__auth__`
///
/// # Invariant
///
/// Inverse of [`resource_owner`]; the round-trip is lossless only for block
/// ids whose segments match `^[a-z0-9]+(?:-[a-z0-9]+)*$`. That pattern is
/// enforced at registration by `wafer_run::runtime::validate_block_name`
/// (exactly two `/`-separated segments, lowercase alphanumeric, single
/// internal hyphens, no underscores), so callers never pass an id that would
/// produce an ambiguous prefix.
pub fn resource_prefix(block_id: &str) -> String {
    let mut prefix = block_id.replace('/', "__").replace('-', "_");
    prefix.push_str("__");
    prefix
}

/// Extract the owning block id from a storage path of the form
/// `{org}/{block}/{rest}`.
///
/// The path is the resolved backend path the storage handler authorizes and
/// touches — never the `@`-prefixed form a caller may write in a request,
/// which the handler strips before a path reaches WRAP.
///
/// Returns `None` if the path doesn't have at least two slash-separated
/// segments.
///
/// ```ignore
/// assert_eq!(storage_resource_owner("my-org/files/photos/a.png"),
///            Some("my-org/files".to_string()));
/// assert_eq!(storage_resource_owner("just-one-segment"), None);
/// ```
pub fn storage_resource_owner(path: &str) -> Option<String> {
    let mut parts = path.splitn(3, '/');
    let org = parts.next()?;
    let block = parts.next()?;
    if org.is_empty() || block.is_empty() {
        return None;
    }
    Some(format!("{org}/{block}"))
}

/// Whether every `/`-separated segment of `path` is a plain name — i.e. the
/// path has no EMPTY segment (a leading or trailing `/`, a `//` run, or the
/// empty string itself), no RELATIVE segment (`.` or `..`), and no `\`.
///
/// `/` is the only separator in a storage path. A backend that maps the path
/// onto a filesystem (`wafer-block-local-storage` joins it with `Path::join`)
/// reads `\` as a separator too on Windows, so `a\..\..\b` — one plain
/// segment here — would climb there. Refusing `\` keeps the path WRAP
/// authorizes the path every backend touches.
///
/// Storage authorization is textual and prefix-based: the handler authorizes
/// on `"{folder}/{key}"` and
/// [`BlockCapabilities::allows_storage_folder`](crate::capabilities::BlockCapabilities::allows_storage_folder)
/// admits a resource that lies beneath a granted entry. Nothing in that path
/// normalizes the string, so an unnormalized `..` segment would let a caller
/// granted `site/jhg` reach `site/other` by asking for key `../other` —
/// the resource `site/jhg/../other` textually sits under the grant. The fix
/// is to refuse the shape outright rather than to normalize (normalizing
/// would silently rewrite what the caller asked for): the storage handler
/// rejects such a `folder`/`key` with `InvalidArgument` before authorizing,
/// and the capability check refuses it as a second, independent layer.
pub fn is_traversal_safe_path(path: &str) -> bool {
    !path.split('/').any(|segment| {
        segment.is_empty() || segment == "." || segment == ".." || segment.contains('\\')
    })
}

/// Dispatch to the right resource-owner parser for the given resource type.
///
/// `ResourceType::Storage` parses slash-separated `{org}/{block}/...` paths
/// via [`storage_resource_owner`]. Everything else (Db, Config, Vector,
/// Auth, Llm, Image, Embedding, untyped) parses double-underscore
/// `{org}__{block}__...` names via
/// [`resource_owner`].
///
/// Used by `check_access` and by the lifecycle grant validator to apply
/// ownership rules consistently across resource types.
pub fn typed_resource_owner(
    resource: &str,
    resource_type: Option<&crate::types::ResourceType>,
) -> Option<String> {
    match resource_type {
        Some(crate::types::ResourceType::Storage) => storage_resource_owner(resource),
        _ => resource_owner(resource),
    }
}

/// Check whether `caller_id` is allowed to access `resource`.
///
/// For namespace-based resources (Db, Config, Vector, Auth, Llm, Image,
/// Embedding, or untyped):
/// 1. `__raw_sql__` → admin-only (exact match on `admin_block`)
/// 2. `__ddl__` / `__schema__` → any attributable caller (NOT admin-only).
///    Convention is that blocks only reshape their own (`{org}__{block}__*`)
///    tables; this is enforced by code review + the WRAP-grant audit script,
///    not by parsing SQL here. An Auth credential op
///    ([`is_auth_credential_resource`]) is likewise open to any attributable
///    caller; every other Auth resource (e.g. [`AUTH_USER_PROFILE_RESOURCE`])
///    takes the rules below.
/// 3. `WAFER_RUN_SHARED__*` → any block reads, admin-only writes
/// 4. Own resource (`resource_owner(resource) == caller_id`) → Ok
/// 5. Admin (`caller_id == admin_block`) → Ok
/// 6. Grant match (grantee + resource pattern + [`ResourceGrant::admits`]
///    the requested `access` + resource type) → Ok
/// 7. Unnamespaced (`resource_owner()` returns `None`) → Err
/// 8. Otherwise → Err
///
/// For Storage (slash-separated `{org}/{block}/...` paths — the resolved
/// backend path, which the storage handler derives from the caller's request
/// and then touches unchanged):
/// 1. Own resource (`storage_resource_owner(resource) == caller_id`) → Ok
/// 2. Admin → Ok
/// 3. Grant match → Ok
/// 4. Otherwise → Err (default deny)
///
/// The Storage list-all sentinel ([`STORAGE_LIST_ALL_RESOURCE`], used by
/// `storage.list_folders`) is a special case checked before the Storage
/// self-admit: admin-only, with no grant fallthrough (a global folder
/// enumeration is privileged, like raw SQL).
///
/// For Network and Crypto (URLs, operation names — not namespaced):
/// 1. Admin → Ok
/// 2. Grant match → Ok
/// 3. Otherwise → Err (default deny)
pub fn check_access(
    caller_id: Option<&str>,
    resource: &str,
    access: ResourceAccess,
    resource_type: Option<&crate::types::ResourceType>,
    grants: &[ResourceGrant],
    admin_block: &str,
) -> Result<(), WaferError> {
    // Namespace-based rules apply to Db, Config, Vector, Auth, Llm, Image,
    // Embedding, or untyped resources.
    // Network, Storage, and Crypto resources use URLs / file-paths /
    // operation-names, not the {org}__{block}__{name} convention.
    let namespace_based = !matches!(
        resource_type,
        Some(crate::types::ResourceType::Network)
            | Some(crate::types::ResourceType::Storage)
            | Some(crate::types::ResourceType::Crypto)
    );

    if namespace_based {
        // Rule 1: raw SQL is admin-only
        if resource == RAW_SQL_RESOURCE {
            return match caller_id {
                Some(c) if c == admin_block => Ok(()),
                _ => Err(WaferError::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "WRAP: raw SQL access denied (caller: {caller_id:?}, admin: {admin_block})"
                    ),
                )),
            };
        }

        // Rule 1a: DDL and the structured schema ops are open to any
        // attributable caller. Each block is expected to shape only its own
        // tables on init (`{org}__{block}__*`); cross-block DDL is a misuse
        // caught by code review + the WRAP-grant audit script, not by parsing
        // SQL here. Anonymous callers (no `caller_id`) are still denied —
        // schema changes need an attributable owner. The two sentinels are
        // separate resources (and separate capabilities) so a block can hold
        // the structured ops without the raw-statement channel.
        if resource == DDL_RESOURCE || resource == SCHEMA_RESOURCE {
            let what = if resource == DDL_RESOURCE {
                "DDL"
            } else {
                "schema ops"
            };
            return match caller_id {
                Some(_) => Ok(()),
                None => Err(WaferError::new(
                    ErrorCode::PermissionDenied,
                    format!("WRAP: {what} requires an attributable caller (caller: None)"),
                )),
            };
        }

        // Rule 1b: the auth service's credential ops answer from the
        // credential the caller forwards, not from the auth block's own
        // authority, so any attributable caller may ask. Anonymous callers
        // are denied, as for DDL.
        if resource_type == Some(&crate::types::ResourceType::Auth)
            && is_auth_credential_resource(resource)
        {
            return match caller_id {
                Some(_) => Ok(()),
                None => Err(WaferError::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "WRAP: auth credential op '{resource}' requires an attributable caller \
                         (caller: None)"
                    ),
                )),
            };
        }

        // Rule 2: WAFER_RUN_SHARED__ resources
        //
        // Writes (append included): admin only.
        // Reads: any *attributable* caller (caller_id.is_some()). Anonymous
        // callers (None) are denied — shared config may carry secrets and
        // there is no reason an unauthenticated context should read them.
        if resource
            .get(..crate::types::WAFER_RUN_SHARED_PREFIX.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(crate::types::WAFER_RUN_SHARED_PREFIX))
        {
            if access != ResourceAccess::Read {
                return match caller_id {
                    Some(c) if c == admin_block => Ok(()),
                    _ => Err(WaferError::new(
                        ErrorCode::PermissionDenied,
                        format!(
                            "WRAP: only admin can write WAFER_RUN_SHARED__ resources (caller: {caller_id:?})"
                        ),
                    )),
                };
            }
            return match caller_id {
                Some(_) => Ok(()),
                None => Err(WaferError::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "WRAP: WAFER_RUN_SHARED__ read denied for anonymous caller (resource: {resource})"
                    ),
                )),
            };
        }

        // Rule 3: own resource
        let owner = resource_owner(resource);
        if let Some(ref owner) = owner {
            if caller_id == Some(owner.as_str()) {
                return Ok(());
            }
        }

        // Rule 4: admin block has full access
        if caller_id == Some(admin_block) {
            return Ok(());
        }

        // Rule 5: grant match
        if let Some(caller) = caller_id {
            if grants
                .iter()
                .any(|g| grant_allows(g, caller, resource, access, resource_type))
            {
                return Ok(());
            }
        }

        // Rule 6: unnamespaced resource → deny
        if owner.is_none() {
            return Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!("WRAP: unnamespaced resource '{resource}' denied (caller: {caller_id:?})"),
            ));
        }

        // Rule 7: no match → deny
        return Err(WaferError::new(
            ErrorCode::PermissionDenied,
            format!("WRAP: access denied on '{resource}' (caller: {caller_id:?})"),
        ));
    }

    // --- Non-namespace resources (Network, Storage, Crypto) ---
    // Storage admits its owner; everything else falls through to admin +
    // grant matching.
    let is_storage = matches!(resource_type, Some(crate::types::ResourceType::Storage));

    // Storage list-all sentinel: a global folder enumeration is admin-only,
    // like raw SQL, with no grant fallthrough.
    if is_storage && resource == STORAGE_LIST_ALL_RESOURCE {
        return match caller_id {
            Some(c) if c == admin_block => Ok(()),
            _ => Err(WaferError::new(
                ErrorCode::PermissionDenied,
                format!(
                    "WRAP: storage.list_folders (list-all) is admin-only \
                     (caller: {caller_id:?}, admin: {admin_block})"
                ),
            )),
        };
    }

    // Storage paths are matched by owner and prefix and never normalized, so
    // `acme/app/../other/x` would read as `acme/app`'s own resource. The
    // storage handler refuses that shape before it authorizes; refusing it
    // here too keeps every other caller of this check from admitting it.
    if is_storage && !is_traversal_safe_path(resource) {
        return Err(WaferError::new(
            ErrorCode::PermissionDenied,
            format!(
                "WRAP: storage path '{resource}' has an empty, `.` or `..` segment or a `\\` \
                 (caller: {caller_id:?})"
            ),
        ));
    }

    // Storage own resource: the path's `{org}/{block}` owner is the caller.
    // A path owned by another block — or by no block — needs admin or a
    // grant.
    if is_storage {
        if let Some(caller) = caller_id {
            if storage_resource_owner(resource).as_deref() == Some(caller) {
                return Ok(());
            }
        }
    }

    // Admin block has full access
    if caller_id == Some(admin_block) {
        return Ok(());
    }

    // Grant match
    if let Some(caller) = caller_id {
        if grants
            .iter()
            .any(|g| grant_allows(g, caller, resource, access, resource_type))
        {
            return Ok(());
        }
    }

    // Default deny
    Err(WaferError::new(
        ErrorCode::PermissionDenied,
        format!(
            "WRAP: access denied on '{resource}' (caller: {caller_id:?}, type: {resource_type:?})"
        ),
    ))
}

fn grant_matches_grantee(grantee: &str, caller: &str) -> bool {
    grantee == "*" || grantee == caller
}

/// Whether a single grant admits `caller` to `resource` — the one place that
/// encodes the four grant-matching conditions (grantee, resource pattern,
/// access kind, resource-type guard). Used by both the namespace-resource and
/// storage/network/crypto branches of [`check_access`]; the latter passes the
/// canonicalized (`@`-stripped) resource.
fn grant_allows(
    grant: &ResourceGrant,
    caller: &str,
    resource: &str,
    access: ResourceAccess,
    resource_type: Option<&crate::types::ResourceType>,
) -> bool {
    if !grant_matches_grantee(&grant.grantee, caller) {
        return false;
    }
    if !grant_matches_resource(&grant.resource, resource) {
        return false;
    }
    if !grant.admits(access) {
        return false;
    }
    if let Some(ref grant_type) = grant.resource_type {
        match resource_type {
            Some(req_type) if grant_type == req_type => {}
            _ => return false,
        }
    }
    true
}

/// Pattern matching for grant resource patterns.
///
/// Two modes:
/// 1. URL patterns (pattern starts with `http://` or `https://`) — parsed
///    with the `url` crate and matched structurally: scheme + host + path.
///    Subdomain wildcards (`*.example.com`) match exactly one label.
/// 2. Non-URL patterns (storage paths, namespaces) — substring glob with
///    `*` wildcards, used for things like `wafer-run/web/*` or
///    `my_org__auth__*`.
///
/// The URL branch closes the substring-glob bypass (SEC-007): patterns
/// like `https://*.example.com/*` no longer match attacker-controlled
/// strings like `https://example.com.attacker.com/...`.
///
/// Supported URL patterns:
/// - `https://api.stripe.com/*` — any path under exact host
/// - `https://*.example.com/*` — single-level subdomain wildcard
/// - `https://**.example.com/*` — multi-level subdomain wildcard
/// - `https://example.com/v1/items` — exact match (no wildcard)
///
/// Returns `false` for malformed URL patterns (cannot match anything).
fn grant_matches_resource(pattern: &str, resource: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    // URL-shaped patterns get structured matching.
    if is_url_pattern(pattern) {
        return url_pattern_matches(pattern, resource);
    }

    // Non-URL patterns: substring glob (storage paths, namespace prefixes, etc).
    glob_matches(pattern, resource)
}

fn is_url_pattern(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Substring-glob matcher for non-URL patterns (storage paths, namespaces).
///
/// `*` is a wildcard. First segment must anchor to the start. If the
/// pattern doesn't end in `*`, the resource must also end at the final
/// segment.
fn glob_matches(pattern: &str, resource: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return resource == pattern;
    }

    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = resource[pos..].find(part) {
            if i == 0 && found != 0 {
                return false;
            }
            pos += found + part.len();
        } else {
            return false;
        }
    }

    if !pattern.ends_with('*') {
        return pos == resource.len();
    }
    true
}

/// Structured URL pattern matcher.
///
/// - Scheme: exact match (case-insensitive — `url` crate normalizes).
/// - Host:
///   - `**.example.com` — matches `example.com` and any depth of subdomains
///   - `*.example.com`  — matches exactly one extra label (`a.example.com`,
///     not `a.b.example.com`, not `example.com`)
///   - `example.com`    — exact match
///   - `*`              — matches any host
/// - Path: prefix match if pattern ends with `/*`; otherwise exact.
///
/// The `url` crate parses both sides, which means `example.com.evil.com`
/// becomes a literal host (it's not interpreted as a subdomain of
/// `example.com`) — that's exactly what closes the SEC-007 bypass.
fn url_pattern_matches(pattern: &str, resource: &str) -> bool {
    // Replace `*` with a placeholder that's valid in both host labels and
    // URL paths so the pattern parses cleanly. `url::Url` lowercases hosts,
    // so the sentinel must be lowercase as well to survive the round-trip.
    const STAR_SENTINEL: &str = "wafer-star-placeholder";

    let prepared_pattern = pattern.replace('*', STAR_SENTINEL);
    let Ok(parsed_pattern) = url::Url::parse(&prepared_pattern) else {
        return false;
    };
    let Ok(parsed_resource) = url::Url::parse(resource) else {
        return false;
    };

    // Scheme: exact.
    if parsed_pattern.scheme() != parsed_resource.scheme() {
        return false;
    }

    // Host: extract the original (un-sentinel-ed) host pattern.
    let pattern_host = parsed_pattern
        .host_str()
        .map(|h| h.replace(STAR_SENTINEL, "*"));
    let resource_host = parsed_resource.host_str();
    if !host_matches(pattern_host.as_deref(), resource_host) {
        return false;
    }

    // Path: extract the original path with `*` restored.
    let pattern_path = parsed_pattern.path().replace(STAR_SENTINEL, "*");
    let resource_path = parsed_resource.path();
    path_matches(&pattern_path, resource_path)
}

fn host_matches(pattern: Option<&str>, resource: Option<&str>) -> bool {
    match (pattern, resource) {
        (Some(p), Some(r)) => host_pattern_matches(p, r),
        (None, None) => true,
        // One has a host, the other doesn't — no match.
        _ => false,
    }
}

fn host_pattern_matches(pattern: &str, host: &str) -> bool {
    // url::Url lowercases hosts; lowercase pattern for symmetry.
    let pattern = pattern.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();

    if pattern == "*" {
        return true;
    }

    // Multi-level: `**.example.com` matches `example.com` and any subdomain
    // depth.
    if let Some(suffix) = pattern.strip_prefix("**.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }

    // Single-level: `*.example.com` matches exactly one extra label.
    if let Some(suffix) = pattern.strip_prefix("*.") {
        let Some(rest) = host.strip_suffix(&format!(".{suffix}")) else {
            return false;
        };
        // `rest` is the matched wildcard label — must be non-empty and
        // contain no `.` (single level only).
        return !rest.is_empty() && !rest.contains('.');
    }

    pattern == host
}

fn path_matches(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        // `/*` matches the prefix followed by `/` and anything else,
        // OR the prefix exactly (so `/api/*` matches `/api` too if you
        // consider the trailing slash implied). Be strict: require the
        // `/` separator.
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if pattern == "*" {
        return true;
    }
    pattern == path
}

/// What a `database.*` op asks of the resource it is authorized against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseOpAccess {
    /// Every listed access is checked on the op's resource — its collection
    /// or table, or the raw-SQL / DDL sentinel for `query_raw`, `exec_raw`
    /// and `ddl` — and all of them must be admitted.
    On(&'static [ResourceAccess]),
    /// `database.batch`: each write is checked on its own collection with
    /// the access [`crate::wire::database::BatchWrite::access`] names.
    PerWrite,
}

/// The access every `database.*` op needs — the ONE classification the
/// database handler authorizes from. A test fails when an op in
/// [`ServiceOp::DATABASE_OPS`] is missing here, so a new op cannot ship
/// unclassified.
///
/// [`ResourceAccess::Append`] is reserved for writes that only ever insert
/// new rows and return nothing about existing ones. That excludes:
/// - `insert_guarded` — its guards count and sum existing rows, and the
///   response names the guard that refused, so it needs `Read` as well;
/// - `upsert` — its conflict branch updates the existing row;
/// - `take_where` — it deletes the rows it returns;
/// - the schema ops — they reshape the table (and additionally need the
///   [`SCHEMA_RESOURCE`] sentinel).
///
/// An insert admitted through `Append` alone is further held, by the
/// database handler, to the append-only insert rules: it may not name `id`,
/// `created_at` or `updated_at` (the server assigns them) nor any column the
/// table lacks (it may not reshape the table) — so a table without all three
/// of those columns refuses every append-only insert. With the id server-assigned,
/// an append-only insert cannot collide with an existing row.
pub const DATABASE_OP_ACCESS: &[(&str, DatabaseOpAccess)] = {
    use DatabaseOpAccess::{On, PerWrite};
    use ResourceAccess::{Append, Read, Write};
    &[
        (ServiceOp::DATABASE_GET, On(&[Read])),
        (ServiceOp::DATABASE_LIST, On(&[Read])),
        (ServiceOp::DATABASE_CREATE, On(&[Append])),
        (ServiceOp::DATABASE_CREATE_MANY, On(&[Append])),
        (ServiceOp::DATABASE_BATCH, PerWrite),
        (ServiceOp::DATABASE_INSERT_GUARDED, On(&[Append, Read])),
        (ServiceOp::DATABASE_UPDATE_GUARDED, On(&[Write])),
        (ServiceOp::DATABASE_UPDATE, On(&[Write])),
        (ServiceOp::DATABASE_UPDATE_WHERE, On(&[Write])),
        (ServiceOp::DATABASE_UPDATE_WHERE_COUNT, On(&[Write])),
        (ServiceOp::DATABASE_DELETE, On(&[Write])),
        (ServiceOp::DATABASE_DELETE_WHERE, On(&[Write])),
        (ServiceOp::DATABASE_DELETE_WHERE_COUNT, On(&[Write])),
        (ServiceOp::DATABASE_TAKE_WHERE, On(&[Write])),
        (ServiceOp::DATABASE_COUNT, On(&[Read])),
        (ServiceOp::DATABASE_SUM, On(&[Read])),
        (ServiceOp::DATABASE_AGGREGATE, On(&[Read])),
        (ServiceOp::DATABASE_INCREMENT_FIELD_WHERE, On(&[Write])),
        (ServiceOp::DATABASE_UPSERT, On(&[Write])),
        (ServiceOp::DATABASE_QUERY_RAW, On(&[Read])),
        (ServiceOp::DATABASE_EXEC_RAW, On(&[Write])),
        (ServiceOp::DATABASE_DDL, On(&[Write])),
        (ServiceOp::DATABASE_ENSURE_TABLE, On(&[Write])),
        (ServiceOp::DATABASE_ADD_COLUMN, On(&[Write])),
        (ServiceOp::DATABASE_DROP_TABLE, On(&[Write])),
        (ServiceOp::DATABASE_TABLE_EXISTS, On(&[Read])),
    ]
};

/// Look `op` up in [`DATABASE_OP_ACCESS`]. `None` for an op the table does
/// not classify — a caller must treat that as a denial.
#[must_use]
pub fn database_op_access(op: &str) -> Option<DatabaseOpAccess> {
    DATABASE_OP_ACCESS
        .iter()
        .find(|(name, _)| *name == op)
        .map(|(_, access)| *access)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ResourceType;

    #[test]
    fn storage_list_all_sentinel_is_admin_only() {
        let admin = "my-org/admin";
        // Admin allowed.
        assert!(check_access(
            Some(admin),
            STORAGE_LIST_ALL_RESOURCE,
            ResourceAccess::Read,
            Some(&ResourceType::Storage),
            &[],
            admin
        )
        .is_ok());
        // A non-admin caller is denied even with a wildcard Storage grant —
        // the sentinel does not fall through to grant matching.
        let grants = vec![ResourceGrant::read("files/block", "*").typed(ResourceType::Storage)];
        assert!(check_access(
            Some("files/block"),
            STORAGE_LIST_ALL_RESOURCE,
            ResourceAccess::Read,
            Some(&ResourceType::Storage),
            &grants,
            admin
        )
        .is_err());
        // Anonymous caller denied.
        assert!(check_access(
            None,
            STORAGE_LIST_ALL_RESOURCE,
            ResourceAccess::Read,
            Some(&ResourceType::Storage),
            &[],
            admin
        )
        .is_err());
    }

    #[test]
    fn vector_is_namespace_based_and_self_admits() {
        let admin = "my-org/admin";
        // Owner self-admits its own index namespace (read and write).
        assert!(check_access(
            Some("my-org/vector"),
            "my_org__vector__docs",
            ResourceAccess::Read,
            Some(&ResourceType::Vector),
            &[],
            admin
        )
        .is_ok());
        assert!(check_access(
            Some("my-org/vector"),
            "my_org__vector__docs",
            ResourceAccess::Write,
            Some(&ResourceType::Vector),
            &[],
            admin
        )
        .is_ok());
        // A different block with no grant is denied.
        assert!(check_access(
            Some("evil/block"),
            "my_org__vector__docs",
            ResourceAccess::Read,
            Some(&ResourceType::Vector),
            &[],
            admin
        )
        .is_err());
        // An unnamespaced index name is denied.
        assert!(check_access(
            Some("evil/block"),
            "pwned",
            ResourceAccess::Read,
            Some(&ResourceType::Vector),
            &[],
            admin
        )
        .is_err());
    }

    #[test]
    fn vector_grant_satisfies_only_vector_requests() {
        let admin = "my-org/admin";
        let grants =
            vec![ResourceGrant::read("reader/block", "my_org__vector__docs")
                .typed(ResourceType::Vector)];
        // Same-type read is allowed via the grant.
        assert!(check_access(
            Some("reader/block"),
            "my_org__vector__docs",
            ResourceAccess::Read,
            Some(&ResourceType::Vector),
            &grants,
            admin
        )
        .is_ok());
        // A Db request for the same name is not satisfied by a Vector grant
        // (and, being cross-namespace, is denied).
        assert!(check_access(
            Some("reader/block"),
            "my_org__vector__docs",
            ResourceAccess::Read,
            Some(&ResourceType::Db),
            &grants,
            admin
        )
        .is_err());
    }

    #[test]
    fn test_resource_owner() {
        assert_eq!(
            resource_owner("my_org__auth__users"),
            Some("my-org/auth".to_string())
        );
        assert_eq!(
            resource_owner("MY_ORG__AUTH__JWT_SECRET"),
            Some("my-org/auth".to_string())
        );
        assert_eq!(
            resource_owner("my_org__admin__roles"),
            Some("my-org/admin".to_string())
        );
        // Not enough __ segments
        assert_eq!(resource_owner("auth_users"), None);
        assert_eq!(resource_owner("simple"), None);
        // IMPRESSPRESS_SHARED has only one __ before the var name
        assert_eq!(resource_owner("WAFER_RUN_SHARED__APP_NAME"), None);
    }

    #[test]
    fn test_resource_prefix() {
        assert_eq!(resource_prefix("my-org/auth"), "my_org__auth__");
        assert_eq!(resource_prefix("wafer-run/web"), "wafer_run__web__");
    }

    #[test]
    fn test_ddl_permissive_for_any_block() {
        let grants = vec![];
        let admin = "my-org/admin";
        // Non-admin block can DDL its own tables (write).
        assert!(check_access(
            Some("my-org/auth"),
            "__ddl__",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Another non-admin block likewise.
        assert!(check_access(
            Some("my-org/files"),
            "__ddl__",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Admin too (sanity).
        assert!(check_access(
            Some(admin),
            "__ddl__",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Anonymous (no caller) is still denied — DDL needs an attributable caller.
        assert!(
            check_access(None, "__ddl__", ResourceAccess::Write, None, &grants, admin).is_err()
        );
    }

    /// `__schema__` follows the SAME rule 1a as `__ddl__` — any attributable
    /// caller, anonymous denied — because the structured schema ops are
    /// scoped a second time by the table-name check in the handler.
    #[test]
    fn test_schema_sentinel_permissive_for_any_attributable_block() {
        let grants = vec![];
        let admin = "my-org/admin";
        assert!(check_access(
            Some("my-org/auth"),
            "__schema__",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
        assert!(check_access(
            Some("my-org/files"),
            "__schema__",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
        assert!(check_access(
            Some(admin),
            "__schema__",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
        let err = check_access(
            None,
            "__schema__",
            ResourceAccess::Write,
            None,
            &grants,
            admin,
        )
        .expect_err("anonymous callers cannot reshape a schema");
        assert!(
            err.message.contains("schema ops"),
            "the denial must name the sentinel it refused, got: {}",
            err.message
        );
    }

    #[test]
    fn traversal_safe_path_accepts_plain_segments_and_refuses_the_rest() {
        assert!(is_traversal_safe_path("site/jhg/a.txt"));
        assert!(is_traversal_safe_path("uploads"));
        assert!(is_traversal_safe_path("@wafer-run/web/public/index.html"));
        assert!(is_traversal_safe_path("__storage_list_all__"));
        // Relative segments — the traversal shape C1 is about.
        assert!(!is_traversal_safe_path("../other"));
        assert!(!is_traversal_safe_path("site/jhg/../../other/secret"));
        assert!(!is_traversal_safe_path("site/./jhg"));
        assert!(!is_traversal_safe_path(".."));
        // Empty segments — leading, trailing, doubled, and the empty path.
        assert!(!is_traversal_safe_path(""));
        assert!(!is_traversal_safe_path("/site"));
        assert!(!is_traversal_safe_path("site/"));
        assert!(!is_traversal_safe_path("site//jhg"));
        // `\` is a separator to a Windows filesystem backend, so a segment
        // carrying it is not a plain name.
        assert!(!is_traversal_safe_path("site/jhg/..\\..\\other"));
        assert!(!is_traversal_safe_path("a\\b"));
    }

    #[test]
    fn test_raw_sql_admin_only() {
        let grants = vec![];
        // Admin can use raw SQL
        assert!(check_access(
            Some("my-org/admin"),
            "__raw_sql__",
            ResourceAccess::Read,
            None,
            &grants,
            "my-org/admin"
        )
        .is_ok());
        // Non-admin cannot
        assert!(check_access(
            Some("my-org/auth"),
            "__raw_sql__",
            ResourceAccess::Read,
            None,
            &grants,
            "my-org/admin"
        )
        .is_err());
        // No caller cannot
        assert!(check_access(
            None,
            "__raw_sql__",
            ResourceAccess::Read,
            None,
            &grants,
            "my-org/admin"
        )
        .is_err());
    }

    #[test]
    fn test_shared_resources() {
        let grants = vec![];
        let admin = "my-org/admin";
        // Any *attributable* block can read shared
        assert!(check_access(
            Some("my-org/auth"),
            "WAFER_RUN_SHARED__APP_NAME",
            ResourceAccess::Read,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Anonymous (no caller_id) cannot read shared — shared config may
        // contain secrets and there's no reason an unauthenticated context
        // should access it.
        assert!(check_access(
            None,
            "WAFER_RUN_SHARED__APP_NAME",
            ResourceAccess::Read,
            None,
            &grants,
            admin
        )
        .is_err());
        // Only admin can write shared
        assert!(check_access(
            Some("my-org/auth"),
            "WAFER_RUN_SHARED__APP_NAME",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_err());
        assert!(check_access(
            Some("my-org/admin"),
            "WAFER_RUN_SHARED__APP_NAME",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
    }

    #[test]
    fn test_own_resource() {
        let grants = vec![];
        let admin = "my-org/admin";
        // Auth block can access its own resources
        assert!(check_access(
            Some("my-org/auth"),
            "my_org__auth__users",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
        // But not another block's resources
        assert!(check_access(
            Some("my-org/auth"),
            "my_org__admin__roles",
            ResourceAccess::Read,
            None,
            &grants,
            admin
        )
        .is_err());
    }

    #[test]
    fn test_admin_full_access() {
        let grants = vec![];
        let admin = "my-org/admin";
        assert!(check_access(
            Some(admin),
            "my_org__auth__users",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
    }

    #[test]
    fn test_grant_matching() {
        let admin = "my-org/admin";
        // Read grant: admin can read auth users
        let grants = vec![ResourceGrant::read("my-org/admin", "my_org__auth__users")];
        assert!(check_access(
            Some("my-org/admin"),
            "my_org__auth__users",
            ResourceAccess::Read,
            None,
            &grants,
            "some-other/admin" // not admin for this test
        )
        .is_ok());
        // Write denied with read-only grant
        assert!(check_access(
            Some("my-org/admin"),
            "my_org__auth__users",
            ResourceAccess::Write,
            None,
            &grants,
            "some-other/admin"
        )
        .is_err());

        // Wildcard grant
        let grants = vec![ResourceGrant::read("my-org/admin", "my_org__auth__*")];
        assert!(check_access(
            Some("my-org/admin"),
            "my_org__auth__users",
            ResourceAccess::Read,
            None,
            &grants,
            "some-other/admin"
        )
        .is_ok());
        assert!(check_access(
            Some("my-org/admin"),
            "my_org__auth__tokens",
            ResourceAccess::Read,
            None,
            &grants,
            "some-other/admin"
        )
        .is_ok());

        // Wildcard grantee
        let grants = vec![ResourceGrant::read("*", "my_org__admin__network_rules")];
        assert!(check_access(
            Some("my-org/files"),
            "my_org__admin__network_rules",
            ResourceAccess::Read,
            None,
            &grants,
            admin
        )
        .is_ok());
    }

    #[test]
    fn test_unnamespaced_denied() {
        let grants = vec![];
        let admin = "my-org/admin";
        // Unnamespaced resource names are denied in strict mode
        assert!(check_access(
            Some("my-org/auth"),
            "auth_users",
            ResourceAccess::Read,
            None,
            &grants,
            admin
        )
        .is_err());
    }

    #[test]
    fn test_grant_matches_resource_glob() {
        // Wildcard all
        assert!(grant_matches_resource("*", "https://example.com"));
        assert!(grant_matches_resource("*", "anything"));

        // Exact match
        assert!(grant_matches_resource(
            "https://api.stripe.com/v1",
            "https://api.stripe.com/v1"
        ));
        assert!(!grant_matches_resource(
            "https://api.stripe.com/v1",
            "https://api.stripe.com/v2"
        ));

        // Trailing wildcard (URL)
        assert!(grant_matches_resource(
            "https://api.stripe.com/*",
            "https://api.stripe.com/v1/charges"
        ));
        assert!(!grant_matches_resource(
            "https://api.stripe.com/*",
            "https://evil.com/api.stripe.com/"
        ));

        // Middle wildcard (subdomain)
        assert!(grant_matches_resource(
            "https://*.example.com/*",
            "https://api.example.com/v1"
        ));
        assert!(!grant_matches_resource(
            "https://*.example.com/*",
            "http://api.example.com/v1"
        ));

        // SEC-007 regression: substring-glob bypasses must be blocked.
        // Attacker-controlled hostnames that *contain* the pattern host
        // (suffix or prefix) must NOT match.
        assert!(
            !grant_matches_resource(
                "https://*.example.com/*",
                "https://example.com.evil.com/bar"
            ),
            "example.com.evil.com must not match *.example.com"
        );
        assert!(
            !grant_matches_resource(
                "https://*.example.com/*",
                "https://evil.example.com.attacker.com/bar"
            ),
            "evil.example.com.attacker.com must not match *.example.com (cited SEC-007 attack)"
        );

        // Single-level subdomain wildcard must NOT match multi-level
        // subdomains. (Multi-level uses `**`.)
        assert!(
            !grant_matches_resource("https://*.example.com/*", "https://foo.bar.example.com/x"),
            "*.example.com must NOT match deeply-nested subdomains"
        );

        // Multi-level subdomain wildcard via `**.example.com`.
        assert!(grant_matches_resource(
            "https://**.example.com/*",
            "https://foo.bar.example.com/x"
        ));
        assert!(grant_matches_resource(
            "https://**.example.com/*",
            "https://example.com/x"
        ));
        assert!(!grant_matches_resource(
            "https://**.example.com/*",
            "https://example.com.evil.com/x"
        ));

        // Plain prefix patterns still work (no wildcard in host).
        assert!(grant_matches_resource(
            "https://api.example.com/*",
            "https://api.example.com/v1/items"
        ));
        assert!(!grant_matches_resource(
            "https://api.example.com/*",
            "https://other.example.com/v1/items"
        ));

        // Exact URL match (no trailing wildcard).
        assert!(grant_matches_resource(
            "https://api.example.com/v1",
            "https://api.example.com/v1"
        ));
        assert!(!grant_matches_resource(
            "https://api.example.com/v1",
            "https://api.example.com/v2"
        ));

        // Invalid URL pattern → no match.
        assert!(!grant_matches_resource(
            "https://[bad-pattern",
            "https://anything.example.com/"
        ));

        // Storage path patterns
        assert!(grant_matches_resource(
            "wafer-run/web/*",
            "wafer-run/web/public"
        ));
        assert!(grant_matches_resource(
            "wafer-run/web/*",
            "wafer-run/web/public/index.html"
        ));
        assert!(!grant_matches_resource(
            "wafer-run/web/*",
            "my-org/files/uploads"
        ));

        // No match
        assert!(!grant_matches_resource(
            "https://safe.com/*",
            "https://evil.com/safe.com/"
        ));

        // Namespace patterns (backward compat)
        assert!(grant_matches_resource(
            "my_org__auth__*",
            "my_org__auth__users"
        ));
        assert!(grant_matches_resource(
            "my_org__auth__*",
            "my_org__auth__tokens"
        ));
    }

    #[test]
    fn test_read_write_grant() {
        let grants = vec![ResourceGrant::read_write(
            "my-org/auth",
            "my_org__admin__user_roles",
        )];
        let admin = "some-other/admin";
        // Read OK
        assert!(check_access(
            Some("my-org/auth"),
            "my_org__admin__user_roles",
            ResourceAccess::Read,
            None,
            &grants,
            admin
        )
        .is_ok());
        // Write OK
        assert!(check_access(
            Some("my-org/auth"),
            "my_org__admin__user_roles",
            ResourceAccess::Write,
            None,
            &grants,
            admin
        )
        .is_ok());
    }

    #[test]
    fn test_network_resource_type() {
        let admin = "my-org/admin";
        let net = Some(&ResourceType::Network);

        // No grants → denied (default deny for network)
        assert!(check_access(
            Some("my-org/products"),
            "https://api.stripe.com/v1/charges",
            ResourceAccess::Read,
            net,
            &[],
            admin
        )
        .is_err());

        // Wildcard grant for all blocks → allowed
        let grants = vec![ResourceGrant::read("*", "*").typed(ResourceType::Network)];
        assert!(check_access(
            Some("my-org/products"),
            "https://api.stripe.com/v1/charges",
            ResourceAccess::Read,
            net,
            &grants,
            admin
        )
        .is_ok());

        // Specific URL grant
        let grants = vec![
            ResourceGrant::read("my-org/products", "https://api.stripe.com/*")
                .typed(ResourceType::Network),
        ];
        assert!(check_access(
            Some("my-org/products"),
            "https://api.stripe.com/v1/charges",
            ResourceAccess::Read,
            net,
            &grants,
            admin
        )
        .is_ok());
        // Different block → denied
        assert!(check_access(
            Some("my-org/auth"),
            "https://api.stripe.com/v1/charges",
            ResourceAccess::Read,
            net,
            &grants,
            admin
        )
        .is_err());
        // Different URL → denied
        assert!(check_access(
            Some("my-org/products"),
            "https://evil.com/steal",
            ResourceAccess::Read,
            net,
            &grants,
            admin
        )
        .is_err());

        // Admin always allowed
        assert!(check_access(
            Some(admin),
            "https://anything.com",
            ResourceAccess::Read,
            net,
            &[],
            admin
        )
        .is_ok());

        // Network grant doesn't satisfy Db request
        let grants = vec![ResourceGrant::read("*", "*").typed(ResourceType::Network)];
        assert!(check_access(
            Some("my-org/auth"),
            "auth_users",
            ResourceAccess::Read,
            None, // untyped / Db
            &grants,
            admin
        )
        .is_err());
    }

    #[test]
    fn test_crypto_resource_type() {
        let admin = "my-org/admin";
        let crypto = Some(&ResourceType::Crypto);

        // No grants → denied (default deny for crypto, like network/storage)
        assert!(check_access(
            Some("my-org/auth"),
            "sign",
            ResourceAccess::Read,
            crypto,
            &[],
            admin
        )
        .is_err());

        // Wildcard grant for all blocks on all crypto ops → allowed
        let grants = vec![ResourceGrant::read("*", "*").typed(ResourceType::Crypto)];
        assert!(check_access(
            Some("my-org/auth"),
            "sign",
            ResourceAccess::Read,
            crypto,
            &grants,
            admin
        )
        .is_ok());
        assert!(check_access(
            Some("my-org/auth"),
            "random_bytes",
            ResourceAccess::Read,
            crypto,
            &grants,
            admin
        )
        .is_ok());

        // Operation-specific grant: only random_bytes, not sign
        let grants =
            vec![ResourceGrant::read("my-org/auth", "random_bytes").typed(ResourceType::Crypto)];
        assert!(check_access(
            Some("my-org/auth"),
            "random_bytes",
            ResourceAccess::Read,
            crypto,
            &grants,
            admin
        )
        .is_ok());
        assert!(check_access(
            Some("my-org/auth"),
            "sign",
            ResourceAccess::Read,
            crypto,
            &grants,
            admin
        )
        .is_err());

        // Admin always allowed
        assert!(check_access(
            Some(admin),
            "sign",
            ResourceAccess::Read,
            crypto,
            &[],
            admin
        )
        .is_ok());

        // Crypto grant doesn't satisfy a Db request — the typed match fails
        // and there's no namespace fallback for an unnamespaced resource like
        // "sign" / "random_bytes".
        let grants = vec![ResourceGrant::read("*", "*").typed(ResourceType::Crypto)];
        assert!(check_access(
            Some("my-org/auth"),
            "sign",
            ResourceAccess::Read,
            None, // untyped / Db
            &grants,
            admin
        )
        .is_err());
    }

    #[test]
    fn test_storage_resource_type() {
        let admin = "my-org/admin";
        let storage = Some(&ResourceType::Storage);
        let check = |caller: Option<&str>, resource: &str, access, grants: &[ResourceGrant]| {
            check_access(caller, resource, access, storage, grants, admin)
        };

        // Another block's path, no grant → denied, for reads and writes.
        assert!(check(
            Some("my-org/files"),
            "wafer-run/web/public",
            ResourceAccess::Read,
            &[]
        )
        .is_err());
        assert!(check(
            Some("my-org/files"),
            "wafer-run/web/public/a",
            ResourceAccess::Write,
            &[]
        )
        .is_err());

        // A grant on the path admits exactly the access it names.
        let grants = vec![
            ResourceGrant::read("my-org/files", "wafer-run/web/*").typed(ResourceType::Storage)
        ];
        assert!(check(
            Some("my-org/files"),
            "wafer-run/web/public",
            ResourceAccess::Read,
            &grants
        )
        .is_ok());
        assert!(check(
            Some("my-org/files"),
            "wafer-run/web/public",
            ResourceAccess::Write,
            &grants
        )
        .is_err());

        // The owner reaches its own `{org}/{block}/...` paths, reads and
        // writes, without a grant.
        assert!(check(
            Some("wafer-run/web"),
            "wafer-run/web/public",
            ResourceAccess::Read,
            &[]
        )
        .is_ok());
        assert!(check(
            Some("wafer-run/web"),
            "wafer-run/web/public/index.html",
            ResourceAccess::Write,
            &[]
        )
        .is_ok());
        assert!(check(
            Some("wafer-run/web"),
            "wafer-run/web",
            ResourceAccess::Write,
            &[]
        )
        .is_ok());

        // A path is owned by its first two segments and nothing else: a
        // resource that is not under the caller's namespace is not the
        // caller's, however it is spelled.
        assert!(check(
            Some("wafer-run/web"),
            "my-org/files/photos/a.png",
            ResourceAccess::Read,
            &[]
        )
        .is_err());
        assert!(check(
            Some("wafer-run/web"),
            "wafer-run/webx/a",
            ResourceAccess::Read,
            &[]
        )
        .is_err());
        assert!(check(
            Some("wafer-run/web"),
            "wafer-run",
            ResourceAccess::Read,
            &[]
        )
        .is_err());
        assert!(check(Some("wafer-run/web"), "photos", ResourceAccess::Write, &[]).is_err());

        // `@` is request syntax the storage handler strips; a resource that
        // still carries it names no block, so it is nobody's own.
        assert!(check(
            Some("wafer-run/web"),
            "@wafer-run/web/public",
            ResourceAccess::Read,
            &[]
        )
        .is_err());

        // A traversal shape is refused even when its text starts inside the
        // caller's own namespace, and even for the admin.
        assert!(check(
            Some("wafer-run/web"),
            "wafer-run/web/../../my-org/files/a",
            ResourceAccess::Read,
            &[]
        )
        .is_err());
        assert!(check(
            Some("wafer-run/web"),
            "wafer-run/web//a",
            ResourceAccess::Read,
            &[]
        )
        .is_err());
        assert!(check(Some(admin), "my-org/files/./a", ResourceAccess::Read, &[]).is_err());

        // The admin reaches any well-formed path.
        assert!(check(
            Some(admin),
            "my-org/files/photos/a.png",
            ResourceAccess::Write,
            &[]
        )
        .is_ok());

        // Anonymous caller is denied without a grant.
        assert!(check(None, "my-org/files/photos/a.png", ResourceAccess::Read, &[]).is_err());
    }

    #[test]
    fn test_storage_resource_owner_parser() {
        assert_eq!(
            storage_resource_owner("my-org/files/photos/a.png"),
            Some("my-org/files".to_string())
        );
        // `@` is not path syntax: it stays part of the first segment.
        assert_eq!(
            storage_resource_owner("@wafer-run/web/public/index.html"),
            Some("@wafer-run/web".to_string())
        );
        // Exactly two segments — owner is the whole thing
        assert_eq!(
            storage_resource_owner("foo/bar"),
            Some("foo/bar".to_string())
        );
        // Fewer than two segments returns None
        assert_eq!(storage_resource_owner("just-one-segment"), None);
        assert_eq!(storage_resource_owner(""), None);
        assert_eq!(storage_resource_owner("/leading-slash"), None);
    }

    #[test]
    fn test_typed_resource_owner_dispatch() {
        // Storage → slash parser
        assert_eq!(
            typed_resource_owner("my-org/files/photos/a.png", Some(&ResourceType::Storage)),
            Some("my-org/files".to_string())
        );
        // Db / untyped → __-parser
        assert_eq!(
            typed_resource_owner("my_org__auth__users", Some(&ResourceType::Db)),
            Some("my-org/auth".to_string())
        );
        assert_eq!(
            typed_resource_owner("my_org__auth__users", None),
            Some("my-org/auth".to_string())
        );
        // Mismatched format returns None
        assert_eq!(
            typed_resource_owner("my-org/files/photos", Some(&ResourceType::Db)),
            None
        );
        assert_eq!(
            typed_resource_owner("my_org__auth__users", Some(&ResourceType::Storage)),
            None
        );
    }

    #[test]
    fn database_op_access_classifies_every_database_op_once() {
        for op in ServiceOp::DATABASE_OPS {
            let hits = DATABASE_OP_ACCESS.iter().filter(|(n, _)| n == op).count();
            assert_eq!(
                hits, 1,
                "database op `{op}` is classified {hits} times in DATABASE_OP_ACCESS — \
                 give every op in ServiceOp::DATABASE_OPS exactly one entry"
            );
        }
        for (op, _) in DATABASE_OP_ACCESS {
            assert!(
                ServiceOp::DATABASE_OPS.contains(op),
                "DATABASE_OP_ACCESS classifies `{op}`, which is not in ServiceOp::DATABASE_OPS"
            );
        }
    }

    #[test]
    fn append_is_named_only_by_pure_inserts() {
        // The ops an append-only grant admits alone. A change here widens or
        // narrows what an append-only grantee can do — review it as such.
        let append_alone: Vec<&str> = DATABASE_OP_ACCESS
            .iter()
            .filter(|(_, a)| *a == DatabaseOpAccess::On(&[ResourceAccess::Append]))
            .map(|(op, _)| *op)
            .collect();
        assert_eq!(
            append_alone,
            [ServiceOp::DATABASE_CREATE, ServiceOp::DATABASE_CREATE_MANY]
        );
    }

    #[test]
    fn append_grant_admits_only_append_on_its_collection() {
        let admin = "my-org/admin";
        let grants = vec![ResourceGrant::append(
            "my-org/portal",
            "my_org__admin__audit",
        )];
        let db = Some(&ResourceType::Db);
        let check = |caller, resource, access| {
            check_access(Some(caller), resource, access, db, &grants, admin)
        };
        assert!(check(
            "my-org/portal",
            "my_org__admin__audit",
            ResourceAccess::Append
        )
        .is_ok());
        for denied in [ResourceAccess::Read, ResourceAccess::Write] {
            let err = check("my-org/portal", "my_org__admin__audit", denied).unwrap_err();
            assert_eq!(err.code, ErrorCode::PermissionDenied);
        }
        // Another collection, and another caller, gain nothing.
        assert!(check(
            "my-org/portal",
            "my_org__admin__roles",
            ResourceAccess::Append
        )
        .is_err());
        assert!(check("evil/block", "my_org__admin__audit", ResourceAccess::Append).is_err());
        // The owner keeps full access regardless of the grant.
        assert!(check(
            "my-org/admin",
            "my_org__admin__audit",
            ResourceAccess::Write
        )
        .is_ok());
    }

    #[test]
    fn append_to_shared_resources_is_admin_only() {
        let admin = "my-org/admin";
        let res = "WAFER_RUN_SHARED__APP_NAME";
        assert!(check_access(Some("a/b"), res, ResourceAccess::Append, None, &[], admin).is_err());
        assert!(check_access(Some(admin), res, ResourceAccess::Append, None, &[], admin).is_ok());
    }

    /// `auth.user_profile` answers from the auth service's own authority,
    /// so a caller that is neither the auth block nor the admin needs a
    /// grant on it — and only a grant typed `Auth` (or untyped) admits it.
    #[test]
    fn auth_user_profile_needs_admin_own_or_grant() {
        let admin = "my-org/admin";
        let auth = Some(&ResourceType::Auth);
        let check = |caller: Option<&str>, grants: &[ResourceGrant]| {
            check_access(
                caller,
                AUTH_USER_PROFILE_RESOURCE,
                ResourceAccess::Read,
                auth,
                grants,
                admin,
            )
        };

        assert!(check(Some("my-org/feature"), &[]).is_err());
        assert!(check(None, &[]).is_err());
        assert!(check(Some(admin), &[]).is_ok());
        assert!(
            check(Some("wafer-run/auth"), &[]).is_ok(),
            "the auth block owns its own namespace"
        );

        let granted = [
            ResourceGrant::read("my-org/feature", AUTH_USER_PROFILE_RESOURCE)
                .typed(ResourceType::Auth),
        ];
        assert!(check(Some("my-org/feature"), &granted).is_ok());
        assert!(
            check(Some("my-org/other"), &granted).is_err(),
            "a grant admits its grantee only"
        );

        let db_typed = [
            ResourceGrant::read("my-org/feature", AUTH_USER_PROFILE_RESOURCE)
                .typed(ResourceType::Db),
        ];
        assert!(
            check(Some("my-org/feature"), &db_typed).is_err(),
            "a Db-typed grant does not admit an Auth resource"
        );
    }

    /// The credential ops resolve the credential the caller forwards, so any
    /// attributable caller is admitted without a grant; an anonymous one is
    /// not.
    #[test]
    fn auth_credential_ops_admit_any_attributable_caller() {
        let admin = "my-org/admin";
        for resource in [
            AUTH_REQUIRE_USER_RESOURCE,
            AUTH_REQUIRE_TOKEN_RESOURCE,
            AUTH_REQUIRE_ROLE_RESOURCE,
        ] {
            assert!(is_auth_credential_resource(resource));
            assert!(check_access(
                Some("my-org/feature"),
                resource,
                ResourceAccess::Read,
                Some(&ResourceType::Auth),
                &[],
                admin
            )
            .is_ok());
            assert!(check_access(
                None,
                resource,
                ResourceAccess::Read,
                Some(&ResourceType::Auth),
                &[],
                admin
            )
            .is_err());
            // The rule is the Auth type's: the same name asked as a Db
            // collection takes the ordinary namespace rules.
            assert!(check_access(
                Some("my-org/feature"),
                resource,
                ResourceAccess::Read,
                Some(&ResourceType::Db),
                &[],
                admin
            )
            .is_err());
        }
        assert!(!is_auth_credential_resource(AUTH_USER_PROFILE_RESOURCE));
    }

    #[test]
    fn model_and_op_resources_sit_in_the_serving_blocks_namespace() {
        assert_eq!(
            model_resource("wafer-run/llm", "openai", "meta-llama/Llama-3").unwrap(),
            "wafer_run__llm__openai/meta-llama/Llama-3"
        );
        assert_eq!(
            resource_owner(&model_resource("acme/image-gen", "sd", "xl").unwrap()).as_deref(),
            Some("acme/image-gen")
        );
        assert_eq!(
            op_resource("wafer-run/llm", crate::common::ServiceOp::LLM_LIST_MODELS),
            "wafer_run__llm__list_models"
        );
        assert_eq!(
            op_resource("acme/embedder", crate::common::ServiceOp::EMBEDDING_EMBED),
            "acme__embedder__embed"
        );
        let err = model_resource("wafer-run/llm", "openai/x", "m").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    /// A model resource takes the namespace rules: the serving block and the
    /// admin are admitted, anyone else needs that block's grant, a read
    /// grant admits use (`Read`) but not load/unload (`Write`), and a grant
    /// typed for another service family admits nothing.
    #[test]
    fn model_resource_needs_own_admin_or_a_grant_of_the_right_kind() {
        let admin = "my-org/admin";
        let llm = Some(&ResourceType::Llm);
        let res = model_resource("wafer-run/llm", "openai", "gpt").unwrap();
        let check = |caller: Option<&str>, access, grants: &[ResourceGrant]| {
            check_access(caller, &res, access, llm, grants, admin)
        };

        assert!(check(Some("my-org/chat"), ResourceAccess::Read, &[]).is_err());
        assert!(check(None, ResourceAccess::Read, &[]).is_err());
        assert!(check(Some(admin), ResourceAccess::Write, &[]).is_ok());
        assert!(check(Some("wafer-run/llm"), ResourceAccess::Write, &[]).is_ok());

        let read = [
            ResourceGrant::read("my-org/chat", "wafer_run__llm__openai/*").typed(ResourceType::Llm),
        ];
        assert!(check(Some("my-org/chat"), ResourceAccess::Read, &read).is_ok());
        assert!(
            check(Some("my-org/chat"), ResourceAccess::Write, &read).is_err(),
            "a read grant does not admit loading or unloading a model"
        );
        assert!(check(Some("my-org/other"), ResourceAccess::Read, &read).is_err());

        let image_typed =
            [ResourceGrant::read("my-org/chat", "wafer_run__llm__*").typed(ResourceType::Image)];
        assert!(check(Some("my-org/chat"), ResourceAccess::Read, &image_typed).is_err());
    }
}
