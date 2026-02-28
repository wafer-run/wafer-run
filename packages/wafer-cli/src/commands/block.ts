import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { log } from "../util/logger.js";
import { readWaferConfig, writeWaferConfig } from "../config/wafer-json.js";
import { blockTemplate, extensionForLang } from "../templates/block.js";

interface BlockOptions {
  lang?: string;
}

export async function blockCommand(name: string, opts: BlockOptions): Promise<void> {
  const lang = (opts.lang ?? "typescript") as "typescript" | "go" | "rust";
  const ext = extensionForLang(lang);
  const sourcePath = `blocks/${name}`;

  const config = await readWaferConfig();

  const existing = config.blocks.find((b) => b.name === name);
  if (existing) {
    log.error(`Block "${name}" already exists (source: ${existing.source})`);
    process.exitCode = 1;
    return;
  }

  const blockDir = join(process.cwd(), sourcePath);
  await mkdir(blockDir, { recursive: true });
  await writeFile(join(blockDir, `index${ext}`), blockTemplate(name, lang));

  config.blocks.push({ name, source: sourcePath });
  await writeWaferConfig(config);

  log.success(`Created block ${name} at ${sourcePath}/index${ext}`);
}
