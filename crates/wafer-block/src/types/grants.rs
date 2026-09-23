//! WRAP resource access grants — [`ResourceGrant`] and [`ResourceType`].

/// Resource type for typed grants. `None` matches any type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceType {
    /// Database collections (get, list, create, update, delete)
    Db,
    /// Configuration keys (get, set)
    Config,
    /// Storage folders and files (get, put, delete, list)
    Storage,
    /// Per-block crypto key namespaces (sign, verify)
    Crypto,
    /// Outbound network access (HTTP requests)
    Network,
    /// Vector indexes (create_index, delete_index, upsert, query, delete,
    /// count). Namespace-based like `Db`: the index storage name is
    /// `{org}__{block}__{index}` and its owner self-admits.
    Vector,
}

impl std::fmt::Display for ResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db => f.write_str("db"),
            Self::Config => f.write_str("config"),
            Self::Storage => f.write_str("storage"),
            Self::Crypto => f.write_str("crypto"),
            Self::Network => f.write_str("network"),
            Self::Vector => f.write_str("vector"),
        }
    }
}

impl ResourceType {
    /// Parse a `ResourceType` from its lowercase string form. Returns `None`
    /// for unrecognized values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "db" => Some(Self::Db),
            "config" => Some(Self::Config),
            "storage" => Some(Self::Storage),
            "crypto" => Some(Self::Crypto),
            "network" => Some(Self::Network),
            "vector" => Some(Self::Vector),
            _ => None,
        }
    }

    /// Parse a stored grant `resource_type` column. Absent or empty means
    /// the grant applies to all types (`Ok(None)` — the documented
    /// wildcard); a non-empty unrecognized value is an error so readers can
    /// reject the row instead of silently widening a typo to the all-types
    /// wildcard.
    pub fn parse_stored(value: Option<&str>) -> Result<Option<Self>, UnknownResourceType> {
        match value {
            None | Some("") => Ok(None),
            Some(s) => Self::parse(s)
                .map(Some)
                .ok_or_else(|| UnknownResourceType(s.to_string())),
        }
    }
}

/// Error from [`ResourceType::parse_stored`]: a non-empty stored value that
/// is not a recognized resource type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownResourceType(pub String);

impl std::fmt::Display for UnknownResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unrecognized resource_type `{}` (expected db|config|storage|crypto|network|vector)",
            self.0
        )
    }
}

impl std::error::Error for UnknownResourceType {}

/// What a single resource-access request asks to do — the access the WRAP
/// check ([`crate::wrap::check_access`]) authorizes against the caller's
/// grants.
///
/// The three kinds are not a ladder: a read-only grant admits [`Self::Read`]
/// alone and an append-only grant admits [`Self::Append`] alone, while a
/// read-write grant admits all three. See [`ResourceGrant::admits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceAccess {
    /// Observes the resource without changing it.
    Read,
    /// Adds new rows to a database collection without reading, changing or
    /// removing any existing row (`database.create` / `create_many`, a
    /// `Create` inside `database.batch`).
    Append,
    /// Any other change: updating, overwriting, removing or consuming
    /// existing state, and every write outside a database collection.
    Write,
}

impl std::fmt::Display for ResourceAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Append => f.write_str("append"),
            Self::Write => f.write_str("write"),
        }
    }
}

/// A resource access grant declared by a block.
///
/// Blocks can only grant access to resources they own (enforced at startup).
/// The runtime collects all grants and checks them in `call_block()`.
///
/// A grant is one of three kinds, chosen by `write` and `append`:
///
/// | `write` | `append` | kind                        | admits                  |
/// |---------|----------|-----------------------------|-------------------------|
/// | `false` | `false`  | read-only ([`Self::read`])  | `Read`                  |
/// | `true`  | `false`  | read-write ([`Self::read_write`]) | `Read`, `Append`, `Write` |
/// | `false` | `true`   | append-only ([`Self::append`]) | `Append`             |
/// | `true`  | `true`   | invalid ([`Self::check_shape`]) | nothing             |
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResourceGrant {
    /// Block ID that receives this grant, or `"*"` for all blocks.
    pub grantee: String,
    /// Exact resource name or prefix pattern ending with `*`.
    pub resource: String,
    /// If true, the grantee can both read and write. If false, read-only
    /// (or append-only, when `append` is set).
    #[serde(default)]
    pub write: bool,
    /// If true, the grantee may only insert rows into the matched database
    /// collections — no read, update, delete or upsert. Requires
    /// `write == false` and `resource_type == Some(ResourceType::Db)`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub append: bool,
    /// Resource type this grant applies to. `None` = all types (wildcard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<ResourceType>,
}

impl ResourceGrant {
    /// Create a read-only grant (all resource types).
    pub fn read(grantee: &str, resource: &str) -> Self {
        Self {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write: false,
            append: false,
            resource_type: None,
        }
    }

    /// Create a read-write grant (all resource types).
    pub fn read_write(grantee: &str, resource: &str) -> Self {
        Self {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write: true,
            append: false,
            resource_type: None,
        }
    }

    /// Create an append-only grant on database collections: the grantee may
    /// insert rows (`database.create`, `create_many`, a `Create` inside
    /// `database.batch`) and nothing else — it cannot read, update, delete,
    /// upsert or consume a row. Pair it with [`Self::read`] on the same
    /// resource when the grantee also needs to read.
    pub fn append(grantee: &str, resource: &str) -> Self {
        Self {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write: false,
            append: true,
            resource_type: Some(ResourceType::Db),
        }
    }

    /// Restrict this grant to a specific resource type.
    pub fn typed(mut self, rt: ResourceType) -> Self {
        self.resource_type = Some(rt);
        self
    }

    /// Whether this grant admits a request for `access` — the access half of
    /// grant matching (grantee, resource pattern and resource type are
    /// matched by [`crate::wrap::check_access`]). A grant that fails
    /// [`Self::check_shape`] admits nothing, so a malformed grant that
    /// reaches the check without passing registration fails closed.
    #[must_use]
    pub fn admits(&self, access: ResourceAccess) -> bool {
        match (self.write, self.append) {
            (false, false) => access == ResourceAccess::Read,
            (true, false) => true,
            (false, true) => access == ResourceAccess::Append,
            (true, true) => false,
        }
    }

    /// Reject a grant whose fields contradict each other: `append` together
    /// with `write`, or `append` on anything but a typed `Db` grant (only a
    /// database collection has an insert-only access to admit). Called at
    /// block registration, where a failing grant is rejected with
    /// [`crate::error::GrantValidationError`].
    pub fn check_shape(&self) -> Result<(), InvalidGrantShape> {
        if self.append && self.write {
            return Err(InvalidGrantShape::AppendWithWrite);
        }
        if self.append && self.resource_type != Some(ResourceType::Db) {
            return Err(InvalidGrantShape::AppendNotDb);
        }
        Ok(())
    }
}

/// Error from [`ResourceGrant::check_shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidGrantShape {
    /// `append` and `write` are both set; a grant is either read-write or
    /// append-only.
    AppendWithWrite,
    /// `append` is set on a grant not typed `Db`.
    AppendNotDb,
}

impl std::fmt::Display for InvalidGrantShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AppendWithWrite => f.write_str(
                "`append` and `write` are both set; a grant is either read-write or append-only",
            ),
            Self::AppendNotDb => f.write_str(
                "append-only grants apply to database collections only; type the grant `db`",
            ),
        }
    }
}

impl std::error::Error for InvalidGrantShape {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stored_absent_and_empty_are_wildcard() {
        assert_eq!(ResourceType::parse_stored(None), Ok(None));
        assert_eq!(ResourceType::parse_stored(Some("")), Ok(None));
    }

    #[test]
    fn parse_stored_known_values() {
        assert_eq!(
            ResourceType::parse_stored(Some("db")),
            Ok(Some(ResourceType::Db))
        );
        assert_eq!(
            ResourceType::parse_stored(Some("network")),
            Ok(Some(ResourceType::Network))
        );
    }

    #[test]
    fn parse_stored_rejects_unrecognized() {
        let err = ResourceType::parse_stored(Some("databsae")).unwrap_err();
        assert_eq!(err, UnknownResourceType("databsae".to_string()));
        // Display names the bad value and the accepted set.
        let msg = err.to_string();
        assert!(msg.contains("databsae"));
        assert!(msg.contains("db|config|storage|crypto|network"));
    }

    #[test]
    fn vector_variant_round_trips() {
        assert_eq!(ResourceType::Vector.to_string(), "vector");
        assert_eq!(ResourceType::parse("vector"), Some(ResourceType::Vector));
        assert_eq!(
            ResourceType::parse_stored(Some("vector")).unwrap(),
            Some(ResourceType::Vector)
        );
    }

    #[test]
    fn grant_kinds_admit_their_accesses() {
        use ResourceAccess::{Append, Read, Write};
        let read = ResourceGrant::read("a/b", "x__y__z");
        let rw = ResourceGrant::read_write("a/b", "x__y__z");
        let append = ResourceGrant::append("a/b", "x__y__z");
        assert_eq!(
            [read.admits(Read), read.admits(Append), read.admits(Write)],
            [true, false, false]
        );
        assert_eq!(
            [rw.admits(Read), rw.admits(Append), rw.admits(Write)],
            [true, true, true]
        );
        assert_eq!(
            [
                append.admits(Read),
                append.admits(Append),
                append.admits(Write)
            ],
            [false, true, false]
        );
        let mut both = ResourceGrant::read_write("a/b", "x__y__z").typed(ResourceType::Db);
        both.append = true;
        assert!(![Read, Append, Write].into_iter().any(|a| both.admits(a)));
    }

    #[test]
    fn check_shape_rejects_contradictory_append_grants() {
        assert_eq!(
            ResourceGrant::append("a/b", "x__y__z").check_shape(),
            Ok(())
        );
        let mut both = ResourceGrant::append("a/b", "x__y__z");
        both.write = true;
        assert_eq!(both.check_shape(), Err(InvalidGrantShape::AppendWithWrite));
        let untyped = ResourceGrant {
            resource_type: None,
            ..ResourceGrant::append("a/b", "x__y__z")
        };
        assert_eq!(untyped.check_shape(), Err(InvalidGrantShape::AppendNotDb));
        let storage = ResourceGrant::append("a/b", "x/y").typed(ResourceType::Storage);
        assert_eq!(storage.check_shape(), Err(InvalidGrantShape::AppendNotDb));
    }

    #[test]
    fn append_flag_serializes_only_when_set() {
        // A read or read-write grant serializes exactly as before the
        // `append` field existed, and a payload without it deserializes as
        // not append-only — existing declarations keep their meaning.
        let rw = serde_json::to_value(ResourceGrant::read_write("a/b", "x__y__z")).unwrap();
        assert_eq!(
            rw,
            serde_json::json!({"grantee": "a/b", "resource": "x__y__z", "write": true})
        );
        let legacy: ResourceGrant = serde_json::from_value(
            serde_json::json!({"grantee": "a/b", "resource": "r", "write": true}),
        )
        .unwrap();
        assert!(!legacy.append);
        let append = serde_json::to_value(ResourceGrant::append("a/b", "x__y__z")).unwrap();
        assert_eq!(append["append"], serde_json::json!(true));
        let back: ResourceGrant = serde_json::from_value(append).unwrap();
        assert!(back.append && !back.write);
        assert_eq!(back.resource_type, Some(ResourceType::Db));
    }
}
