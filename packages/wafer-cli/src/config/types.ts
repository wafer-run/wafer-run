export interface NodeDef {
  block: string;
  next?: string[];
}

export interface ChainDef {
  id: string;
  root: NodeDef;
  nodes?: Record<string, NodeDef>;
}

export interface BlockEntry {
  name: string;
  source: string;
}

export interface PublishConfig {
  repo: string;
  blocks: string[];
}

export interface ChainEntry {
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
  chains: ChainEntry[];
  interfaces?: InterfaceEntry[];
  publish?: PublishConfig;
}
