//! Runtime-only types and convenience impls on core types.
//!
//! Core types (`Message`, `MetaEntry`, `ErrorCode`, `WaferError`) are defined
//! in `core_types`. The submodules here add runtime-only types (block
//! metadata, endpoints, config vars, schemas, grants, skills) and ergonomic
//! impls on the core types. Everything is re-exported flat, so
//! `crate::types::X` paths keep working unchanged.

mod block_info;
mod config_var;
mod endpoint;
mod grants;
mod interface_spec;
mod message_ext;
mod request_action;
mod schema;
mod skill;

pub use block_info::{BlockCategory, BlockInfo, BlockInfoError, BlockRuntime};
pub use config_var::{ConfigVar, InputType, SOLOBASE_SHARED_PREFIX};
pub use endpoint::{AuthLevel, BlockEndpoint, HttpMethod};
pub use grants::{ResourceGrant, ResourceType};
pub use interface_spec::{ActionSpec, InterfaceSpec};
pub use message_ext::{hashmap_to_meta, meta_to_hashmap, MetaGet, MetaSet};
pub use request_action::RequestAction;
pub use schema::{CollectionSchema, FieldSchema, IndexSchema};
pub use skill::{ExternalAsset, SkillTool};
