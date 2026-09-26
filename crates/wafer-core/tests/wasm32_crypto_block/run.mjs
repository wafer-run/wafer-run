// Drives the crypto block on wasm32 through a CryptoService whose password
// ops complete only after an await point (see src/lib.rs and Cargo.toml),
// and fails unless every call waited for the service and returned its
// answer.
//
// Usage: node run.mjs <fixture.wasm>
import { readFileSync } from "node:fs";

const STEPS = {
  1: "the block answered before the service did",
  2: "the service's answer did not wake the block's call",
  3: "the call was woken but stayed pending",
  4: "the block answered with an error",
  5: "a buffered answer did not complete",
  6: "the block's answer is not the service's",
};

const module = new WebAssembly.Module(readFileSync(process.argv[2]));
// The wasm-bindgen placeholders the dependency graph links are never
// reached from these exports; stub them so an unexpected call fails loudly.
const imports = {};
for (const { module: from, name } of WebAssembly.Module.imports(module)) {
  imports[from] ??= {};
  imports[from][name] = () => {
    throw new Error(`unexpected import call: ${from}.${name}`);
  };
}
const { exports } = new WebAssembly.Instance(module, imports);

let failed = false;
for (const [label, run] of [
  ["crypto.hash", () => exports.hash_awaits_the_service()],
  ["crypto.compare_hash (match)", () => exports.compare_hash_awaits_the_service(1)],
  ["crypto.compare_hash (mismatch)", () => exports.compare_hash_awaits_the_service(0)],
]) {
  const code = run();
  if (code === 0) {
    console.log(`${label}: waited for the service and returned its answer`);
  } else {
    console.error(`${label}: ${STEPS[code] ?? `unknown failure ${code}`}`);
    failed = true;
  }
}
process.exit(failed ? 1 : 0);
