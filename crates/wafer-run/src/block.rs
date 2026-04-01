//! Re-exported from wafer-block.
pub use wafer_block::block::*;
// Re-export for backward compatibility (existing code does `use crate::block::{Block, BlockInfo}`)
pub use wafer_block::types::{BlockCategory, BlockInfo, UiRoute};
