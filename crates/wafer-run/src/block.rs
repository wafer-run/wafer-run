//! Re-exported from wafer-block.
pub use wafer_block::block::*;
// Also re-export BlockInfo and AdminUIInfo here for backward compatibility
// (existing code does `use crate::block::{Block, BlockInfo}`)
pub use wafer_block::types::{AdminUIInfo, BlockInfo};
