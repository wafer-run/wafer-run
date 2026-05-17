//! Shared enums used by both host and block code: stable error codes and the
//! canonical list of cross-block service names + ops referenced by WRAP grants
//! and the dispatcher.

mod error_codes;
pub use error_codes::ErrorCode;
mod service_names;
pub use service_names::{ServiceName, ServiceOp};
