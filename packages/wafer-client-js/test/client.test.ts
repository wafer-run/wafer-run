import { describe, expect, it } from 'vitest';

import { WaferClient } from '../src';

interface Call {
  url: string;
  init: RequestInit;
}

/** A `fetch` that records each request and answers with `response`. */
function recordingFetch(response: () => Response): { fetch: typeof fetch; calls: Call[] } {
  const calls: Call[] = [];
  const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init: init ?? {} });
    return response();
  }) as typeof fetch;
  return { fetch: fetchFn, calls };
}

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

describe('WaferClient', () => {
  it('sends a GET with the API key and query params, and parses the JSON body', async () => {
    const { fetch, calls } = recordingFetch(() => json(200, { id: 7 }));
    const client = new WaferClient({ url: 'https://app.example.com/', apiKey: 'sk-1', fetch });

    const result = await client.get<{ id: number }>('/api/users', { params: { page: '2' } });

    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe('https://app.example.com/api/users?page=2');
    expect(calls[0].init.method).toBe('GET');
    expect(calls[0].init.body).toBeUndefined();
    expect((calls[0].init.headers as Record<string, string>)['Authorization']).toBe('Bearer sk-1');
    expect(result.ok).toBe(true);
    expect(result.data).toEqual({ id: 7 });
    expect(result.response.status).toBe(200);
  });

  it('sends a POST body as JSON with a JSON content type', async () => {
    const { fetch, calls } = recordingFetch(() => json(201, { created: true }));
    const client = new WaferClient({ url: 'https://app.example.com', fetch });

    const result = await client.post('/api/users', { name: 'Alice' });

    expect(calls[0].init.method).toBe('POST');
    expect(calls[0].init.body).toBe('{"name":"Alice"}');
    expect((calls[0].init.headers as Record<string, string>)['Content-Type']).toBe('application/json');
    expect(result.ok).toBe(true);
    expect(result.data).toEqual({ created: true });
  });

  it('routes send() by the METHOD:/path message kind', async () => {
    const { fetch, calls } = recordingFetch(() => json(200, {}));
    const client = new WaferClient({ url: 'https://app.example.com', fetch });

    await client.send({ kind: 'DELETE:/api/users/7', data: undefined, meta: {} });

    expect(calls[0].init.method).toBe('DELETE');
    expect(calls[0].url).toBe('https://app.example.com/api/users/7');
    await expect(client.send({ kind: 'no-colon', data: undefined, meta: {} })).rejects.toThrow(
      /expected "METHOD:\/path"/,
    );
  });
});
