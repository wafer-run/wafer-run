import type { WaferMeta } from './message';
import { WAFER_SERVER_ERROR_CODES, type WaferServerErrorCode } from './error-codes';

/**
 * Codes the client assigns itself when the exchange did not complete:
 * `'timeout'` when sending the request and reading the whole response body
 * outlived the timeout, `'network_error'` when the fetch or the body read
 * failed or the caller's signal aborted it. Both carry status 0.
 */
export type WaferClientErrorCode = 'network_error' | 'timeout';

/** Every code a `WaferError` can carry. */
export type WaferErrorCode = WaferServerErrorCode | WaferClientErrorCode;

/** Whether `value` is a code a Wafer server sends. */
export function isWaferServerErrorCode(value: string): value is WaferServerErrorCode {
  return (WAFER_SERVER_ERROR_CODES as readonly string[]).includes(value);
}

/**
 * Structured error thrown by WaferClient.
 *
 * Wafer error responses follow `{ "error": "NotFound", "message": "..." }`,
 * plus `"code"` when the block attached an application-level detail code.
 */
export class WaferError extends Error {
  /**
   * The server's `"error"` field (e.g. "NotFound", "Unauthenticated"), or a
   * client code. A non-2xx response without a recognised `"error"` field
   * carries "Internal" when it has none and "Unknown" when it names a code
   * this client does not know; `data` keeps the body either way.
   */
  public readonly code: WaferErrorCode;
  /** HTTP status code, or 0 for network/timeout errors */
  public readonly status: number;
  /** Response meta/headers */
  public readonly meta: WaferMeta;
  /** Parsed response body (if available) */
  public readonly data: unknown;
  /** The body's `"code"` field: the block's application-level detail code (e.g. "auth.invalid_email"), if it set one */
  public readonly detailCode?: string;

  constructor(
    code: WaferErrorCode,
    message: string,
    status: number = 0,
    meta: WaferMeta = {},
    data: unknown = null,
    detailCode?: string,
  ) {
    super(message);
    this.name = 'WaferError';
    this.code = code;
    this.status = status;
    this.meta = meta;
    this.data = data;
    this.detailCode = detailCode;
  }

  /** Check if this error matches a given code. */
  is(code: WaferErrorCode): boolean {
    return this.code === code;
  }
}
