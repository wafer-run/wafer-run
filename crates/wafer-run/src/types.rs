// Re-export all types from wafer-block.
pub use wafer_block::{
    streams::{input::InputStream, output::OutputStream},
    types::{
        BlockConfigKey, BlockEndpoint, ConfigVar, InputType, MetaAccess, ResourceGrant,
        ResourceType,
    },
    AuthLevel, CollectionSchema, ErrorCode, FieldSchema, HttpMethod, IndexSchema, InstanceMode,
    LifecycleEvent, LifecycleType, Message, MetaEntry, RequestAction, WaferError,
};
