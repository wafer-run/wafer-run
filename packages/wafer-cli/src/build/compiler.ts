import { readdir } from "node:fs/promises";
import { join } from "node:path";
import type { BlockEntry } from "../config/types.js";
import { buildTypeScript } from "./typescript.js";
import { buildGo } from "./go.js";
import { buildRust } from "./rust.js";

type Lang = "typescript" | "go" | "rust";

const ENTRY_POINTS: Record<string, Lang> = {
  "block.ts": "typescript",
  "index.ts": "typescript",
  "index.go": "go",
  "main.go": "go",
  "lib.rs": "rust",
  "index.rs": "rust",
};

async function resolveEntry(source: string, cwd: string): Promise<{ path: string; lang: Lang }> {
  // If source has a file extension, use it directly
  if (source.endsWith(".ts")) return { path: source, lang: "typescript" };
  if (source.endsWith(".go")) return { path: source, lang: "go" };
  if (source.endsWith(".rs")) return { path: source, lang: "rust" };

  // Otherwise, scan folder for entry point
  const dir = join(cwd, source);
  const files = await readdir(dir);
  for (const [name, lang] of Object.entries(ENTRY_POINTS)) {
    if (files.includes(name)) {
      return { path: join(source, name), lang };
    }
  }
  throw new Error(`No entry point found in ${source}. Expected one of: ${Object.keys(ENTRY_POINTS).join(", ")}`);
}

export async function compileBlock(block: BlockEntry, cwd: string): Promise<void> {
  const { path, lang } = await resolveEntry(block.source, cwd);
  const outWasm = `dist/${block.name}.wasm`;

  switch (lang) {
    case "typescript":
      return buildTypeScript(path, outWasm, cwd);
    case "go":
      return buildGo(path, outWasm, cwd);
    case "rust":
      return buildRust(block.name, outWasm, cwd);
  }
}
