import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { readWaferConfig } from "../config/wafer-json.js";
import { compileBlock } from "../build/compiler.js";
import { log } from "../util/logger.js";

export async function buildCommand(): Promise<void> {
  const cwd = process.cwd();
  const config = await readWaferConfig(cwd);

  if (config.blocks.length === 0) {
    log.warn("No blocks defined in wafer.json");
    return;
  }

  await mkdir(join(cwd, "dist"), { recursive: true });

  log.info(`Building ${config.blocks.length} block(s)...`);

  const results = await Promise.allSettled(
    config.blocks.map((block) => compileBlock(block, cwd)),
  );

  let failed = 0;
  for (let i = 0; i < results.length; i++) {
    const result = results[i];
    if (result.status === "rejected") {
      log.error(`Block "${config.blocks[i].name}" failed: ${result.reason}`);
      failed++;
    }
  }

  if (failed > 0) {
    log.error(`${failed} block(s) failed to build`);
    process.exitCode = 1;
  } else {
    log.success(`All ${config.blocks.length} block(s) built`);
  }
}
