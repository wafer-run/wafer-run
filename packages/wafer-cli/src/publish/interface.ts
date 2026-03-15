import { writeFile, mkdir } from "node:fs/promises";
import { join } from "node:path";
import type { BlockEntry } from "../config/types.js";
import { log } from "../util/logger.js";

/** Published block manifest (stored as .block.json in GitHub releases). */
interface BlockManifest {
  name: string;
  version: string;
  interface: string;
  summary: string;
  runtime: "native" | "wasm" | "both";
  build?: {
    type: string;
    crate: string;
    git: string;
    register: string;
    feature?: string;
  };
}

/**
 * Generate .block.json manifest files for blocks that have metadata in wafer.json.
 * Returns the list of generated file paths (absolute).
 */
export async function generateInterfaces(
  blocks: BlockEntry[],
  publishBlocks: string[],
  version: string,
  cwd: string,
): Promise<string[]> {
  const distDir = join(cwd, "dist");
  await mkdir(distDir, { recursive: true });

  const paths: string[] = [];

  for (const block of blocks) {
    if (!publishBlocks.includes(block.name)) continue;
    if (!block.interface) continue;

    const manifest: BlockManifest = {
      name: block.name,
      version,
      interface: block.interface,
      summary: block.summary || "",
      runtime: block.runtime || "wasm",
    };

    if (block.build) {
      manifest.build = block.build as BlockManifest["build"];
    }

    const filename = `${block.name}.block.json`;
    const filepath = join(distDir, filename);
    await writeFile(filepath, JSON.stringify(manifest, null, 2) + "\n");
    paths.push(filepath);
    log.step(`Generated ${filename}`);
  }

  return paths;
}
