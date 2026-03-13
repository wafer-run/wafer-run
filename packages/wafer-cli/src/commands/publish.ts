import { readWaferConfig, writeWaferConfig } from "../config/wafer-json.js";
import { buildCommand } from "./build.js";
import { bumpVersion, type BumpType } from "../publish/version.js";
import { createGitHubRelease } from "../publish/github.js";
import { generateInterfaces } from "../publish/interface.js";
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

  const { repo, blocks: publishBlocks } = config.publish;

  // Separate blocks by runtime type:
  // - native-only blocks get .interface.json files
  // - wasm/both blocks get compiled to .wasm
  const wasmBlocks = config.blocks.filter(
    (b) => publishBlocks.includes(b.name) && (!b.runtime || b.runtime === "wasm" || b.runtime === "both"),
  );
  const nativeBlocks = config.blocks.filter(
    (b) => publishBlocks.includes(b.name) && b.runtime === "native",
  );

  // Build WASM blocks only (skip native-only blocks)
  if (wasmBlocks.length > 0) {
    await buildCommand(wasmBlocks);
    if (process.exitCode) return;
  }

  // Generate .interface.json for all blocks with metadata
  const interfacePaths = await generateInterfaces(
    config.blocks,
    publishBlocks,
    config.version,
    cwd,
  );

  // Collect all assets to upload
  const wasmAssets = wasmBlocks.map((b) => `${b.name}.wasm`);
  const allAssets = [...wasmAssets, ...interfacePaths.map((p) => p)];

  if (allAssets.length === 0) {
    log.error("No assets to publish");
    process.exitCode = 1;
    return;
  }

  log.info(
    `Publishing ${wasmBlocks.length} wasm + ${nativeBlocks.length} native block(s) to ${repo}...`,
  );

  await createGitHubReleaseWithAssets(repo, config.version, wasmAssets, interfacePaths, cwd);
  log.success(`Published v${config.version}`);
}

/**
 * Create a GitHub release with both .wasm and .interface.json assets.
 */
async function createGitHubReleaseWithAssets(
  repo: string,
  version: string,
  wasmBlockNames: string[],
  interfacePaths: string[],
  cwd: string,
): Promise<void> {
  // If there are only wasm blocks and no interface files, use the original path
  if (interfacePaths.length === 0) {
    await createGitHubRelease(repo, version, wasmBlockNames, cwd);
    return;
  }

  // Otherwise, build the full asset list with absolute paths
  const { join } = await import("node:path");
  const assets: string[] = [
    ...wasmBlockNames.map((name) => join(cwd, "dist", name)),
    ...interfacePaths,
  ];

  const { exec } = await import("../util/exec.js");
  const tag = `v${version}`;

  try {
    const args = ["release", "create", tag, "--repo", repo, "--title", tag, ...assets];
    await exec("gh", args, { cwd });
    log.success(`Created release ${tag} on ${repo}`);
  } catch (ghErr) {
    log.warn("gh CLI failed, trying GitHub REST API...");

    const token = process.env.GITHUB_TOKEN;
    if (!token) {
      throw new Error(
        `gh CLI failed (${ghErr}) and GITHUB_TOKEN is not set. ` +
        "Set GITHUB_TOKEN or install gh CLI to publish.",
      );
    }

    await createReleaseViaAPI(repo, tag, assets, token);
    log.success(`Created release ${tag} on ${repo} (via API)`);
  }
}

async function createReleaseViaAPI(
  repo: string,
  tag: string,
  assetPaths: string[],
  token: string,
): Promise<void> {
  const { readFile } = await import("node:fs/promises");
  const { basename, extname } = await import("node:path");

  const createRes = await fetch(`https://api.github.com/repos/${repo}/releases`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ tag_name: tag, name: tag }),
  });

  if (!createRes.ok) {
    const text = await createRes.text();
    if (createRes.status === 422) {
      throw new Error(`Release ${tag} already exists on ${repo}. Bump the version and try again.`);
    }
    throw new Error(`GitHub API error ${createRes.status}: ${text}`);
  }

  const release = (await createRes.json()) as { upload_url: string };
  const uploadBase = release.upload_url.replace(/\{[^}]*\}/, "");

  for (const assetPath of assetPaths) {
    const data = await readFile(assetPath);
    const name = basename(assetPath);
    const contentType = name.endsWith(".wasm") ? "application/wasm" : "application/json";

    const uploadRes = await fetch(`${uploadBase}?name=${encodeURIComponent(name)}`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: "application/vnd.github+json",
        "Content-Type": contentType,
      },
      body: data,
    });

    if (!uploadRes.ok) {
      throw new Error(`Failed to upload ${name}: ${uploadRes.status} ${await uploadRes.text()}`);
    }

    log.step(`Uploaded ${name}`);
  }
}
