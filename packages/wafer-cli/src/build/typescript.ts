import { existsSync } from "node:fs";
import { join } from "node:path";
import { exec } from "../util/exec.js";
import { log } from "../util/logger.js";

function resolveWitDir(cwd: string): string {
  const candidates = [
    join(cwd, "wit"),
    join(cwd, "node_modules", "wafer-wit", "wit"),
    join(cwd, "..", "wafer-wit", "wit"),
  ];
  for (const candidate of candidates) {
    if (existsSync(candidate)) {
      return candidate;
    }
  }
  return candidates[candidates.length - 1];
}

export async function buildTypeScript(source: string, outWasm: string, cwd: string): Promise<void> {
  log.step(`Compiling ${source} (TypeScript)...`);

  // Step 1: Compile TypeScript to JavaScript.
  const distDir = join(cwd, "dist");
  await exec("npx", ["tsc", "--outDir", distDir], { cwd });

  // Step 2: Componentize the compiled JS into a WASM component.
  // jco reads the WIT definition and wires up host imports / guest exports.
  const jsEntry = join(distDir, source.replace(/\.ts$/, ".js"));
  const witDir = resolveWitDir(cwd);
  await exec("npx", [
    "jco", "componentize", jsEntry,
    "--wit", witDir,
    "--world-name", "wafer-block",
    "-o", join(cwd, outWasm),
  ], { cwd });

  log.success(`Built ${outWasm}`);
}
