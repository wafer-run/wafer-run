// Logger service — ergonomic wrappers over the WIT-generated host imports.

export interface LogField {
  key: string;
  value: string;
}

export declare const logger: {
  debug(msg: string, fields: LogField[]): void;
  info(msg: string, fields: LogField[]): void;
  warn(msg: string, fields: LogField[]): void;
  error(msg: string, fields: LogField[]): void;
};

export function debug(msg: string, fields?: Record<string, string>): void {
  logger.debug(msg, toFields(fields));
}

export function info(msg: string, fields?: Record<string, string>): void {
  logger.info(msg, toFields(fields));
}

export function warn(msg: string, fields?: Record<string, string>): void {
  logger.warn(msg, toFields(fields));
}

export function error(msg: string, fields?: Record<string, string>): void {
  logger.error(msg, toFields(fields));
}

function toFields(fields?: Record<string, string>): LogField[] {
  if (!fields) return [];
  return Object.entries(fields).map(([key, value]) => ({ key, value }));
}
