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
    /// Operations of the auth service (`wafer-run/auth`). Namespace-based
    /// like `Db`: each operation is a resource in the auth block's own
    /// `wafer_run__auth__` namespace (see [`crate::wrap::AUTH_USER_PROFILE_RESOURCE`]),
    /// so only the auth block may grant one.
    Auth,
    /// Models of an LLM service block (`llm.*`). Namespace-based like `Db`:
    /// each model is the resource `{org}__{block}__{backend_id}/{model_id}`
    /// in the serving block's own namespace, and `llm.list_models` is
    /// `{org}__{block}__list_models` (see [`crate::wrap::model_resource`]),
    /// so only that block may grant one.
    Llm,
    /// Models of an image-generation service block (`image.*`). Named like
    /// [`Self::Llm`], in the serving block's namespace.
    Image,
    /// Operations of an embedding service block (`embedding.*`): each op is
    /// the resource `{org}__{block}__{op}` in the serving block's namespace
    /// (see [`crate::wrap::op_resource`]), so only that block may grant one.
    Embedding,
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
            Self::Auth => f.write_str("auth"),
            Self::Llm => f.write_str("llm"),
            Self::Image => f.write_str("image"),
            Self::Embedding => f.write_str("embedding"),
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
            "auth" => Some(Self::Auth),
            "llm" => Some(Self::Llm),
            "image" => Some(Self::Image),
            "embedding" => Some(Self::Embedding),
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
            "unrecognized resource_type `{}` (expected db|config|storage|crypto|network|vector|auth|llm|image|embedding)",
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

/// How much a [`ResourceGrant`] lets its grantee change — the grant's
/// `write` field.
///
/// Encoded on the wire as `false` ([`Self::None`]), `true` ([`Self::Full`])
/// or the string `"append"` ([`Self::Append`]). The first two are the
/// boolean `write` field as it has always been encoded, so every existing
/// grant keeps its encoding and its meaning. `"append"` is deliberately not
/// a boolean: a runtime that predates append-only grants decodes `write` as
/// a `bool`, so it rejects a grant carrying `"append"` — and with it the
/// declaring `BlockInfo` — instead of reading it as a read-only grant and
/// admitting reads the grant never conferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrantWrite {
    /// Read-only: admits [`ResourceAccess::Read`].
    #[default]
    None,
    /// Read-write: admits every [`ResourceAccess`].
    Full,
    /// Append-only: admits [`ResourceAccess::Append`] on database
    /// collections, and nothing else — not even [`ResourceAccess::Read`].
    Append,
}

/// The wire string of [`GrantWrite::Append`].
const GRANT_WRITE_APPEND: &str = "append";

impl serde::Serialize for GrantWrite {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::None => serializer.serialize_bool(false),
            Self::Full => serializer.serialize_bool(true),
            Self::Append => serializer.serialize_str(GRANT_WRITE_APPEND),
        }
    }
}

impl<'de> serde::Deserialize<'de> for GrantWrite {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct GrantWriteVisitor;

        impl serde::de::Visitor<'_> for GrantWriteVisitor {
            type Value = GrantWrite;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "a boolean or the string `{GRANT_WRITE_APPEND}`")
            }

            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<GrantWrite, E> {
                Ok(if v {
                    GrantWrite::Full
                } else {
                    GrantWrite::None
                })
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<GrantWrite, E> {
                if v == GRANT_WRITE_APPEND {
                    Ok(GrantWrite::Append)
                } else {
                    Err(E::invalid_value(serde::de::Unexpected::Str(v), &self))
                }
            }
        }

        deserializer.deserialize_any(GrantWriteVisitor)
    }
}

/// A resource access grant declared by a block.
///
/// Blocks can only grant access to resources they own (enforced at startup).
/// The runtime collects all grants and checks them in `call_block()`.
/// What the grant admits is set by [`Self::write`]; see [`Self::admits`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResourceGrant {
    /// Block ID that receives this grant, or `"*"` for all blocks.
    pub grantee: String,
    /// Exact resource name or prefix pattern ending with `*`.
    pub resource: String,
    /// Read-only, read-write or append-only.
    #[serde(default)]
    pub write: GrantWrite,
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
            write: GrantWrite::None,
            resource_type: None,
        }
    }

    /// Create a read-write grant (all resource types).
    pub fn read_write(grantee: &str, resource: &str) -> Self {
        Self {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write: GrantWrite::Full,
            resource_type: None,
        }
    }

    /// Create an append-only grant on database collections: the grantee may
    /// insert rows (`database.create`, `create_many`, a `Create` inside
    /// `database.batch`) and nothing else — it cannot read, update, delete,
    /// upsert or consume a row, add a column, or choose a row's `id`,
    /// `created_at` or `updated_at`. The collection must have all three of
    /// those columns: a table lacking one refuses every append-only insert.
    /// Pair it with [`Self::read`] on the same resource when the grantee also
    /// needs to read.
    pub fn append(grantee: &str, resource: &str) -> Self {
        Self {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write: GrantWrite::Append,
            resource_type: Some(ResourceType::Db),
        }
    }

    /// Restrict this grant to a specific resource type.
    pub fn typed(mut self, rt: ResourceType) -> Self {
        self.resource_type = Some(rt);
        self
    }

    /// Whether this grant admits a request for `access`. This is the access
    /// half of grant matching; the grantee, resource pattern and resource
    /// type are matched alongside it when [`crate::wrap::check_access`]
    /// looks for a grant.
    ///
    /// A read-only grant admits `Read`; a read-write grant admits every
    /// access; an append-only grant admits `Append` only, and only when it
    /// is typed `Db` — an append grant of any other type admits nothing
    /// (registration rejects it too, see [`Self::check_shape`]).
    #[must_use]
    pub fn admits(&self, access: ResourceAccess) -> bool {
        match self.write {
            GrantWrite::None => access == ResourceAccess::Read,
            GrantWrite::Full => true,
            GrantWrite::Append => {
                access == ResourceAccess::Append && self.resource_type == Some(ResourceType::Db)
            }
        }
    }

    /// Reject a grant the runtime will not install: an append-only grant
    /// not typed `Db` (only a database collection has an insert-only access
    /// to admit). Block registration and `Wafer::add_wrap_grants` both run
    /// it, rejecting a failing grant with
    /// [`crate::error::GrantValidationError`].
    pub fn check_shape(&self) -> Result<(), InvalidGrantShape> {
        if self.write == GrantWrite::Append && self.resource_type != Some(ResourceType::Db) {
            return Err(InvalidGrantShape::AppendNotDb);
        }
        Ok(())
    }
}

/// Error from [`ResourceGrant::check_shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidGrantShape {
    /// An append-only grant not typed `Db`.
    AppendNotDb,
}

impl std::fmt::Display for InvalidGrantShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
    fn auth_variant_round_trips() {
        assert_eq!(ResourceType::Auth.to_string(), "auth");
        assert_eq!(ResourceType::parse("auth"), Some(ResourceType::Auth));
        assert_eq!(
            ResourceType::parse_stored(Some("auth")).unwrap(),
            Some(ResourceType::Auth)
        );
        assert_eq!(
            serde_json::to_value(ResourceType::Auth).unwrap(),
            serde_json::json!("auth")
        );
    }

    #[test]
    fn llm_image_and_embedding_variants_round_trip() {
        for (rt, text) in [
            (ResourceType::Llm, "llm"),
            (ResourceType::Image, "image"),
            (ResourceType::Embedding, "embedding"),
        ] {
            assert_eq!(rt.to_string(), text);
            assert_eq!(ResourceType::parse(text), Some(rt.clone()));
            assert_eq!(
                ResourceType::parse_stored(Some(text)).unwrap(),
                Some(rt.clone())
            );
            assert_eq!(serde_json::to_value(&rt).unwrap(), serde_json::json!(text));
        }
    }

    #[test]
    fn grant_kinds_admit_their_accesses() {
        use ResourceAccess::{Append, Read, Write};
        let admitted = |g: &ResourceGrant| [g.admits(Read), g.admits(Append), g.admits(Write)];
        assert_eq!(
            admitted(&ResourceGrant::read("a/b", "x__y__z")),
            [true, false, false]
        );
        assert_eq!(
            admitted(&ResourceGrant::read_write("a/b", "x__y__z")),
            [true, true, true]
        );
        assert_eq!(
            admitted(&ResourceGrant::append("a/b", "x__y__z")),
            [false, true, false]
        );
        // An append grant not typed `Db` admits nothing, even if it reaches
        // the check without passing `check_shape`.
        let untyped = ResourceGrant {
            resource_type: None,
            ..ResourceGrant::append("a/b", "x__y__z")
        };
        assert_eq!(admitted(&untyped), [false, false, false]);
    }

    #[test]
    fn check_shape_rejects_append_grants_not_typed_db() {
        assert_eq!(
            ResourceGrant::append("a/b", "x__y__z").check_shape(),
            Ok(())
        );
        let untyped = ResourceGrant {
            resource_type: None,
            ..ResourceGrant::append("a/b", "x__y__z")
        };
        assert_eq!(untyped.check_shape(), Err(InvalidGrantShape::AppendNotDb));
        let storage = ResourceGrant::append("a/b", "x/y").typed(ResourceType::Storage);
        assert_eq!(storage.check_shape(), Err(InvalidGrantShape::AppendNotDb));
        assert_eq!(
            ResourceGrant::read_write("a/b", "x/y").check_shape(),
            Ok(())
        );
    }

    #[test]
    fn existing_grants_keep_their_encoding() {
        let rw = serde_json::to_value(ResourceGrant::read_write("a/b", "x__y__z")).unwrap();
        assert_eq!(
            rw,
            serde_json::json!({"grantee": "a/b", "resource": "x__y__z", "write": true})
        );
        let ro = serde_json::to_value(ResourceGrant::read("a/b", "x__y__z")).unwrap();
        assert_eq!(ro["write"], serde_json::json!(false));
        let absent: ResourceGrant =
            serde_json::from_value(serde_json::json!({"grantee": "a/b", "resource": "r"})).unwrap();
        assert_eq!(absent.write, GrantWrite::None);
    }

    /// The `ResourceGrant` shape a runtime that predates append-only grants
    /// decodes: `write` is a `bool`.
    #[derive(Debug, serde::Deserialize)]
    #[expect(dead_code, reason = "decoded only to prove the decode fails")]
    struct PreAppendGrant {
        grantee: String,
        resource: String,
        #[serde(default)]
        write: bool,
        #[serde(default)]
        resource_type: Option<ResourceType>,
    }

    #[test]
    fn append_grants_round_trip_and_older_decoders_reject_them() {
        let append = ResourceGrant::append("a/b", "x__y__z");
        let json = serde_json::to_value(&append).unwrap();
        assert_eq!(json["write"], serde_json::json!("append"));
        let back: ResourceGrant = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.write, GrantWrite::Append);
        assert_eq!(back.resource_type, Some(ResourceType::Db));
        let packed = crate::codec::encode(&append).unwrap();
        let back: ResourceGrant = crate::codec::decode(&packed).unwrap();
        assert_eq!(back.write, GrantWrite::Append);

        // Both BlockInfo encodings a host decodes a guest's grants from.
        assert!(serde_json::from_value::<PreAppendGrant>(json).is_err());
        assert!(crate::codec::decode::<PreAppendGrant>(&packed).is_err());
        // Existing grants still decode on such a runtime.
        let rw = crate::codec::encode(&ResourceGrant::read_write("a/b", "r")).unwrap();
        assert!(crate::codec::decode::<PreAppendGrant>(&rw).is_ok());
    }

    #[test]
    fn grant_write_rejects_other_strings() {
        let bad = serde_json::json!({"grantee": "a/b", "resource": "r", "write": "yes"});
        assert!(serde_json::from_value::<ResourceGrant>(bad).is_err());
    }
}
