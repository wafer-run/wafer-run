// @wafer-run/sdk — TypeScript/JavaScript SDK for writing WAFER blocks.
//
// Block authors define a BlockDef and call block() to register it. The SDK
// handles all ABI plumbing once the WASM compilation pipeline is available.
//
// # Quick start
//
//   import { block, respond } from "@wafer-run/sdk";
//
//   export default block({
//     name: "example/echo",
//     version: "0.1.0",
//     interface: "handler@v1",
//     summary: "Echo block",
//     handle(msg) {
//       return respond({ echo: msg.kind });
//     },
//   });

export * from "./types.js";
export { callBlock, log, isCancelled } from "./host.js";

import type { BlockResult, InstanceMode, LifecycleEvent, Message } from "./types.js";

/** Full definition of a WAFER block written in TypeScript/JavaScript. */
export interface BlockDef {
  /** Block name in {org}/{block} format, e.g. "example/echo". */
  name: string;
  /** Semantic version string, e.g. "0.1.0". */
  version: string;
  /** Interface identifier, e.g. "handler@v1". */
  interface: string;
  /** One-line human-readable description. */
  summary: string;
  /** Controls how many instances are created (default: "PerNode"). */
  instance_mode?: InstanceMode;
  /** Process an incoming message and return a result. */
  handle(msg: Message): BlockResult;
  /** Handle a lifecycle event (init, start, stop). Optional. */
  lifecycle?(event: LifecycleEvent): void;
}

/**
 * Register a WAFER block definition.
 *
 * Sets global variables that the Rust/boa runtime reads:
 * - `__wafer_block_info`: block metadata (name, version, etc.)
 * - `__wafer_handle_message`: function that receives a JSON message string
 *   and returns a JSON result string.
 * - `__wafer_lifecycle_handler`: function that receives a JSON lifecycle
 *   event and returns a JSON result string.
 *
 * Returns the BlockDef unchanged so it can be used as the module's default
 * export for tooling inspection.
 */
export function block(def: BlockDef): BlockDef {
  (globalThis as any).__wafer_block_info = {
    name: def.name,
    version: def.version,
    interface: def.interface,
    summary: def.summary,
    instance_mode: def.instance_mode ?? "PerNode",
  };

  (globalThis as any).__wafer_handle_message = (msgJson: string): string => {
    const msg: Message = JSON.parse(msgJson);
    const result = def.handle(msg);
    return JSON.stringify(result);
  };

  if (def.lifecycle) {
    const lifecycleFn = def.lifecycle;
    (globalThis as any).__wafer_lifecycle_handler = (evtJson: string): string => {
      const evt: LifecycleEvent = JSON.parse(evtJson);
      lifecycleFn(evt);
      return "null";
    };
  }

  return def;
}
