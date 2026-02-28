// Network service — ergonomic wrappers over the WIT-generated host imports.

import type { MetaEntry } from "../types.js";

export interface HttpRequest {
  method: string;
  url: string;
  headers: MetaEntry[];
  body?: Uint8Array;
}

export interface HttpResponse {
  statusCode: number;
  headers: MetaEntry[];
  body: Uint8Array;
}

export declare const network: {
  doRequest(req: HttpRequest): HttpResponse;
};

const encoder = new TextEncoder();

export function doRequest(req: HttpRequest): HttpResponse {
  return network.doRequest(req);
}

export function httpGet(url: string, headers?: Record<string, string>): HttpResponse {
  return network.doRequest({
    method: "GET",
    url,
    headers: headers
      ? Object.entries(headers).map(([key, value]) => ({ key, value }))
      : [],
  });
}

export function httpPostJSON(
  url: string,
  body: unknown,
  headers?: Record<string, string>,
): HttpResponse {
  const allHeaders: MetaEntry[] = [
    { key: "Content-Type", value: "application/json" },
    ...Object.entries(headers ?? {}).map(([key, value]) => ({ key, value })),
  ];
  return network.doRequest({
    method: "POST",
    url,
    headers: allHeaders,
    body: encoder.encode(JSON.stringify(body)),
  });
}
