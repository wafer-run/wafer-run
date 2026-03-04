import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { log } from "../util/logger.js";
import {
  waferJsonTemplate,
  mainFlowTemplate,
  packageJsonTemplate,
  helloBlockTemplate,
  tsconfigTemplate,
  gitignoreTemplate,
} from "../templates/init.js";

export async function initCommand(name: string): Promise<void> {
  const dir = join(process.cwd(), name);

  log.step(`Creating project ${name}...`);

  await mkdir(join(dir, "blocks", "hello"), { recursive: true });
  await mkdir(join(dir, "flows"), { recursive: true });
  await mkdir(join(dir, "dist"), { recursive: true });

  await Promise.all([
    writeFile(join(dir, "wafer.json"), JSON.stringify(waferJsonTemplate(name), null, 2) + "\n"),
    writeFile(join(dir, "package.json"), JSON.stringify(packageJsonTemplate(name), null, 2) + "\n"),
    writeFile(join(dir, "blocks", "hello", "index.ts"), helloBlockTemplate()),
    writeFile(join(dir, "flows", "main.json"), JSON.stringify(mainFlowTemplate(), null, 2) + "\n"),
    writeFile(join(dir, "tsconfig.json"), JSON.stringify(tsconfigTemplate(), null, 2) + "\n"),
    writeFile(join(dir, ".gitignore"), gitignoreTemplate()),
  ]);

  log.success(`Created project ${name}`);
  console.log();
  console.log("  Next steps:");
  console.log(`    cd ${name}`);
  console.log("    npm install");
  console.log("    wafer dev");
  console.log();
}
