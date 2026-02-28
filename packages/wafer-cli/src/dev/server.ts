import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import type { WaferConfig, ChainDef, ChainEntry } from "../config/types.js";
import { log } from "../util/logger.js";

interface WaferRuntimeLike {
  registerWasmBlock(typeName: string, wasmPath: string): void;
  addChainDef(chainDefJson: string): void;
  resolve(): void;
  start(): void;
  execute(chainId: string, messageJson: string): string;
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

async function loadChains(entries: ChainEntry[], cwd: string): Promise<ChainDef[]> {
  const chains: ChainDef[] = [];
  for (const entry of entries) {
    const raw = await readFile(join(cwd, entry.source), "utf-8");
    chains.push(JSON.parse(raw) as ChainDef);
  }
  return chains;
}

export interface DevServer {
  server: Server;
  close(): Promise<void>;
}

export async function startDevServer(config: WaferConfig, port: number, cwd: string): Promise<DevServer> {
  const runtime = await createRuntime();

  for (const block of config.blocks) {
    const wasmPath = join(cwd, "dist", `${block.name}.wasm`);
    runtime.registerWasmBlock(block.name, wasmPath);
  }

  const chains = await loadChains(config.chains, cwd);
  for (const chain of chains) {
    runtime.addChainDef(JSON.stringify(chain));
  }

  runtime.resolve();
  runtime.start();

  const defaultChainId = chains[0]?.id ?? "main";

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
        const result = JSON.parse(runtime.execute(defaultChainId, msg));
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
