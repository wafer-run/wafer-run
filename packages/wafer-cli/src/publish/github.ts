import { join } from "node:path";
import { exec } from "../util/exec.js";
import { log } from "../util/logger.js";

export async function createGitHubRelease(
  repo: string,
  version: string,
  wasmFiles: string[],
  cwd: string,
): Promise<void> {
  const tag = `v${version}`;
  const assetPaths = wasmFiles.map((f) => join(cwd, "dist", `${f}.wasm`));

  try {
    // Try gh CLI first
    const args = ["release", "create", tag, "--repo", repo, "--title", tag, ...assetPaths];
    await exec("gh", args, { cwd });
    log.success(`Created release ${tag} on ${repo}`);
  } catch (ghErr) {
    // Fallback to GitHub REST API
    log.warn("gh CLI failed, trying GitHub REST API...");

    const token = process.env.GITHUB_TOKEN;
    if (!token) {
      throw new Error(
        `gh CLI failed (${ghErr}) and GITHUB_TOKEN is not set. ` +
        "Set GITHUB_TOKEN or install gh CLI to publish.",
      );
    }

    try {
      await createReleaseViaAPI(repo, tag, assetPaths, token);
      log.success(`Created release ${tag} on ${repo} (via API)`);
    } catch (apiErr: any) {
      if (apiErr?.message?.includes("422")) {
        throw new Error(`Release ${tag} already exists on ${repo}. Bump the version and try again.`);
      }
      throw apiErr;
    }
  }
}

async function createReleaseViaAPI(
  repo: string,
  tag: string,
  assetPaths: string[],
  token: string,
): Promise<void> {
  const { readFile } = await import("node:fs/promises");
  const { basename } = await import("node:path");

  // Create the release
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
    throw new Error(`GitHub API error ${createRes.status}: ${await createRes.text()}`);
  }

  const release = (await createRes.json()) as { upload_url: string };
  const uploadBase = release.upload_url.replace(/\{[^}]*\}/, "");

  // Upload each asset
  for (const assetPath of assetPaths) {
    const data = await readFile(assetPath);
    const name = basename(assetPath);

    const uploadRes = await fetch(`${uploadBase}?name=${encodeURIComponent(name)}`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        Accept: "application/vnd.github+json",
        "Content-Type": "application/wasm",
      },
      body: data,
    });

    if (!uploadRes.ok) {
      throw new Error(`Failed to upload ${name}: ${uploadRes.status} ${await uploadRes.text()}`);
    }

    log.step(`Uploaded ${name}`);
  }
}
