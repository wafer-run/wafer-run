// WAFER block types — mirror the Rust wafer-block core_types.rs exactly.
// Serialization uses PascalCase for enum variants (no serde rename_all).
// `data` fields are byte arrays (JSON number arrays in the wire format).

/** A key-value metadata entry attached to a Message or Response. */
export interface MetaEntry {
  key: string;
  value: string;
}

/**
 * A message flowing through the block pipeline.
 * Matches the Rust Message struct: data is a Vec<u8> serialized as a JSON
 * array of numbers (0–255).
 */
export interface Message {
  kind: string;
  data: number[];
  meta: MetaEntry[];
}

/**
 * The action a block wants the runtime to take after processing a message.
 * Values match the Rust Action enum serialized by serde (PascalCase).
 */
export type Action = "Continue" | "Respond" | "Drop" | "Error";

/**
 * A response payload returned when a block short-circuits the pipeline.
 * Matches the Rust Response struct.
 */
export interface Response {
  data: number[];
  meta: MetaEntry[];
}

/**
 * Error codes mirroring the Rust ErrorCode enum.
 * Values are PascalCase strings, matching serde serialization.
 */
export type ErrorCode =
  | "Ok"
  | "Cancelled"
  | "Unknown"
  | "InvalidArgument"
  | "DeadlineExceeded"
  | "NotFound"
  | "AlreadyExists"
  | "PermissionDenied"
  | "ResourceExhausted"
  | "FailedPrecondition"
  | "Aborted"
  | "OutOfRange"
  | "Unimplemented"
  | "Internal"
  | "Unavailable"
  | "DataLoss"
  | "Unauthenticated";

/**
 * A structured error returned by a block.
 * Matches the Rust WaferError struct.
 */
export interface WaferError {
  code: ErrorCode;
  message: string;
  meta: MetaEntry[];
}

/**
 * The result of processing a message.
 * Matches the Rust BlockResult struct exactly.
 */
export interface BlockResult {
  action: Action;
  response?: Response;
  error?: WaferError;
  message?: Message;
}

/**
 * Controls how many block instances are created and when.
 * Values match the Rust InstanceMode enum (PascalCase).
 */
export type InstanceMode =
  | "PerNode"
  | "Singleton"
  | "PerFlow"
  | "PerExecution";

/** The block's identity and metadata, returned by __wafer_info. */
export interface BlockInfo {
  name: string;
  version: string;
  interface: string;
  summary: string;
  instance_mode?: InstanceMode;
}

/** Lifecycle event types matching the Rust LifecycleType enum. */
export type LifecycleType = "Init" | "Start" | "Stop";

/** A lifecycle event sent to blocks during transitions. */
export interface LifecycleEvent {
  event_type: LifecycleType;
  data: number[];
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

/** Return a BlockResult that passes the message to the next block. */
export function cont(): BlockResult {
  return { action: "Continue" };
}

/** Return a BlockResult that silently discards the message. */
export function drop(): BlockResult {
  return { action: "Drop" };
}

/** Return a BlockResult that short-circuits the pipeline with raw bytes. */
export function respondBytes(data: number[], meta: MetaEntry[] = []): BlockResult {
  return { action: "Respond", response: { data, meta } };
}

/**
 * Return a BlockResult that short-circuits the pipeline with a JSON value.
 * The value is JSON-serialized and the resulting UTF-8 bytes are used as data.
 */
export function respond(value: unknown, meta: MetaEntry[] = []): BlockResult {
  const json = JSON.stringify(value);
  const bytes = Array.from(new TextEncoder().encode(json));
  return respondBytes(bytes, meta);
}

/** Return a BlockResult signalling an error. */
export function blockError(
  code: ErrorCode,
  message: string,
  meta: MetaEntry[] = [],
): BlockResult {
  return {
    action: "Error",
    error: { code, message, meta },
  };
}
