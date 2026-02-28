import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { WaferConfig } from "./types.js";

const FILENAME = "wafer.json";

export async function readWaferConfig(dir: string = process.cwd()): Promise<WaferConfig> {
  const filepath = join(dir, FILENAME);
  const raw = await readFile(filepath, "utf-8");
  return JSON.parse(raw) as WaferConfig;
}

export async function writeWaferConfig(config: WaferConfig, dir: string = process.cwd()): Promise<void> {
  const filepath = join(dir, FILENAME);
  await writeFile(filepath, JSON.stringify(config, null, 2) + "\n");
}

export function detectLanguage(source: string): "typescript" | "go" | "rust" {
  if (source.endsWith(".ts")) return "typescript";
  if (source.endsWith(".go")) return "go";
  if (source.endsWith(".rs")) return "rust";
  // Folder-based blocks are resolved at compile time by the compiler
  throw new Error(`Cannot detect language from path: ${source}. Use the compiler's resolveEntry for folder-based blocks.`);
}
