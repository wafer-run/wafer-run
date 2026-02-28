/**
 * Configuration for the WaferClient.
 */
export interface WaferConfig {
  /** Base URL of the Wafer instance (e.g. "https://app.example.com") */
  url: string;
  /** Optional API key for authentication */
  apiKey?: string;
  /** Default headers to include with every request */
  headers?: Record<string, string>;
  /** Request timeout in milliseconds (default: 30000) */
  timeout?: number;
  /** Fetch credentials mode (default: "include" in browser, "same-origin" in Node) */
  credentials?: RequestCredentials;
  /** Custom fetch implementation (for polyfill or testing) */
  fetch?: typeof globalThis.fetch;
}
