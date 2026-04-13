// Host function stubs for the WAFER JS/TS SDK.
//
// These functions are placeholders that throw at runtime in a plain Node/browser
// environment. When the WASM compilation pipeline is available, they will be
// replaced by imports linked by the WAFER wasmi runtime (similar to the Go SDK's
// //go:wasmimport directives).
//
// Until then, this file provides correct type signatures so block authors can
// write and type-check their code without a working WASM toolchain.

import type { BlockResult, Message } from "./types.js";

/**
 * Call another block by name and return its BlockResult.
 * Only available inside a compiled WASM block linked to the WAFER runtime.
 */
export function callBlock(_name: string, _msg: Message): BlockResult {
  throw new Error(
    "callBlock is only available inside a compiled WASM block linked to the WAFER runtime.",
  );
}

/**
 * Emit a log message at the given level.
 * Conventional level strings: "trace", "debug", "info", "warn", "error".
 * Only available inside a compiled WASM block.
 */
export function log(_level: string, _msg: string): void {
  throw new Error(
    "log is only available inside a compiled WASM block linked to the WAFER runtime.",
  );
}

/**
 * Report whether the current execution has been cancelled by the runtime
 * (e.g. client disconnect or deadline exceeded).
 * Only available inside a compiled WASM block.
 */
export function isCancelled(): boolean {
  throw new Error(
    "isCancelled is only available inside a compiled WASM block linked to the WAFER runtime.",
  );
}
