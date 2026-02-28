// Config service — ergonomic wrappers over the WIT-generated host imports.

export declare const config: {
  get(key: string): string | undefined;
  set(key: string, value: string): void;
};

export function get(key: string): string | undefined {
  return config.get(key);
}

export function getDefault(key: string, defaultValue: string): string {
  return config.get(key) ?? defaultValue;
}

export function set(key: string, value: string): void {
  config.set(key, value);
}
