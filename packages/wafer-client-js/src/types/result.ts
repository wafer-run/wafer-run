import type { WaferMeta } from './message';

/**
 * Parsed HTTP response from a Wafer instance.
 */
export interface WaferResponse {
  /** Parsed JSON body (or null if not JSON) */
  data: unknown;
  /** Raw response body as string */
  rawData: string;
  /** Response meta/headers from Wafer */
  meta: WaferMeta;
  /** HTTP status code */
  status: number;
  /** Response Content-Type header */
  contentType: string;
  /** All response headers */
  headers: Record<string, string>;
}

/**
 * Result wrapper returned by all WaferClient methods.
 */
export interface WaferResult<T = unknown> {
  /** Parsed response data, typed as T */
  data: T;
  /** Full response details */
  response: WaferResponse;
  /** Whether the request was successful (2xx status) */
  ok: boolean;
}
