// Verifies an adversarial sequence of stored argon2id hashes in one wasm32
// instance of this fixture and fails when linear memory grows by more than
// the sum of `ARGON2_MEMORY_CLASSES` (see src/lib.rs and Cargo.toml).
//
// Usage: node measure.mjs <fixture.wasm>
import { readFileSync } from "node:fs";

const MIB = 1024 * 1024;
// `ARGON2_MEMORY_CLASSES` in KiB: 4096 + 19456 + 47104.
const CLASSES_KIB = 4096 + 19456 + 47104;
// Allocator bookkeeping and page rounding around the three buffers, plus the
// fixture's own small allocations: 0.25 MiB measured, 1 MiB allowed.
const SLACK = MIB;
const BOUND = CLASSES_KIB * 1024 + SLACK;

// Sizes chosen to defeat an allocator that serves each request in a fresh
// block: two just under the ceiling in ascending order, then every class
// boundary from both sides, then the ceiling again. After each derivation
// the application keeps a small allocation (`pin_allocation`), so a freed
// buffer cannot be grown in place for the next, larger request.
const SEQUENCE = [
  [46000, 1, 1],
  [47104, 1, 1],
  [4096, 2, 1],
  [20000, 1, 1],
  [19456, 2, 1],
  [3000, 1, 1],
  [4100, 1, 1],
  [46000, 1, 2],
  [47104, 1, 1],
];

const module = new WebAssembly.Module(readFileSync(process.argv[2]));
// The wasm-bindgen placeholders the dependency graph links are never
// reached from `verify_at`; stub them so an unexpected call fails loudly.
const imports = {};
for (const { module: from, name } of WebAssembly.Module.imports(module)) {
  imports[from] ??= {};
  imports[from][name] = () => {
    throw new Error(`unexpected import call: ${from}.${name}`);
  };
}
const { exports } = new WebAssembly.Instance(module, imports);
const size = () => exports.memory.buffer.byteLength;

const base = size();
let failed = false;
for (const [m, t, p] of SEQUENCE) {
  const result = exports.verify_at(m, t, p);
  exports.pin_allocation(4096);
  const growth = size() - base;
  console.log(
    `m=${m} t=${t} p=${p}: result ${result}, growth ${(growth / MIB).toFixed(2)} MiB`,
  );
  if (result !== 0) {
    console.error(`  expected a full derivation ending in a mismatch (0)`);
    failed = true;
  }
}
// Above the ceiling: refused (2) before any memory is taken.
const before = size();
if (exports.verify_at(47105, 1, 1) !== 2 || size() !== before) {
  console.error("m=47105 was not refused without growing memory");
  failed = true;
}
const growth = size() - base;
console.log(
  `total growth ${(growth / MIB).toFixed(2)} MiB, bound ${(BOUND / MIB).toFixed(2)} MiB`,
);
if (growth > BOUND) {
  console.error("argon2 grew wasm32 linear memory past the size-class bound");
  failed = true;
}
process.exit(failed ? 1 : 0);
