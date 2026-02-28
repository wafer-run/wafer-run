/**
 * Metadata key-value pairs attached to a Wafer message.
 */
export type WaferMeta = Record<string, string>;

/**
 * A Wafer message, matching the internal Message type.
 *
 * `kind` follows the format "METHOD:/path" (e.g. "POST:/api/users").
 */
export interface WaferMessage {
  kind: string;
  data?: unknown;
  meta?: WaferMeta;
}
