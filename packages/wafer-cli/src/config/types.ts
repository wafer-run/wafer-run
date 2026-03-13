export interface NodeDef {
  block: string;
  next?: string[];
}

export interface FlowDef {
  id: string;
  root: NodeDef;
  nodes?: Record<string, NodeDef>;
}

export interface BlockEntry {
  name: string;
  source: string;
  /** Interface identifier, e.g. "database@v1". Used for .block.json generation. */
  interface?: string;
  /** Human-readable description. */
  summary?: string;
  /** Runtime type: "native", "wasm", or "both". Defaults to "wasm". */
  runtime?: "native" | "wasm" | "both";
  /** Build/install instructions. The "type" field discriminates (e.g. "cargo", "npm"). */
  build?: Record<string, unknown>;
}

export interface PublishConfig {
  repo: string;
  blocks: string[];
}

export interface FlowEntry {
  name: string;
  source: string;
}

export interface InterfaceEntry {
  ref: string;
  local?: string;
}

export interface WaferConfig {
  name: string;
  version: string;
  blocks: BlockEntry[];
  flows: FlowEntry[];
  interfaces?: InterfaceEntry[];
  publish?: PublishConfig;
}
