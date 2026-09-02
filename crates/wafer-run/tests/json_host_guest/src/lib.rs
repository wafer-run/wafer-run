//! JSON-codec fixture guest. No crates: this is the smallest guest a
//! dependency-free toolchain (Rubrc) can build, and it proves the host
//! transcodes host-call payloads for `__wafer_host_codec() == 1`.
//!
//! Every request body below is a *static* JSON string and every response is
//! handed back to the host-side test as the raw frame bytes the host wrote
//! into guest memory — the guest never parses JSON, so nothing here depends
//! on `serde_json` (or on anything else). The host-side test does the
//! parsing, which is exactly the compatibility claim this fixture exists to
//! pin down: a std-only guest can drive `database` / `storage` / `config`
//! over the streaming ABI as long as it can *emit* JSON.
//!
//! Operations, selected by the request `kind` (read from the JSON handle
//! frame by substring match — the frame is `[{"kind":"…","meta":[…]}, [bytes]]`):
//!   test.roundtrip — ensure_table, create, get; body = the `get` frame (JSON)
//!   test.storage   — storage.put then storage.get; body = the frames read back
//!   test.config    — config.get; body = the frame
//!   test.error     — ensure_table, then database.get of a missing id;
//!                    body = the take_error JSON
//!   test.attach    — stream_attach; body = "attach=<code>"
//!
//! Any host-call error short-circuits the arm and is returned as the
//! `take_error` JSON body, so the host-side test can assert on
//! `{"code":…,"message":…}` for the denial/not-found cases without a second
//! response channel.
//!
//! Resource naming follows the WRAP convention: this block is registered as
//! `test/json-host-guest`, so it owns `test__json_host_guest__*` tables, the
//! `test/json-host-guest/…` storage namespace and `TEST__JSON_HOST_GUEST__*`
//! config keys. Anything outside those namespaces would need an explicit
//! grant, which this fixture deliberately does not have.

#[link(wasm_import_module = "wafer")]
extern "C" {
    fn __wafer_host_stream_init(name_ptr: i32, name_len: i32, msg_ptr: i32, msg_len: i32) -> i64;
    fn __wafer_host_stream_write_chunk(handle: i64, ptr: i32, len: i32) -> i32;
    fn __wafer_host_stream_attach(handle: i64, ptr: i32, len: i32) -> i32;
    fn __wafer_host_stream_finish(handle: i64) -> i32;
    fn __wafer_host_stream_read_chunk(handle: i64) -> i64;
    fn __wafer_host_stream_take_error(handle: i64) -> i64;
    fn __wafer_host_stream_close(handle: i64);
}

/// The three service blocks this guest calls.
const DATABASE: &str = "wafer-run/database";
const STORAGE: &str = "wafer-run/storage";
const CONFIG: &str = "wafer-run/config";

/// `BlockInfo` as JSON. Only `name`/`version`/`interface`/`summary` are
/// required; every other field defaults, so the capability block below spells
/// out exactly what this guest uses and nothing else (an omitted `Allowlist`
/// deserializes as `None` = deny).
///
/// Note the shape of `storage_folders`: the host authorizes a storage op on
/// `"{folder}/{key}"` and matches the allowlist by EXACT string, so the entry
/// is the full object path, not the folder prefix its field name suggests.
const INFO: &str = r#"{
  "name":"test/json-host-guest","version":"0.0.0","interface":"handler@v1",
  "summary":"JSON host-codec fixture",
  "requires":["wafer-run/database","wafer-run/storage","wafer-run/config"],
  "capabilities":{"collections":{"Only":["test__json_host_guest__notes"]},"ddl":true,
    "storage_folders":{"Only":["test/json-host-guest/a.txt"]},
    "config":{"Only":["TEST__JSON_HOST_GUEST__GREETING"]},
    "callable_blocks":{"Only":["wafer-run/database","wafer-run/storage","wafer-run/config"]}}
}"#;

fn pack(bytes: &[u8]) -> i64 {
    ((bytes.as_ptr() as u32 as i64) << 32) | bytes.len() as i64
}

/// Reinterpret a host-returned `(ptr << 32) | len` as the bytes the host just
/// wrote into linear memory via `__wafer_alloc`. Those allocations are leaked
/// (see `__wafer_alloc`), so the `'static` lifetime is real.
fn unpack(packed: i64) -> &'static [u8] {
    let ptr = (packed >> 32) as u32 as *const u8;
    let len = (packed & 0xffff_ffff) as usize;
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn leak(s: String) -> &'static [u8] {
    Box::leak(s.into_boxed_str()).as_bytes()
}

#[no_mangle]
pub extern "C" fn __wafer_alloc(size: i32) -> i32 {
    let v = vec![0u8; size.max(0) as usize].into_boxed_slice();
    Box::leak(v).as_mut_ptr() as i32
}

/// Negotiate the JSON host-call codec (`wafer_block::abi::HOST_CODEC_JSON`).
#[no_mangle]
pub extern "C" fn __wafer_host_codec() -> i32 {
    1
}

#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    pack(INFO.as_bytes())
}

/// `Result<(), WaferError>::Ok(())` in the v1 (JSON) core ABI.
#[no_mangle]
pub extern "C" fn __wafer_lifecycle(_p: i32, _l: i32) -> i64 {
    pack(br#"{"Ok":null}"#)
}

/// One buffered host call. Returns `(status, concatenated response frames,
/// error json)`.
///
/// `status` is the `stream_finish` return (or the negative `stream_init`
/// sentinel when the call never opened). A host-side failure that happens
/// *after* dispatch — a WRAP denial, a NotFound from the backend — surfaces
/// as a negative `read_chunk`, so the error JSON is the authoritative signal
/// here, not the status.
fn call(target: &str, kind: &str, body: &str) -> (i32, Vec<u8>, Vec<u8>) {
    let msg = format!(r#"{{"kind":"{kind}","meta":[]}}"#);
    unsafe {
        let h = __wafer_host_stream_init(
            target.as_ptr() as i32,
            target.len() as i32,
            msg.as_ptr() as i32,
            msg.len() as i32,
        );
        if h < 0 {
            return (h as i32, Vec::new(), Vec::new());
        }
        if !body.is_empty() {
            __wafer_host_stream_write_chunk(h, body.as_ptr() as i32, body.len() as i32);
        }
        let status = __wafer_host_stream_finish(h);
        let mut frames = Vec::new();
        if status == 0 {
            loop {
                let packed = __wafer_host_stream_read_chunk(h);
                // 0 = end of stream, negative = ErrorCode sentinel (details
                // via take_error below).
                if packed <= 0 {
                    break;
                }
                frames.extend_from_slice(unpack(packed));
            }
        }
        let err_packed = __wafer_host_stream_take_error(h);
        let err = if err_packed > 0 {
            unpack(err_packed).to_vec()
        } else {
            Vec::new()
        };
        __wafer_host_stream_close(h);
        (status, frames, err)
    }
}

/// Build a `GuestResult::Respond` in the v1 (JSON) core ABI. Body bytes are an
/// integer array under JSON — the documented v1 encoding of `serde_bytes`.
fn respond(body: &[u8], content_type: &str) -> i64 {
    let data: Vec<String> = body.iter().map(|b| b.to_string()).collect();
    let out = format!(
        r#"{{"action":"Respond","response":{{"data":[{}],"meta":[{{"key":"resp.content_type","value":"{content_type}"}}]}},"error":null,"message":null}}"#,
        data.join(",")
    );
    pack(leak(out))
}

/// A well-formed `codec::encode(&("a", Attachment { mime: "text/plain",
/// bytes: [104, 105], filename: None }))` — MessagePack, hand-carried as a
/// byte literal because this guest has no encoder. Used by `test.attach` so
/// the refusal it observes is provably the JSON-codec gate rather than an
/// undecodable payload.
const ATTACH_PAYLOAD: [u8; 39] = [
    0x92, 0xa1, 0x61, 0x83, 0xa4, 0x6d, 0x69, 0x6d, 0x65, 0xaa, 0x74, 0x65, 0x78, 0x74, 0x2f, 0x70,
    0x6c, 0x61, 0x69, 0x6e, 0xa5, 0x62, 0x79, 0x74, 0x65, 0x73, 0x92, 0x68, 0x69, 0xa8, 0x66, 0x69,
    0x6c, 0x65, 0x6e, 0x61, 0x6d, 0x65, 0xc0,
];

/// `database.ensure_table` request for the guest's own table.
const NOTES_TABLE: &str = r#"{"table":{"name":"test__json_host_guest__notes","columns":[{"name":"id","kind":"string","primary_key":true},{"name":"body","kind":"text","nullable":true}]}}"#;

const JSON: &str = "application/json";

#[no_mangle]
pub extern "C" fn __wafer_handle(ptr: i32, len: i32) -> i64 {
    let frame = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let text = String::from_utf8_lossy(frame);

    if text.contains("test.roundtrip") {
        let (_, _, e1) = call(DATABASE, "database.ensure_table", NOTES_TABLE);
        if !e1.is_empty() {
            return respond(&e1, JSON);
        }
        let (_, _, e2) = call(
            DATABASE,
            "database.create",
            r#"{"collection":"test__json_host_guest__notes","data":{"id":"n1","body":"hello"}}"#,
        );
        if !e2.is_empty() {
            return respond(&e2, JSON);
        }
        let (_, frames, e3) = call(
            DATABASE,
            "database.get",
            r#"{"collection":"test__json_host_guest__notes","id":"n1"}"#,
        );
        if !e3.is_empty() {
            return respond(&e3, JSON);
        }
        return respond(&frames, JSON);
    }

    if text.contains("test.storage") {
        let (_, _, e1) = call(
            STORAGE,
            "storage.put",
            r#"{"folder":"test/json-host-guest","key":"a.txt","data":[104,105],"content_type":"text/plain"}"#,
        );
        if !e1.is_empty() {
            return respond(&e1, JSON);
        }
        // `storage.get` answers with TWO frames: a MessagePack `ObjectInfo`
        // header and then the object body *verbatim* (raw bytes, not a wire
        // DTO). Only the header transcodes to JSON; the body frame is
        // rejected by the host's frame transcoder, which ends the read loop.
        // The frames returned here are therefore the header alone — see the
        // e2e test for the assertion and the note on the limitation.
        let (_, frames, _) = call(
            STORAGE,
            "storage.get",
            r#"{"folder":"test/json-host-guest","key":"a.txt"}"#,
        );
        return respond(&frames, JSON);
    }

    if text.contains("test.config") {
        let (_, frames, err) = call(
            CONFIG,
            "config.get",
            r#"{"key":"TEST__JSON_HOST_GUEST__GREETING"}"#,
        );
        if !err.is_empty() {
            return respond(&err, JSON);
        }
        return respond(&frames, JSON);
    }

    if text.contains("test.error") {
        // Ensure the table first: a `get` against a table that does not exist
        // is an Internal backend error, not the NotFound this arm is about.
        let (_, _, e1) = call(DATABASE, "database.ensure_table", NOTES_TABLE);
        if !e1.is_empty() {
            return respond(&e1, JSON);
        }
        let (_, _, err) = call(
            DATABASE,
            "database.get",
            r#"{"collection":"test__json_host_guest__notes","id":"missing"}"#,
        );
        return respond(&err, JSON);
    }

    if text.contains("test.attach") {
        // Attachments are a MessagePack-only feature. The payload below is a
        // WELL-FORMED one (see `ATTACH_PAYLOAD`), so a guest on the rmp host
        // codec would get 0 back here — the InvalidArgument the host-side test
        // asserts is specifically the JSON-codec refusal, not a decode failure.
        let msg = r#"{"kind":"config.get","meta":[]}"#;
        let code = unsafe {
            let h = __wafer_host_stream_init(
                CONFIG.as_ptr() as i32,
                CONFIG.len() as i32,
                msg.as_ptr() as i32,
                msg.len() as i32,
            );
            let c = __wafer_host_stream_attach(
                h,
                ATTACH_PAYLOAD.as_ptr() as i32,
                ATTACH_PAYLOAD.len() as i32,
            );
            __wafer_host_stream_close(h);
            c
        };
        return respond(format!("attach={code}").as_bytes(), "text/plain");
    }

    respond(b"unknown", "text/plain")
}
