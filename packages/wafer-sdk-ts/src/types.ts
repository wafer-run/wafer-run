// Core types matching the WIT definitions in wafer-wit/wit/types.wit.
// These TypeScript interfaces mirror the WIT records and enums so that block
// authors have a fully typed API.

/** A key-value metadata entry (WIT has no map type). */
export interface MetaEntry {
  key: string;
  value: string;
}

/** A message flowing through the block chain. */
export interface Message {
  kind: string;
  data: Uint8Array;
  meta: MetaEntry[];
}

/** The action a block wants the runtime to take after processing. */
export enum Action {
  Continue = "continue",
  Respond = "respond",
  Drop = "drop",
  Error = "error",
}

/** A response payload returned when a block short-circuits the chain. */
export interface Response {
  data: Uint8Array;
  meta: MetaEntry[];
}

/** A structured error returned by a block. */
export interface WaferError {
  code: string;
  message: string;
  meta: MetaEntry[];
}

/** The result of processing a message. */
export interface BlockResult {
  action: Action;
  response?: Response;
  error?: WaferError;
  message?: Message;
}

/** How many block instances are created. */
export enum InstanceMode {
  PerNode = "per-node",
  Singleton = "singleton",
  PerChain = "per-chain",
  PerExecution = "per-execution",
}

/** Metadata describing a block. */
export interface BlockInfo {
  name: string;
  version: string;
  interface: string;
  summary: string;
  instanceMode: InstanceMode;
  allowedModes: InstanceMode[];
}

/** The kind of lifecycle event. */
export enum LifecycleType {
  Init = "init",
  Start = "start",
  Stop = "stop",
}

/** A lifecycle event sent to blocks during transitions. */
export interface LifecycleEvent {
  eventType: LifecycleType;
  data: Uint8Array;
}

// ─── Message helpers ─────────────────────────────────────────

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** Get a metadata value from a message by key. */
export function getMeta(msg: Message, key: string): string | undefined {
  const entry = msg.meta.find((e) => e.key === key);
  return entry?.value;
}

/** Set a metadata value on a message. Mutates in place. */
export function setMeta(msg: Message, key: string, value: string): void {
  const idx = msg.meta.findIndex((e) => e.key === key);
  if (idx >= 0) {
    msg.meta[idx].value = value;
  } else {
    msg.meta.push({ key, value });
  }
}

/** Decode message data as a UTF-8 string. */
export function dataAsString(msg: Message): string {
  return decoder.decode(msg.data);
}

/** Decode message data as JSON. */
export function dataAsJSON<T = unknown>(msg: Message): T {
  return JSON.parse(dataAsString(msg));
}

/** Create a new message with optional JSON data. */
export function newMessage(
  kind: string,
  data?: unknown,
  meta?: Record<string, string>,
): Message {
  const metaEntries: MetaEntry[] = meta
    ? Object.entries(meta).map(([key, value]) => ({ key, value }))
    : [];
  return {
    kind,
    data: data ? encoder.encode(JSON.stringify(data)) : new Uint8Array(),
    meta: metaEntries,
  };
}

// ─── Result constructors ─────────────────────────────────────

/** Pass the message to the next block. */
export function continueResult(msg: Message): BlockResult {
  return { action: Action.Continue, message: msg };
}

/** Short-circuit with a response. */
export function respondResult(data: Uint8Array, meta?: MetaEntry[]): BlockResult {
  return {
    action: Action.Respond,
    response: { data, meta: meta ?? [] },
  };
}

/** Short-circuit with a JSON response. */
export function jsonRespond(value: unknown): BlockResult {
  return {
    action: Action.Respond,
    response: {
      data: encoder.encode(JSON.stringify(value)),
      meta: [{ key: "content-type", value: "application/json" }],
    },
  };
}

/** Silently drop the message. */
export function dropResult(): BlockResult {
  return { action: Action.Drop };
}

/** Short-circuit with an error. */
export function errorResult(code: string, message: string): BlockResult {
  return {
    action: Action.Error,
    error: { code, message, meta: [] },
  };
}
