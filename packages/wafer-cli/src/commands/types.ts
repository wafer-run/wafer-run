import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { log } from "../util/logger.js";
import { readWaferConfig, writeWaferConfig } from "../config/wafer-json.js";

const REGISTRY_BASE = "https://wafer.run";

/**
 * Parse an interface reference like "github.com/owner/repo@version"
 * into { name, version }.
 */
function parseRef(ref: string): { name: string; version: string } {
  const atIdx = ref.lastIndexOf("@");
  if (atIdx === -1) {
    throw new Error(
      `Invalid interface reference "${ref}". Expected format: github.com/owner/repo@version`,
    );
  }
  const name = ref.slice(0, atIdx);
  const version = ref.slice(atIdx + 1);
  if (!name.startsWith("github.com/") || !version) {
    throw new Error(
      `Invalid interface reference "${ref}". Expected format: github.com/owner/repo@version`,
    );
  }
  return { name, version };
}

export async function typesAddCommand(ref: string): Promise<void> {
  const { name, version } = parseRef(ref);

  log.step(`Fetching interface ${name}@${version}`);

  // Download the .interface.json from the registry
  const url = `${REGISTRY_BASE}/registry/packages/${name}/download/${version}?type=interface`;
  const resp = await fetch(url, { redirect: "follow" });

  if (!resp.ok) {
    log.error(`Failed to fetch interface: ${resp.status} ${resp.statusText}`);
    process.exitCode = 1;
    return;
  }

  const body = await resp.text();

  // Validate it's valid JSON
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    log.error("Downloaded file is not valid JSON");
    process.exitCode = 1;
    return;
  }

  // Extract the interface name from the JSON (or fall back to repo name)
  const ifaceName =
    (parsed as Record<string, unknown>).name ??
    name.split("/").pop() ??
    "unknown";

  // Write to .wafer/interfaces/
  const interfacesDir = join(process.cwd(), ".wafer", "interfaces");
  await mkdir(interfacesDir, { recursive: true });

  const filename = `${ifaceName}@${version}.interface.json`;
  const filepath = join(interfacesDir, filename);
  await writeFile(filepath, JSON.stringify(parsed, null, 2) + "\n");

  log.success(`Saved ${filename} to .wafer/interfaces/`);

  // Update wafer.json
  const config = await readWaferConfig();
  if (!config.interfaces) {
    config.interfaces = [];
  }

  // Check if already present
  const existing = config.interfaces.find((e) => e.ref === ref);
  if (existing) {
    existing.local = `.wafer/interfaces/${filename}`;
    log.info(`Updated existing interface entry for ${ref}`);
  } else {
    config.interfaces.push({
      ref,
      local: `.wafer/interfaces/${filename}`,
    });
    log.info(`Added interface entry for ${ref}`);
  }

  await writeWaferConfig(config);
}
