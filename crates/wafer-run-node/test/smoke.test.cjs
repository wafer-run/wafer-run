// Loads the napi addon that `npm run build-test` builds from the current
// sources into test-build/ (never the checked-in wafer-run.node) and drives
// it the way a Node embedder does. The block
// is the `example/echo` guest (examples/wasmi-block), which
// scripts/build-fixtures.sh builds into crates/wafer-run/testdata/.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');

const { WaferRuntime, validateWaferflow } = require('../test-build/wafer-run.node');

const ECHO_WASM = path.join(__dirname, '../../wafer-run/testdata/echo_block.wasm');

const FLOW = {
  id: 'smoke',
  name: 'Node smoke',
  version: '0.1.0',
  steps: [{ id: 'root', block: 'example/echo' }],
};

function writeFlow(t, flow = FLOW) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'wafer-run-node-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const file = path.join(dir, 'flow.json');
  fs.writeFileSync(file, JSON.stringify(flow));
  return file;
}

test('validateWaferflow accepts a valid flow and names the fault in an invalid one', () => {
  assert.equal(validateWaferflow(JSON.stringify(FLOW)), null);
  const err = validateWaferflow('{"id":');
  assert.equal(typeof err, 'string');
  assert.match(err, /^parse error/);
});

test('register, resolve, run and stop round-trip through the addon', async (t) => {
  const w = new WaferRuntime();
  await w.register('example/echo', ECHO_WASM);
  await w.register('smoke', writeFlow(t));

  assert.equal(await w.hasBlock('example/echo'), true);
  assert.equal(await w.hasBlock('example/missing'), false);
  const flows = JSON.parse(await w.flowsInfo());
  assert.ok(
    flows.some((f) => f.id === 'smoke'),
    `flowsInfo lacks the registered flow: ${JSON.stringify(flows)}`,
  );

  await w.start();
  try {
    const message = { kind: 'smoke.kind', meta: [{ key: 'a', value: '1' }] };
    const out = JSON.parse(await w.run('smoke', JSON.stringify(message)));
    assert.equal(out.action, 'respond', `unexpected terminal: ${JSON.stringify(out)}`);
    const body = JSON.parse(out.body);
    assert.equal(body.echo, true, `not the echo guest's body: ${out.body}`);
    assert.equal(body.kind, 'smoke.kind');
  } finally {
    await w.stop();
  }
});

test('run rejects a message that is not the runtime Message shape', async () => {
  const w = new WaferRuntime();
  await assert.rejects(w.run('smoke', '{"kind":"x"}'), /invalid Message JSON/);
});

test('start after a failed resolve rejects with the same failure', async (t) => {
  const w = new WaferRuntime();
  // The one step names a block nobody registers, so resolving fails.
  await w.register(
    'broken',
    writeFlow(t, { ...FLOW, id: 'broken', steps: [{ id: 'root', block: 'missing' }] }),
  );
  const resolveErr = await w.resolve().then(
    () => assert.fail('resolve must fail'),
    (e) => e,
  );
  await assert.rejects(w.start(), (e) => e.message === resolveErr.message);
});

test('run refuses a runtime that did not seal', async (t) => {
  const message = JSON.stringify({ kind: 'smoke.kind', meta: [] });

  const w = new WaferRuntime();
  await w.register('example/echo', ECHO_WASM);
  await w.register('smoke', writeFlow(t));
  const unsealed = JSON.parse(await w.run('smoke', message));
  assert.equal(unsealed.action, 'error', JSON.stringify(unsealed));
  assert.match(unsealed.error.message, /not sealed/);

  const broken = new WaferRuntime();
  await broken.register(
    'broken',
    writeFlow(t, { ...FLOW, id: 'broken', steps: [{ id: 'root', block: 'missing' }] }),
  );
  await assert.rejects(broken.resolve());
  const failed = JSON.parse(await broken.run('broken', message));
  assert.equal(failed.action, 'error', JSON.stringify(failed));
  assert.match(failed.error.message, /failed to seal/);
});
