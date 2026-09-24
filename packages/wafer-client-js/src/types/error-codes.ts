// @generated from `wafer_block::ErrorCode` by
// crates/wafer-block/tests/client_js_error_codes.rs. Do not edit; run
// `WAFER_REGENERATE=1 cargo test -p wafer-block --test client_js_error_codes`.

/** Every code a Wafer server sends in an error body's `"error"` field. */
export const WAFER_SERVER_ERROR_CODES = [
  'Ok',
  'Cancelled',
  'Unknown',
  'InvalidArgument',
  'DeadlineExceeded',
  'NotFound',
  'AlreadyExists',
  'PermissionDenied',
  'ResourceExhausted',
  'FailedPrecondition',
  'Aborted',
  'OutOfRange',
  'Unimplemented',
  'Internal',
  'Unavailable',
  'DataLoss',
  'Unauthenticated',
] as const;

/** A code a Wafer server sends in an error body's `"error"` field. */
export type WaferServerErrorCode = (typeof WAFER_SERVER_ERROR_CODES)[number];
