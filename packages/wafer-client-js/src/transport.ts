import type { WaferConfig } from './types/config';
import type { WaferResponse } from './types/result';
import { WaferError, isWaferServerErrorCode, type WaferErrorCode } from './types/error';

export interface TransportRequest {
  method: string;
  path: string;
  body?: unknown;
  headers?: Record<string, string>;
  params?: Record<string, string>;
  timeout?: number;
  signal?: AbortSignal;
}

function buildUrl(baseUrl: string, path: string, params?: Record<string, string>): string {
  const url = new URL(path, baseUrl);
  if (params) {
    for (const [key, value] of Object.entries(params)) {
      url.searchParams.set(key, value);
    }
  }
  return url.toString();
}

function parseHeaders(headers: Headers): Record<string, string> {
  const result: Record<string, string> = {};
  headers.forEach((value, key) => {
    result[key] = value;
  });
  return result;
}

/**
 * Settle with `promise`, or reject with the abort reason as soon as `signal`
 * aborts. A fetch implementation need not stop a pending `fetch()` or body
 * read on abort; this makes the timeout and the caller's signal bound both
 * regardless.
 */
function untilAborted<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(signal.reason);
    signal.addEventListener('abort', onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener('abort', onAbort);
        resolve(value);
      },
      (err: unknown) => {
        signal.removeEventListener('abort', onAbort);
        reject(err);
      },
    );
  });
}

function defaultCredentials(): RequestCredentials | undefined {
  // In browser environments, default to 'include' for cookie-based auth.
  // In Node, credentials is not meaningful, so omit it.
  if (typeof window !== 'undefined') {
    return 'include';
  }
  return undefined;
}

export async function send(config: WaferConfig, request: TransportRequest): Promise<WaferResponse> {
  const fetchFn = config.fetch ?? globalThis.fetch;
  const timeout = request.timeout ?? config.timeout ?? 30_000;
  const url = buildUrl(config.url, request.path, request.params);

  // Merge headers: config defaults < per-request
  const headers: Record<string, string> = {
    ...config.headers,
    ...request.headers,
  };

  if (config.apiKey) {
    headers['Authorization'] = `Bearer ${config.apiKey}`;
  }

  // Set Content-Type for requests with a body
  if (request.body !== undefined && !headers['Content-Type'] && !headers['content-type']) {
    headers['Content-Type'] = 'application/json';
  }

  const body = request.body !== undefined ? JSON.stringify(request.body) : undefined;

  // One controller aborts the whole exchange — the fetch and the body read —
  // on the timeout or on the caller's signal. `timedOut` records which one
  // fired: fetch rejects with the abort reason, whatever its shape, so the
  // error itself does not say.
  const controller = new AbortController();
  const externalSignal = request.signal;
  let timedOut = false;
  const onExternalAbort = () => controller.abort(externalSignal?.reason);
  if (externalSignal?.aborted) {
    onExternalAbort();
  } else {
    externalSignal?.addEventListener('abort', onExternalAbort, { once: true });
  }
  const timeoutId = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, timeout);

  const credentials = config.credentials ?? defaultCredentials();

  let res: Response;
  let rawData: string;
  try {
    res = await untilAborted(
      fetchFn(url, {
        method: request.method,
        headers,
        body,
        signal: controller.signal,
        ...(credentials ? { credentials } : {}),
      }),
      controller.signal,
    );
    rawData = await untilAborted(res.text(), controller.signal);
  } catch (err: unknown) {
    if (timedOut) {
      throw new WaferError('timeout', `Request timed out after ${timeout}ms`);
    }
    if (externalSignal?.aborted) {
      throw new WaferError('network_error', 'Request aborted');
    }
    const message = err instanceof Error ? err.message : 'Network request failed';
    throw new WaferError('network_error', message);
  } finally {
    clearTimeout(timeoutId);
    externalSignal?.removeEventListener('abort', onExternalAbort);
  }

  const contentType = res.headers.get('content-type') ?? '';
  const responseHeaders = parseHeaders(res.headers);

  // Parse JSON if content-type indicates it
  let data: unknown = null;
  if (contentType.includes('application/json') && rawData.length > 0) {
    try {
      data = JSON.parse(rawData);
    } catch {
      data = null;
    }
  }

  const waferResponse: WaferResponse = {
    data,
    rawData,
    status: res.status,
    contentType,
    headers: responseHeaders,
  };

  // Throw WaferError for non-2xx responses
  if (!res.ok) {
    // Try to parse Wafer's error format:
    // { "error": "NotFound", "message": "...", "code": "detail code" (optional) }
    let errorCode: WaferErrorCode = 'Internal';
    let errorMessage = `HTTP ${res.status}`;
    let detailCode: string | undefined;

    if (data && typeof data === 'object' && data !== null) {
      const body = data as Record<string, unknown>;
      if (typeof body.error === 'string') {
        errorCode = isWaferServerErrorCode(body.error) ? body.error : 'Unknown';
      }
      if (typeof body.message === 'string') {
        errorMessage = body.message;
      }
      if (typeof body.code === 'string') {
        detailCode = body.code;
      }
    }

    throw new WaferError(errorCode, errorMessage, res.status, responseHeaders, data, detailCode);
  }

  return waferResponse;
}
