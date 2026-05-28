//! Re-export RuntimeError from wafer-block where the canonical definition lives.
pub use wafer_block::error::{
    AliasError, BlockReferenceError, BlockReferenceSource, GrantValidationError, RuntimeError,
};
