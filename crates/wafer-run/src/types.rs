// Re-export all types from wafer-block.
pub use wafer_block::types::{
    BlockConfigKey, BlockEndpoint, ConfigVar, InputType, MetaAccess, ResourceGrant, ResourceType,
};
pub use wafer_block::{
    Action, AuthLevel, BlockResult, CollectionSchema, ErrorCode, FieldSchema, HttpMethod,
    IndexSchema, InstanceMode, LifecycleEvent, LifecycleType, Message, MetaEntry, RequestAction,
    Response, Result_, WaferError,
};
