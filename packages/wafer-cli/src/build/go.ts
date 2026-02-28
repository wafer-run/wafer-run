import { exec } from "../util/exec.js";
import { log } from "../util/logger.js";
import { join } from "node:path";
import { unlink } from "node:fs/promises";

export async function buildGo(source: string, outWasm: string, cwd: string): Promise<void> {
  log.step(`Compiling ${source} (Go)...`);

  // Try TinyGo first (native wasip2/component support), fall back to
  // standard Go + wasm-tools componentize.
  try {
    await exec("tinygo", [
      "build",
      "-target=wasip2",
      "-o", outWasm,
      ".",
    ], { cwd });
  } catch {
    // Fallback: standard Go WASM build + wasm-tools component new.
    log.step("TinyGo not found, falling back to Go + wasm-tools...");
    const corePath = join(cwd, outWasm.replace(/\.wasm$/, ".core.wasm"));
    await exec("go", ["build", "-o", corePath, "."], {
      cwd,
      env: { ...process.env, GOOS: "wasip1", GOARCH: "wasm" },
    });
    await exec("wasm-tools", [
      "component", "new", corePath,
      "--adapt", "wasi_snapshot_preview1=wasi_snapshot_preview1.reactor.wasm",
      "-o", join(cwd, outWasm),
    ], { cwd });

    // Clean up intermediate core module.
    await unlink(corePath).catch(() => {});
  }

  log.success(`Built ${outWasm}`);
}
