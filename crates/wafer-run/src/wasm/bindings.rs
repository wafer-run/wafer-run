wasmtime::component::bindgen!({
    path: "../../wit/wit",
    world: "wafer-block",
    async: false,
});

// Re-export generated modules at shorter paths for convenience.
pub use wafer::block_world::runtime;
pub use wafer::block_world::types;
