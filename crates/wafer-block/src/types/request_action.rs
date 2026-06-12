//! HTTP-level request actions — [`RequestAction`] and its wire constants.

/// HTTP-level request action mapped from method to WAFER semantics.
///
/// The associated `&str` constants ([`Self::RETRIEVE`] etc.) are the
/// single source of truth for the on-the-wire action names. Blocks that
/// emit actions (`http-listener`, `router`) or filter on them
/// (`readonly-guard`) reference these constants instead of duplicating
/// the string literals — same name, same place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestAction {
    /// Read (GET).
    Retrieve,
    /// Create (POST).
    Create,
    /// Mutate (PATCH/PUT).
    Update,
    /// Remove (DELETE).
    Delete,
    /// Custom RPC-style action (POST without CRUD semantics).
    Execute,
}

impl RequestAction {
    /// Wire constant for [`Self::Retrieve`].
    pub const RETRIEVE: &'static str = "retrieve";
    /// Wire constant for [`Self::Create`].
    pub const CREATE: &'static str = "create";
    /// Wire constant for [`Self::Update`].
    pub const UPDATE: &'static str = "update";
    /// Wire constant for [`Self::Delete`].
    pub const DELETE: &'static str = "delete";
    /// Wire constant for [`Self::Execute`].
    pub const EXECUTE: &'static str = "execute";

    /// Return the canonical lowercase string for this action.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Retrieve => Self::RETRIEVE,
            Self::Create => Self::CREATE,
            Self::Update => Self::UPDATE,
            Self::Delete => Self::DELETE,
            Self::Execute => Self::EXECUTE,
        }
    }
}
