//! `packages/wafer-client-js/src/types/error-codes.ts` is generated from
//! [`ErrorCode`]: the list holds each variant spelled exactly as serde puts
//! it in an HTTP error body's `"error"` field. This test fails when the
//! committed file differs from what the enum produces; run it with
//! `WAFER_REGENERATE=1` to rewrite the file.

use std::path::PathBuf;

use wafer_block::ErrorCode;

/// Every `ErrorCode` variant, in declaration order.
const EVERY_CODE: [ErrorCode; 17] = [
    ErrorCode::Ok,
    ErrorCode::Cancelled,
    ErrorCode::Unknown,
    ErrorCode::InvalidArgument,
    ErrorCode::DeadlineExceeded,
    ErrorCode::NotFound,
    ErrorCode::AlreadyExists,
    ErrorCode::PermissionDenied,
    ErrorCode::ResourceExhausted,
    ErrorCode::FailedPrecondition,
    ErrorCode::Aborted,
    ErrorCode::OutOfRange,
    ErrorCode::Unimplemented,
    ErrorCode::Internal,
    ErrorCode::Unavailable,
    ErrorCode::DataLoss,
    ErrorCode::Unauthenticated,
];

/// Position of `code` in [`EVERY_CODE`]. The match has no wildcard, so a new
/// variant does not compile until it gets an arm here — and the test below
/// fails until [`EVERY_CODE`] holds it at that position.
fn position(code: ErrorCode) -> usize {
    match code {
        ErrorCode::Ok => 0,
        ErrorCode::Cancelled => 1,
        ErrorCode::Unknown => 2,
        ErrorCode::InvalidArgument => 3,
        ErrorCode::DeadlineExceeded => 4,
        ErrorCode::NotFound => 5,
        ErrorCode::AlreadyExists => 6,
        ErrorCode::PermissionDenied => 7,
        ErrorCode::ResourceExhausted => 8,
        ErrorCode::FailedPrecondition => 9,
        ErrorCode::Aborted => 10,
        ErrorCode::OutOfRange => 11,
        ErrorCode::Unimplemented => 12,
        ErrorCode::Internal => 13,
        ErrorCode::Unavailable => 14,
        ErrorCode::DataLoss => 15,
        ErrorCode::Unauthenticated => 16,
    }
}

fn ts_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/wafer-client-js/src/types/error-codes.ts")
}

fn render_ts() -> String {
    let mut out = String::from(
        "// @generated from `wafer_block::ErrorCode` by\n\
         // crates/wafer-block/tests/client_js_error_codes.rs. Do not edit; run\n\
         // `WAFER_REGENERATE=1 cargo test -p wafer-block --test client_js_error_codes`.\n\
         \n\
         /** Every code a Wafer server sends in an error body's `\"error\"` field. */\n\
         export const WAFER_SERVER_ERROR_CODES = [\n",
    );
    for code in EVERY_CODE {
        // The wire spelling is serde's, not a hand-written name.
        let wire = serde_json::to_value(code).expect("ErrorCode serializes");
        let wire = wire.as_str().expect("ErrorCode serializes to a string");
        out.push_str(&format!("  '{wire}',\n"));
    }
    out.push_str(
        "] as const;\n\
         \n\
         /** A code a Wafer server sends in an error body's `\"error\"` field. */\n\
         export type WaferServerErrorCode = (typeof WAFER_SERVER_ERROR_CODES)[number];\n",
    );
    out
}

#[test]
fn every_code_lists_each_variant_once_in_order() {
    for (i, code) in EVERY_CODE.into_iter().enumerate() {
        assert_eq!(position(code), i, "{code:?} is out of place in EVERY_CODE");
    }
}

#[test]
fn client_js_error_codes_match_the_wire() {
    let path = ts_path();
    let expected = render_ts();
    if std::env::var_os("WAFER_REGENERATE").is_some() {
        std::fs::write(&path, &expected).expect("write error-codes.ts");
        return;
    }
    let committed =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert!(
        committed == expected,
        "{} is stale; run `WAFER_REGENERATE=1 cargo test -p wafer-block --test \
         client_js_error_codes` and commit the result.\n--- expected ---\n{expected}",
        path.display()
    );
}
