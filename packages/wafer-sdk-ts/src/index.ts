// wafer-sdk-ts — TypeScript SDK for writing WAFER blocks as WebAssembly components.
//
// Build blocks with:
//   npx tsc --outDir dist/
//   npx jco componentize dist/index.js --wit ../wafer-wit/wit --world-name wafer-block -o block.wasm
//
// A minimal block:
//
//   import { defineBlock, type Message, type BlockResult, continueResult } from 'wafer-sdk-ts';
//
//   defineBlock({
//     info: () => ({
//       name: '@example/myblock',
//       version: '1.0.0',
//       interface: 'processor@v1',
//       summary: 'A simple message processor.',
//       instanceMode: InstanceMode.PerNode,
//       allowedModes: [InstanceMode.PerNode],
//     }),
//     handle: (msg) => continueResult(msg),
//     lifecycle: (_event) => {},
//   });

export {
  type MetaEntry,
  type Message,
  Action,
  type Response,
  type WaferError,
  type BlockResult,
  InstanceMode,
  type BlockInfo,
  LifecycleType,
  type LifecycleEvent,
  getMeta,
  setMeta,
  dataAsString,
  dataAsJSON,
  newMessage,
  continueResult,
  respondResult,
  jsonRespond,
  dropResult,
  errorResult,
} from "./types.js";

export { ErrorCode, type ErrorCodeType } from "./error-codes.js";

export * as services from "./services/index.js";

// Re-export individual services at top level for convenience.
export * as database from "./services/database.js";
export * as storage from "./services/storage.js";
export * as crypto from "./services/crypto.js";
export * as network from "./services/network.js";
export * as logger from "./services/logger.js";
export * as config from "./services/config.js";

import type { BlockInfo, Message, BlockResult, LifecycleEvent } from "./types.js";

/** The interface a WAFER block must implement. */
export interface Block {
  info(): BlockInfo;
  handle(msg: Message): BlockResult;
  lifecycle(event: LifecycleEvent): void;
}

/**
 * Register a block implementation. This exports the block functions so the
 * component model runtime can call them.
 *
 * Call this from your top-level module scope (not inside a function).
 */
export function defineBlock(block: Block): Block {
  // The returned object is what jco picks up as the exported `block` interface.
  // When componentize-js processes this module, it wires the returned object's
  // methods to the WIT-exported `block` interface.
  return block;
}
