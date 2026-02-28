import type { WaferMeta } from './message';

/**
 * Well-known error codes returned by Wafer.
 */
export type WaferErrorCode =
  | 'bad_request'
  | 'unauthorized'
  | 'forbidden'
  | 'not_found'
  | 'method_not_allowed'
  | 'conflict'
  | 'gone'
  | 'unprocessable_entity'
  | 'too_many_requests'
  | 'internal_error'
  | 'not_implemented'
  | 'service_unavailable'
  | 'network_error'
  | 'timeout'
  | (string & {});

/**
 * Structured error thrown by WaferClient.
 *
 * Wafer error responses follow `{ "error": "code", "message": "..." }`.
 */
export class WaferError extends Error {
  /** Error code (e.g. "not_found", "unauthorized") */
  public readonly code: WaferErrorCode;
  /** HTTP status code, or 0 for network/timeout errors */
  public readonly status: number;
  /** Response meta/headers */
  public readonly meta: WaferMeta;
  /** Parsed response body (if available) */
  public readonly data: unknown;

  constructor(code: WaferErrorCode, message: string, status: number = 0, meta: WaferMeta = {}, data: unknown = null) {
    super(message);
    this.name = 'WaferError';
    this.code = code;
    this.status = status;
    this.meta = meta;
    this.data = data;
  }

  /** Check if this error matches a given code. */
  is(code: WaferErrorCode): boolean {
    return this.code === code;
  }
}
