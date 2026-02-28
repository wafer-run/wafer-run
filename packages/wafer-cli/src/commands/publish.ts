import { readWaferConfig, writeWaferConfig } from "../config/wafer-json.js";
import { buildCommand } from "./build.js";
import { bumpVersion, type BumpType } from "../publish/version.js";
import { createGitHubRelease } from "../publish/github.js";
import { log } from "../util/logger.js";

interface PublishOptions {
  bump?: string;
}

export async function publishCommand(opts: PublishOptions): Promise<void> {
  const cwd = process.cwd();
  const config = await readWaferConfig(cwd);

  if (!config.publish) {
    log.error('No "publish" section in wafer.json. Add a publish.repo and publish.blocks config.');
    process.exitCode = 1;
    return;
  }

  // Optional version bump
  if (opts.bump) {
    const bump = opts.bump as BumpType;
    const oldVersion = config.version;
    config.version = bumpVersion(oldVersion, bump);
    await writeWaferConfig(config, cwd);
    log.info(`Bumped version ${oldVersion} → ${config.version}`);
  }

  // Build all blocks
  await buildCommand();
  if (process.exitCode) return;

  // Publish
  const { repo, blocks } = config.publish;
  log.info(`Publishing ${blocks.length} block(s) to ${repo}...`);

  await createGitHubRelease(repo, config.version, blocks, cwd);
  log.success(`Published v${config.version}`);
}
