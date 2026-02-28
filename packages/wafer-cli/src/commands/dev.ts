import { readWaferConfig } from "../config/wafer-json.js";
import { buildCommand } from "./build.js";
import { startDevServer, type DevServer } from "../dev/server.js";
import { watchProject } from "../dev/watcher.js";
import { log } from "../util/logger.js";

interface DevOptions {
  port?: string;
}

export async function devCommand(opts: DevOptions): Promise<void> {
  const port = parseInt(opts.port ?? "8080", 10);
  const cwd = process.cwd();

  // Initial build
  await buildCommand();
  if (process.exitCode) return;

  const config = await readWaferConfig(cwd);
  let server: DevServer | null;
  try {
    server = await startDevServer(config, port, cwd);
  } catch (err) {
    log.error(String(err instanceof Error ? err.message : err));
    process.exitCode = 1;
    return;
  }

  let restarting = false;

  const restart = async () => {
    if (restarting) return;
    restarting = true;

    log.info("Change detected, rebuilding...");

    try {
      // Tear down existing server
      if (server) {
        await server.close();
        server = null;
      }

      // Rebuild
      process.exitCode = undefined;
      await buildCommand();
      if (process.exitCode) {
        log.warn("Build failed, waiting for changes...");
        restarting = false;
        return;
      }

      // Re-read config (may have changed) and restart
      const newConfig = await readWaferConfig(cwd);
      server = await startDevServer(newConfig, port, cwd);
    } catch (err) {
      log.error(`Restart failed: ${err}`);
      log.warn("Waiting for changes...");
    } finally {
      restarting = false;
    }
  };

  const watcher = watchProject(cwd, restart);

  // Handle graceful shutdown
  const shutdown = async () => {
    log.info("Shutting down...");
    await watcher.close();
    if (server) await server.close();
    process.exit(0);
  };

  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}
