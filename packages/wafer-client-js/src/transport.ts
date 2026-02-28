import type { WaferConfig } from './types/config';
import type { WaferResponse } from './types/result';
import { WaferError } from './types/error';

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

  // Timeout via AbortController
  const controller = new AbortController();
  const externalSignal = request.signal;
  let timeoutId: ReturnType<typeof setTimeout> | undefined;

  if (externalSignal?.aborted) {
    controller.abort(externalSignal.reason);
  } else {
    externalSignal?.addEventListener('abort', () => controller.abort(externalSignal.reason), { once: true });
    timeoutId = setTimeout(() => controller.abort('timeout'), timeout);
  }

  const credentials = config.credentials ?? defaultCredentials();

  let res: Response;
  try {
    res = await fetchFn(url, {
      method: request.method,
      headers,
      body,
      signal: controller.signal,
      ...(credentials ? { credentials } : {}),
    });
  } catch (err: unknown) {
    if (timeoutId !== undefined) clearTimeout(timeoutId);

    if (err instanceof DOMException && err.name === 'AbortError') {
      // Distinguish between user abort and timeout
      if (externalSignal?.aborted) {
        throw new WaferError('network_error', 'Request aborted');
      }
      throw new WaferError('timeout', `Request timed out after ${timeout}ms`);
    }
    const message = err instanceof Error ? err.message : 'Network request failed';
    throw new WaferError('network_error', message);
  } finally {
    if (timeoutId !== undefined) clearTimeout(timeoutId);
  }

  const rawData = await res.text();
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
    meta: responseHeaders,
    status: res.status,
    contentType,
    headers: responseHeaders,
  };

  // Throw WaferError for non-2xx responses
  if (!res.ok) {
    // Try to parse Wafer's error format: { "error": "code", "message": "..." }
    let errorCode = 'internal_error';
    let errorMessage = `HTTP ${res.status}`;

    if (data && typeof data === 'object' && data !== null) {
      const body = data as Record<string, unknown>;
      if (typeof body.error === 'string') {
        errorCode = body.error;
      }
      if (typeof body.message === 'string') {
        errorMessage = body.message;
      }
    }

    throw new WaferError(errorCode, errorMessage, res.status, responseHeaders, data);
  }

  return waferResponse;
}
