# wafer-client-js

JavaScript/TypeScript client for [Wafer](https://github.com/wafer-run/wafer) instances.

Zero runtime dependencies — uses native `fetch` (browser + Node 18+).

## Installation

```bash
npm install wafer-client-js
```

## Quick Start

```typescript
import { WaferClient } from 'wafer-client-js';

const client = new WaferClient('https://app.example.com');

// GET
const { data } = await client.get('/api/users');

// POST
const result = await client.post('/api/users', { name: 'Alice' });

// With API key
const authed = new WaferClient({ url: 'https://app.example.com', apiKey: 'sk-...' });
const profile = await authed.get('/api/me');
```

## API

### Constructor

```typescript
// String shorthand
const client = new WaferClient('https://app.example.com');

// Full config
const client = new WaferClient({
  url: 'https://app.example.com',
  apiKey: 'sk-...',
  headers: { 'X-Custom': 'value' },
  timeout: 10_000,
  credentials: 'include',
  fetch: customFetchFn,
});
```

### HTTP Methods

All methods return `Promise<WaferResult<T>>`:

```typescript
client.get<T>(path, options?)
client.post<T>(path, data?, options?)
client.put<T>(path, data?, options?)
client.patch<T>(path, data?, options?)
client.delete<T>(path, options?)
```

### Request Options

```typescript
interface RequestOptions {
  headers?: Record<string, string>;
  params?: Record<string, string>;   // URL query parameters
  timeout?: number;                   // Override default timeout
  signal?: AbortSignal;               // Cancellation
}
```

### Raw Message API

For advanced usage, send messages directly using Wafer's internal format:

```typescript
const result = await client.send<MyResponse>({
  kind: 'POST:/api/users',
  data: { name: 'Alice' },
  meta: { 'x-custom': 'value' },
});
```

### Auth Management

```typescript
client.setApiKey('sk-...');
client.removeApiKey();
client.getConfig();
```

### Error Handling

Non-2xx responses throw `WaferError`. Wafer error bodies are
`{ "error": "NotFound", "message": "...", "code": "auth.invalid_email" }`:
`"error"` is the server's error code, spelled as the `ErrorCode` variant on the
wire (`WAFER_SERVER_ERROR_CODES` lists them), and the optional `"code"` is a
block's application-level detail code.

```typescript
import { WaferError } from 'wafer-client-js';

try {
  await client.get('/api/secret');
} catch (err) {
  if (err instanceof WaferError) {
    console.log(err.code);       // "Unauthenticated"
    console.log(err.status);     // 401
    console.log(err.message);    // "Authentication required"
    console.log(err.detailCode); // the body's "code", if the block set one

    if (err.is('NotFound')) {
      // handle not found
    }
  }
}
```

A body without an `"error"` field gives `"Internal"`; an `"error"` this client
does not know gives `"Unknown"` (the body stays in `err.data`). When the
exchange does not complete, `status` is 0 and `code` is `"timeout"` (the
timeout covers sending the request and reading the whole body), `"aborted"`
(the request's `signal` aborted it) or `"network_error"` (the fetch or the body
read failed).

### Meta Constants

Exported constants matching Wafer's internal meta keys:

```typescript
import { META_REQ_ACTION, META_AUTH_USER_ID, META_RESP_STATUS } from 'wafer-client-js';
```

## Types

| Type | Description |
|------|-------------|
| `WaferConfig` | Client configuration |
| `WaferMessage` | Raw message (`{ kind, data?, meta? }`) |
| `WaferResult<T>` | Response wrapper (`{ data, response, ok }`) |
| `WaferResponse` | Full response details |
| `WaferError` | Error with `code`, `status`, `meta`, `data`, `detailCode` |
| `WaferErrorCode` | `WaferServerErrorCode` or `WaferClientErrorCode` |
| `WaferServerErrorCode` | A code the server sends; generated from Rust's `ErrorCode` |
| `WaferClientErrorCode` | `'timeout'`, `'aborted'` or `'network_error'` |
| `WaferMeta` | `Record<string, string>` |
| `RequestOptions` | Per-request options |

## License

MIT
