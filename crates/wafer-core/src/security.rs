//! SSRF defenses shared by host- and native-side fetchers.
//!
//! The predicates live in the `wafer-net-security` crate (SEC-09) so that
//! both leaf blocks (`wafer-block-network`, via this re-export) and the
//! runtime's registry downloads (`wafer-run`, which does not depend on
//! `wafer-core`) apply the same rules. This module re-exports them to keep
//! the established `wafer_core::security::*` paths working.

pub use wafer_net_security::{is_blocked_ip, is_blocked_ipv4, is_blocked_ipv6, is_blocked_url};
