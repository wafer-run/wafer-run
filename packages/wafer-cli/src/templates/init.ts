import type { WaferConfig, ChainDef } from "../config/types.js";

export function waferJsonTemplate(name: string): WaferConfig {
  return {
    name,
    version: "0.1.0",
    blocks: [
      { name: "hello", source: "blocks/hello" },
    ],
    chains: [
      { name: "main", source: "chains/main.json" },
    ],
    publish: {
      repo: `wafer-run/${name}`,
      blocks: ["hello"],
    },
  };
}

export function mainChainTemplate(): ChainDef {
  return {
    id: "main",
    root: { block: "hello" },
    http: {
      routes: [{ path: "/", path_prefix: true }],
    },
  };
}

export function packageJsonTemplate(name: string): object {
  return {
    name,
    version: "0.1.0",
    private: true,
    type: "module",
    scripts: {
      build: "wafer build",
      dev: "wafer dev",
    },
    devDependencies: {
      "typescript": "^5.7.0",
      "wafer-sdk-ts": "^0.1.0",
      "@bytecodealliance/jco": "^1.8.0",
    },
  };
}

export function helloBlockTemplate(): string {
  return `import { defineBlock, type Message, type BlockResult, type LifecycleEvent, continueResult, jsonRespond, InstanceMode } from 'wafer-sdk-ts';

export default defineBlock({
  info: () => ({
    name: 'hello',
    version: '1.0.0',
    interface: 'processor@v1',
    summary: 'A simple hello-world block.',
    instanceMode: InstanceMode.PerNode,
    allowedModes: [InstanceMode.PerNode],
  }),

  handle: (msg: Message): BlockResult => {
    return jsonRespond({ hello: 'world' });
  },

  lifecycle: (_event: LifecycleEvent): void => {},
});
`;
}

export function tsconfigTemplate(): object {
  return {
    compilerOptions: {
      target: "ES2022",
      module: "ES2022",
      moduleResolution: "bundler",
      declaration: true,
      strict: true,
      outDir: "dist",
      rootDir: "blocks",
      esModuleInterop: true,
      skipLibCheck: true,
    },
    include: ["blocks/**/*.ts"],
    exclude: ["node_modules", "dist"],
  };
}

export function gitignoreTemplate(): string {
  return `node_modules/
dist/
*.wasm
`;
}
