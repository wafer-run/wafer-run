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
