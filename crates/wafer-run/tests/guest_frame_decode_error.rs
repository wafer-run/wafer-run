//! A `#[wafer_block]` guest handed a frame it cannot decode answers with an
//! error naming the decode failure, on both `__wafer_handle` and
//! `__wafer_lifecycle`.
//!
//! The host always encodes well-formed frames, so `WasmiBlock` cannot be
//! made to send a bad one. These tests therefore drive the macro-generated
//! exports of the example echo guest (`testdata/echo_block.wasm`, built by
//! `scripts/build-fixtures.sh`) directly through wasmi, writing bytes that
//! are not MessagePack into guest memory, and decode what the guest returns
//! exactly as the host does.

#![cfg(feature = "wasmi")]

use wafer_block::{
    abi::{GuestAction, GuestResult},
    ErrorCode, WaferError,
};
use wasmi::{Engine, Extern, ExternType, Func, Instance, Linker, Module, Store};

const ECHO_WASM: &[u8] = include_bytes!("../testdata/echo_block.wasm");

/// `0xc1` is the one byte MessagePack never uses: no frame starts with it.
const NOT_MESSAGEPACK: &[u8] = &[0xc1, 0xc1, 0xc1];

/// Instantiate the echo guest with every import stubbed to trap. Neither
/// export under test reaches a host import on its decode-failure path, so a
/// trap here would itself be a failure.
fn echo_guest() -> (Store<()>, Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, ECHO_WASM).expect("echo_block.wasm compiles");
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::<()>::new(&engine);
    for import in module.imports() {
        if let ExternType::Func(ty) = import.ty() {
            let name = format!("{}::{}", import.module(), import.name());
            let stub = Func::new(&mut store, ty.clone(), move |_, _, _| {
                Err(wasmi::Error::new(format!(
                    "unexpected host import call: {name}"
                )))
            });
            linker
                .define(import.module(), import.name(), Extern::Func(stub))
                .expect("define import stub");
        }
    }
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate echo guest")
        .start(&mut store)
        .expect("start echo guest");
    (store, instance)
}

/// Copy `bytes` into guest memory, call `export(ptr, len)` and return the
/// bytes of the `(ptr, len)` packet it hands back.
fn call_with_frame(export: &str, bytes: &[u8]) -> Vec<u8> {
    let (mut store, instance) = echo_guest();
    let memory = instance
        .get_memory(&store, "memory")
        .expect("guest exports memory");
    let alloc = instance
        .get_typed_func::<i32, i32>(&store, "__wafer_alloc")
        .expect("guest exports __wafer_alloc");
    let ptr = alloc
        .call(&mut store, bytes.len() as i32)
        .expect("__wafer_alloc");
    memory
        .write(&mut store, ptr as usize, bytes)
        .expect("write frame into guest memory");

    let func = instance
        .get_typed_func::<(i32, i32), i64>(&store, export)
        .unwrap_or_else(|e| panic!("guest exports {export}: {e}"));
    let packed = func
        .call(&mut store, (ptr, bytes.len() as i32))
        .unwrap_or_else(|e| panic!("{export} trapped: {e}"));

    let (out_ptr, out_len) = ((packed >> 32) as usize, (packed & 0xFFFF_FFFF) as usize);
    let mut out = vec![0u8; out_len];
    memory
        .read(&store, out_ptr, &mut out)
        .expect("read result from guest memory");
    out
}

/// The error must say that decoding failed and for which frame type.
fn assert_names_decode_failure(err: &WaferError, frame_type: &str) {
    assert_eq!(err.code, ErrorCode::Internal, "{err}");
    assert!(
        err.message.contains("codec decode error in") && err.message.contains(frame_type),
        "error should name the {frame_type} decode failure; got: {}",
        err.message
    );
}

#[test]
fn handle_answers_an_undecodable_frame_with_the_decode_error() {
    let out = call_with_frame("__wafer_handle", NOT_MESSAGEPACK);
    let result: GuestResult = wafer_block::codec::decode(&out)
        .unwrap_or_else(|e| panic!("the guest must return a GuestResult, got {out:?}: {e}"));
    assert_eq!(result.action, GuestAction::Error);
    let err = result.error.expect("an Error result carries the error");
    assert_names_decode_failure(&err, "CallFrame");
}

#[test]
fn lifecycle_answers_an_undecodable_event_with_the_decode_error() {
    let out = call_with_frame("__wafer_lifecycle", NOT_MESSAGEPACK);
    let result: Result<(), WaferError> = wafer_block::codec::decode(&out)
        .unwrap_or_else(|e| panic!("the guest must return a lifecycle result, got {out:?}: {e}"));
    let err = result.expect_err("an undecodable event is an error");
    assert_names_decode_failure(&err, "LifecycleEvent");
}
