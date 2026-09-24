import { spawnSync } from 'node:child_process';
import { getEventListeners } from 'node:events';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { build } from 'esbuild';

import { afterEach, describe, expect, it } from 'vitest';

import { WAFER_SERVER_ERROR_CODES, WaferClient, WaferError, type WaferErrorCode } from '../src';

/** The error `promise` rejects with; fails the test if it resolves. */
async function rejection(promise: Promise<unknown>): Promise<WaferError> {
  try {
    await promise;
  } catch (err) {
    expect(err).toBeInstanceOf(WaferError);
    return err as WaferError;
  }
  throw new Error('expected the request to reject');
}

/** A real HTTP server on an ephemeral port; `handle` answers each request. */
let server: Server | undefined;
const open: ServerResponse[] = [];

async function serve(handle: (res: ServerResponse, req: IncomingMessage) => void): Promise<string> {
  server = createServer((req, res) => {
    open.push(res);
    handle(res, req);
  });
  await new Promise<void>((resolve) => server!.listen(0, '127.0.0.1', resolve));
  return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
}

afterEach(async () => {
  for (const res of open.splice(0)) res.destroy();
  if (server) {
    server.closeAllConnections();
    await new Promise((resolve) => server!.close(resolve));
    server = undefined;
  }
});

function jsonError(status: number, body: unknown) {
  return (res: ServerResponse) => {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  };
}

describe('error codes', () => {
  it('reads the server code as it is on the wire', async () => {
    const url = await serve(jsonError(404, { error: 'NotFound', message: 'no such user' }));
    const err = await rejection(new WaferClient(url).get('/api/users/7'));

    expect(err.code).toBe('NotFound');
    expect(err.is('NotFound')).toBe(true);
    expect(err.status).toBe(404);
    expect(err.message).toBe('no such user');
    expect(err.detailCode).toBeUndefined();
  });

  it('surfaces the body "code" field as detailCode', async () => {
    const url = await serve(
      jsonError(400, { error: 'InvalidArgument', message: 'bad email', code: 'auth.invalid_email' }),
    );
    const err = await rejection(new WaferClient(url).post('/auth/signup', {}));

    expect(err.code).toBe('InvalidArgument');
    expect(err.detailCode).toBe('auth.invalid_email');
  });

  it('gives Internal to a body without an "error" field', async () => {
    const url = await serve((res) => {
      res.writeHead(502, { 'content-type': 'text/html' });
      res.end('<h1>Bad Gateway</h1>');
    });
    const err = await rejection(new WaferClient(url).get('/'));

    expect(err.code).toBe('Internal');
    expect(err.status).toBe(502);
  });

  it('gives Unknown to an "error" field the client does not know, keeping the body', async () => {
    const url = await serve(jsonError(500, { error: 'not_found', message: 'x' }));
    const err = await rejection(new WaferClient(url).get('/'));

    expect(err.code).toBe('Unknown');
    expect(err.data).toEqual({ error: 'not_found', message: 'x' });
  });

  it('types the union as the wire vocabulary only', () => {
    const wire: WaferErrorCode[] = [...WAFER_SERVER_ERROR_CODES, 'timeout', 'aborted', 'network_error'];
    expect(wire).toContain('NotFound');
    // @ts-expect-error — snake_case is not what a Wafer server sends
    const snake: WaferErrorCode = 'not_found';
    expect(wire).not.toContain(snake);
  });
});

describe('timeout and abort', () => {
  it('times out a body that never finishes', async () => {
    const url = await serve((res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.write('{"partial":');
    });
    const err = await rejection(new WaferClient({ url, timeout: 200 }).get('/slow-body'));

    expect(err.code).toBe('timeout');
    expect(err.status).toBe(0);
  }, 5_000);

  it('times out a response whose headers never arrive', async () => {
    const url = await serve(() => {});
    const err = await rejection(new WaferClient({ url, timeout: 200 }).get('/no-headers'));

    expect(err.code).toBe('timeout');
  }, 5_000);

  it('times out a fetch that ignores the abort signal', async () => {
    const fetch = (() => new Promise<Response>(() => {})) as typeof globalThis.fetch;
    const err = await rejection(
      new WaferClient({ url: 'https://app.example.com', timeout: 50, fetch }).get('/'),
    );

    expect(err.code).toBe('timeout');
  }, 5_000);

  it("reports the caller's abort during the body read as aborted", async () => {
    const url = await serve((res) => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.write('{"partial":');
    });
    const controller = new AbortController();
    const pending = new WaferClient(url).get('/slow-body', { signal: controller.signal });
    setTimeout(() => controller.abort(), 100);
    const err = await rejection(pending);

    expect(err.code).toBe('aborted');
    expect(err.message).toBe('Request aborted');
  }, 5_000);

  it('leaves no abort listener on a reused signal', async () => {
    const url = await serve((res, req) => {
      if (req.url === '/hang') return;
      jsonError(200, {})(res);
    });
    const client = new WaferClient(url);
    const controller = new AbortController();

    for (let i = 0; i < 5; i++) {
      await client.get('/', { signal: controller.signal });
    }
    const err = await rejection(client.get('/hang', { signal: controller.signal, timeout: 50 }));
    expect(err.code).toBe('timeout');

    expect(getEventListeners(controller.signal, 'abort')).toHaveLength(0);
  });

  it('reports a network failure as network_error', async () => {
    const url = await serve(() => {});
    const port = new URL(url).port;
    await new Promise((resolve) => server!.close(resolve));
    server = undefined;
    const err = await rejection(new WaferClient(`http://127.0.0.1:${port}`).get('/'));

    expect(err.code).toBe('network_error');
  });

  it('cancels the body of a fetch that ignores the signal', async () => {
    let cancelled: unknown = 'not cancelled';
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('{"partial":'));
      },
      cancel(reason) {
        cancelled = reason;
      },
    });
    const fetch = (async () => new Response(stream, { status: 200 })) as typeof globalThis.fetch;
    const err = await rejection(
      new WaferClient({ url: 'https://app.example.com', timeout: 50, fetch }).get('/'),
    );

    expect(err.code).toBe('timeout');
    expect(cancelled).toBeInstanceOf(DOMException);
    expect(stream.locked).toBe(false);
  }, 5_000);

  it('cancels the body of a response that arrives after the timeout', async () => {
    let cancelled = false;
    let arrived!: () => void;
    const late = new Promise<void>((resolve) => (arrived = resolve));
    const fetch = (() =>
      new Promise<Response>((resolve) =>
        setTimeout(() => {
          const stream = new ReadableStream<Uint8Array>({
            cancel() {
              cancelled = true;
              arrived();
            },
          });
          resolve(new Response(stream, { status: 200 }));
        }, 100),
      )) as typeof globalThis.fetch;
    const err = await rejection(
      new WaferClient({ url: 'https://app.example.com', timeout: 20, fetch }).get('/'),
    );
    expect(err.code).toBe('timeout');

    await late;
    expect(cancelled).toBe(true);
  }, 5_000);

  // Run in a child `node` so an unhandled rejection takes Node's default
  // action (the process exits non-zero) instead of vitest's handler.
  it('does not leave an unhandled rejection when the signal is already aborted', async () => {
    const bundle = await build({
      entryPoints: [fileURLToPath(new URL('../src/index.ts', import.meta.url))],
      bundle: true,
      format: 'esm',
      platform: 'node',
      write: false,
    });
    const dir = mkdtempSync(join(tmpdir(), 'wafer-client-js-'));
    const client = join(dir, 'client.mjs');
    writeFileSync(client, bundle.outputFiles[0].text);
    const script = `
      const { WaferClient } = await import(${JSON.stringify(pathToFileURL(client).href)});
      const controller = new AbortController();
      controller.abort();
      try {
        await new WaferClient('http://127.0.0.1:9').get('/', { signal: controller.signal });
      } catch (err) {
        console.log('code=' + err.code);
      }
      await new Promise((resolve) => setTimeout(resolve, 200));
      console.log('alive');
    `;
    try {
      const result = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
        encoding: 'utf8',
        timeout: 10_000,
      });

      expect(result.stderr).toBe('');
      expect(result.stdout).toBe('code=aborted\nalive\n');
      expect(result.status).toBe(0);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  }, 15_000);
});
