//! Runtime capability for querying live flow state.
//!
//! Returned as `Option<&dyn FlowIntrospection>` from
//! [`crate::context::Context::flow_introspection`]. Real runtime impls
//! return `Some(self)`; mock contexts inherit the default `None`.
//!
//! JSON pass-through (not typed views) because the only in-tree consumer
//! (`wafer-block-inspector`) immediately re-serializes to JSON for HTTP
//! responses. Future typed consumers can add wafer-flow as a direct dep.

/// Trait the runtime implements to let blocks introspect live flow state.
pub trait FlowIntrospection: crate::compat::MaybeSend + crate::compat::MaybeSync {
    /// Compact summaries (id, name, version, …). One Value per flow.
    /// Returned in registration order.
    fn flow_infos_json(&self) -> Vec<serde_json::Value>;

    /// Full flow definitions. One Value per flow.
    /// Returned in registration order.
    fn flow_defs_json(&self) -> Vec<serde_json::Value>;
}
