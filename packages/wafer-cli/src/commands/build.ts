import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { readWaferConfig } from "../config/wafer-json.js";
import type { BlockEntry } from "../config/types.js";
import { compileBlock } from "../build/compiler.js";
import { log } from "../util/logger.js";

/**
 * Build blocks to WASM. When called with no arguments, builds all blocks
 * from wafer.json. When `only` is provided, builds only those blocks.
 */
export async function buildCommand(only?: BlockEntry[]): Promise<void> {
  const cwd = process.cwd();
  const config = await readWaferConfig(cwd);
  const blocks = only ?? config.blocks;

  if (blocks.length === 0) {
    log.warn("No blocks to build");
    return;
  }

  await mkdir(join(cwd, "dist"), { recursive: true });

  log.info(`Building ${blocks.length} block(s)...`);

  const results = await Promise.allSettled(
    blocks.map((block) => compileBlock(block, cwd)),
  );

  let failed = 0;
  for (let i = 0; i < results.length; i++) {
    const result = results[i];
    if (result.status === "rejected") {
      log.error(`Block "${blocks[i].name}" failed: ${result.reason}`);
      failed++;
    }
  }

  if (failed > 0) {
    log.error(`${failed} block(s) failed to build`);
    process.exitCode = 1;
  } else {
    log.success(`All ${blocks.length} block(s) built`);
  }
}
