//! Config variable declarations — [`ConfigVar`], [`InputType`], and the
//! platform-reserved [`SOLOBASE_SHARED_PREFIX`].

/// Reserved prefix for cross-block shared config variables.
///
/// Keys with this prefix are special-cased throughout the runtime:
/// - [`ConfigVar::is_deletable`] returns `false` (admins can't delete them).
/// - WRAP (`wafer_block::wrap::check_access`) treats reads as readable by any
///   attributable caller but writes as admin-only.
/// - [`BlockInfo::validate`] rejects blocks that try to *declare* a key under
///   this prefix (the prefix is owned by the platform, not any one block).
///
/// Same literal everywhere — env vars, D1, and the config API all use this
/// exact string with no translation.
pub const SOLOBASE_SHARED_PREFIX: &str = "SOLOBASE_SHARED__";

/// Input type for config variable UI rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum InputType {
    /// Plain text input.
    #[default]
    Text,
    /// Boolean on/off toggle.
    Toggle,
    /// Password field (masked, treated as sensitive).
    Password,
    /// Color picker.
    Color,
    /// URL field (validated on write).
    Url,
}

/// A configuration variable declared by a block.
///
/// This is the single source of truth for config variable metadata.
/// Validation rules are derived from naming conventions:
/// - Sensitive (masked in API): `input_type == Password`
/// - Can't be emptied: `input_type == Password`
/// - Can't be deleted: key starts with `SOLOBASE_SHARED__`
/// - URL validated on write: `input_type == Url`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigVar {
    /// Full config key (e.g., `SUPPERS_AI__AUTH__JWT_SECRET`).
    pub key: String,
    /// Display label for the admin UI (e.g., "JWT Secret").
    #[serde(default)]
    pub name: String,
    /// What this variable controls.
    #[serde(default)]
    pub description: String,
    /// Default value if not set.
    #[serde(default)]
    pub default: String,
    /// Input type for UI rendering and validation.
    #[serde(default)]
    pub input_type: InputType,
    /// Optional warning shown in the admin UI (e.g., "Changing this invalidates sessions").
    #[serde(default)]
    pub warning: String,
    /// If true, a random value is auto-generated when the variable doesn't exist.
    /// Used for secrets like JWT signing keys and webhook HMAC secrets.
    #[serde(default)]
    pub auto_generate: bool,
    /// If true, this variable is admin-configurable but not required for
    /// block startup. Blocks mark vars `.optional()` when they degrade
    /// gracefully without them (e.g., OAuth credentials — block boots
    /// without them, just disables that provider). The required-config
    /// startup validator skips optional vars.
    #[serde(default)]
    pub optional: bool,
}

impl ConfigVar {
    /// Create a config var with key, description, and default value.
    /// Use builder methods for additional metadata (name, input_type, warning).
    pub fn new(key: &str, description: &str, default: &str) -> Self {
        Self {
            key: key.into(),
            name: String::new(),
            description: description.into(),
            default: default.into(),
            input_type: InputType::Text,
            warning: String::new(),
            auto_generate: false,
            optional: false,
        }
    }

    /// Set the display label for the admin UI.
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.into();
        self
    }

    /// Set the human-readable description.
    pub fn description(mut self, description: &str) -> Self {
        self.description = description.into();
        self
    }

    /// Set the default value used when the variable is not configured.
    pub fn default_value(mut self, default: &str) -> Self {
        self.default = default.into();
        self
    }

    /// Set the UI [`InputType`] (drives masking, validation, widget).
    pub fn input_type(mut self, input_type: InputType) -> Self {
        self.input_type = input_type;
        self
    }

    /// Attach a warning string shown in the admin UI on edit.
    pub fn warning(mut self, warning: &str) -> Self {
        self.warning = warning.into();
        self
    }

    /// Mark this variable as auto-generated if not provided.
    /// On first startup, a random secret is generated and stored.
    pub fn auto_generate(mut self) -> Self {
        self.auto_generate = true;
        self
    }

    /// Mark this variable as optional. See the `optional` field doc.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Whether this variable is sensitive (should be masked in API responses).
    pub fn is_sensitive(&self) -> bool {
        self.input_type == InputType::Password
    }

    /// Whether this variable needs URL validation on write.
    pub fn is_url(&self) -> bool {
        self.input_type == InputType::Url
    }

    /// Whether this variable can be deleted by an admin.
    /// Shared system vars cannot be deleted.
    pub fn is_deletable(&self) -> bool {
        !self.key.starts_with(SOLOBASE_SHARED_PREFIX)
    }

    /// Whether this variable can be set to an empty value.
    /// Sensitive vars (passwords/secrets) cannot be emptied.
    pub fn can_be_empty(&self) -> bool {
        !self.is_sensitive()
    }
}
