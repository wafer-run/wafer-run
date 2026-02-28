wasmtime::component::bindgen!({
    path: "../../wit/wit",
    world: "wafer-block",
    async: false,
});

// Re-export generated modules at shorter paths for convenience.
pub use wafer::block_world::database;
pub use wafer::block_world::storage;
pub use wafer::block_world::crypto;
pub use wafer::block_world::network;
pub use wafer::block_world::logger;
pub use wafer::block_world::config;
pub use wafer::block_world::runtime;
pub use wafer::block_world::types;
