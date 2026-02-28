import { copyFile } from "node:fs/promises";
import { join } from "node:path";
import { exec } from "../util/exec.js";
import { log } from "../util/logger.js";

export async function buildRust(name: string, outWasm: string, cwd: string): Promise<void> {
  log.step(`Compiling ${name} (Rust)...`);

  // Step 1: Build the core WASM module.
  await exec("cargo", ["build", "--target", "wasm32-wasip1", "--release"], { cwd });
  const corePath = join(cwd, "target", "wasm32-wasip1", "release", `${name}.wasm`);

  // Step 2: Convert the core module into a WASM component with WASI adapter.
  const componentPath = join(cwd, outWasm);
  await exec("wasm-tools", [
    "component", "new", corePath,
    "--adapt", "wasi_snapshot_preview1=wasi_snapshot_preview1.reactor.wasm",
    "-o", componentPath,
  ], { cwd });

  log.success(`Built ${outWasm}`);
}
