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
}

impl std::fmt::Display for ResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db => f.write_str("db"),
            Self::Config => f.write_str("config"),
            Self::Storage => f.write_str("storage"),
            Self::Crypto => f.write_str("crypto"),
            Self::Network => f.write_str("network"),
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
            "unrecognized resource_type `{}` (expected db|config|storage|crypto|network)",
            self.0
        )
    }
}

impl std::error::Error for UnknownResourceType {}

/// A resource access grant declared by a block.
///
/// Blocks can only grant access to resources they own (enforced at startup).
/// The runtime collects all grants and checks them in `call_block()`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResourceGrant {
    /// Block ID that receives this grant, or `"*"` for all blocks.
    pub grantee: String,
    /// Exact resource name or prefix pattern ending with `*`.
    pub resource: String,
    /// If true, the grantee can both read and write. If false, read-only.
    #[serde(default)]
    pub write: bool,
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
            resource_type: None,
        }
    }

    /// Create a read-write grant (all resource types).
    pub fn read_write(grantee: &str, resource: &str) -> Self {
        Self {
            grantee: grantee.to_string(),
            resource: resource.to_string(),
            write: true,
            resource_type: None,
        }
    }

    /// Restrict this grant to a specific resource type.
    pub fn typed(mut self, rt: ResourceType) -> Self {
        self.resource_type = Some(rt);
        self
    }
}

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
}
