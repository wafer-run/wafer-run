import type { WaferConfig } from './types/config';
import type { WaferMessage } from './types/message';
import type { WaferResult } from './types/result';
import { send } from './transport';

/**
 * Options for individual requests.
 */
export interface RequestOptions {
  /** Additional headers for this request */
  headers?: Record<string, string>;
  /** URL query parameters */
  params?: Record<string, string>;
  /** Request timeout in milliseconds (overrides client default) */
  timeout?: number;
  /** AbortSignal for cancellation */
  signal?: AbortSignal;
}

export class WaferClient {
  private config: WaferConfig;

  constructor(config: WaferConfig | string) {
    if (typeof config === 'string') {
      this.config = { url: config };
    } else {
      this.config = { ...config };
    }
    // Strip trailing slash
    this.config.url = this.config.url.replace(/\/$/, '');
  }

  /** Send a raw WaferMessage (advanced usage). */
  async send<T = unknown>(message: WaferMessage): Promise<WaferResult<T>> {
    // Parse kind "METHOD:/path" into method + path
    const colonIdx = message.kind.indexOf(':');
    if (colonIdx === -1) {
      throw new Error(`Invalid message kind "${message.kind}": expected "METHOD:/path"`);
    }
    const method = message.kind.slice(0, colonIdx);
    const path = message.kind.slice(colonIdx + 1);

    const response = await send(this.config, {
      method,
      path,
      body: message.data,
      headers: message.meta,
    });

    return {
      data: response.data as T,
      response,
      ok: response.status >= 200 && response.status < 300,
    };
  }

  /** HTTP GET request. */
  async get<T = unknown>(path: string, options?: RequestOptions): Promise<WaferResult<T>> {
    return this.request<T>('GET', path, undefined, options);
  }

  /** HTTP POST request. */
  async post<T = unknown>(path: string, data?: unknown, options?: RequestOptions): Promise<WaferResult<T>> {
    return this.request<T>('POST', path, data, options);
  }

  /** HTTP PUT request. */
  async put<T = unknown>(path: string, data?: unknown, options?: RequestOptions): Promise<WaferResult<T>> {
    return this.request<T>('PUT', path, data, options);
  }

  /** HTTP PATCH request. */
  async patch<T = unknown>(path: string, data?: unknown, options?: RequestOptions): Promise<WaferResult<T>> {
    return this.request<T>('PATCH', path, data, options);
  }

  /** HTTP DELETE request. */
  async delete<T = unknown>(path: string, options?: RequestOptions): Promise<WaferResult<T>> {
    return this.request<T>('DELETE', path, undefined, options);
  }

  /** Set the API key for subsequent requests. */
  setApiKey(key: string): void {
    this.config.apiKey = key;
  }

  /** Remove the API key. */
  removeApiKey(): void {
    delete this.config.apiKey;
  }

  /** Get a copy of the current configuration. */
  getConfig(): WaferConfig {
    return { ...this.config };
  }

  private async request<T>(
    method: string,
    path: string,
    data?: unknown,
    options?: RequestOptions,
  ): Promise<WaferResult<T>> {
    const response = await send(this.config, {
      method,
      path,
      body: data,
      headers: options?.headers,
      params: options?.params,
      timeout: options?.timeout,
      signal: options?.signal,
    });

    return {
      data: response.data as T,
      response,
      ok: response.status >= 200 && response.status < 300,
    };
  }
}
