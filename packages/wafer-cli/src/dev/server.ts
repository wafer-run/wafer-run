import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import type { WaferConfig, FlowDef, FlowEntry } from "../config/types.js";
import { log } from "../util/logger.js";

interface WaferRuntimeLike {
  register(name: string, path: string): void;
  resolve(): void;
  start(): void;
  run(flowId: string, messageJson: string): string;
}

const RUNTIME_PKG = "wafer-run";

async function createRuntime(): Promise<WaferRuntimeLike> {
  try {
    // Dynamic require avoids TS static module resolution
    const mod = await import(/* webpackIgnore: true */ RUNTIME_PKG);
    return new mod.WaferRuntime();
  } catch {
    throw new Error(
      `${RUNTIME_PKG} is required for dev server. Install it with: npm install ${RUNTIME_PKG}`,
    );
  }
}

export interface DevServer {
  server: Server;
  close(): Promise<void>;
}

export async function startDevServer(config: WaferConfig, port: number, cwd: string): Promise<DevServer> {
  const runtime = await createRuntime();

  for (const block of config.blocks) {
    const wasmPath = join(cwd, "dist", `${block.name}.wasm`);
    runtime.register(block.name, wasmPath);
  }

  for (const entry of config.flows) {
    const flowPath = join(cwd, entry.source);
    runtime.register(entry.source, flowPath);
  }

  runtime.resolve();
  runtime.start();

  // Read the first flow file to get its id for the default
  let defaultFlowId = "main";
  if (config.flows.length > 0) {
    try {
      const firstFlowRaw = await readFile(join(cwd, config.flows[0].source), "utf-8");
      defaultFlowId = (JSON.parse(firstFlowRaw) as FlowDef).id ?? "main";
    } catch {
      log.warn("Could not read first flow file, using default flow id 'main'");
    }
  }

  const server = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => chunks.push(c));
    req.on("end", () => {
      const body = Buffer.concat(chunks).toString();
      const msg = JSON.stringify({
        kind: req.method + ":" + req.url,
        data: body,
        meta: { "http.method": req.method, "http.path": req.url },
      });

      try {
        const result = JSON.parse(runtime.run(defaultFlowId, msg));
        const status = result.action === "error" ? 500 : 200;
        res.writeHead(status, { "Content-Type": "application/json" });
        res.end(result.response?.data ?? "{}");
      } catch (err) {
        res.writeHead(500, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: String(err) }));
      }
    });
  });

  await new Promise<void>((resolve) => {
    server.listen(port, () => resolve());
  });

  log.success(`Dev server listening on http://localhost:${port}`);

  return {
    server,
    async close() {
      await new Promise<void>((resolve, reject) => {
        server.close((err) => (err ? reject(err) : resolve()));
      });
    },
  };
}
