import { join } from "node:path";
import { exec } from "../util/exec.js";
import { log } from "../util/logger.js";

export async function buildTypeScript(source: string, outWasm: string, cwd: string): Promise<void> {
  log.step(`Compiling ${source} (TypeScript)...`);

  // Step 1: Compile TypeScript to JavaScript.
  const distDir = join(cwd, "dist");
  await exec("npx", ["tsc", "--outDir", distDir], { cwd });

  // Step 2: Componentize the compiled JS into a WASM component.
  // jco reads the WIT definition and wires up host imports / guest exports.
  const jsEntry = join(distDir, source.replace(/\.ts$/, ".js"));
  await exec("npx", [
    "jco", "componentize", jsEntry,
    "--wit", join(cwd, "..", "wafer-wit", "wit"),
    "--world-name", "wafer-block",
    "-o", join(cwd, outWasm),
  ], { cwd });

  log.success(`Built ${outWasm}`);
}
