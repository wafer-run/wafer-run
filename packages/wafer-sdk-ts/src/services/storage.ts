// Storage service — ergonomic wrappers over the WIT-generated host imports.

export interface ObjectInfo {
  key: string;
  size: number;
  contentType: string;
  lastModified: string;
}

export interface ObjectList {
  objects: ObjectInfo[];
  totalCount: number;
}

export declare const storage: {
  put(folder: string, key: string, data: Uint8Array, contentType: string): void;
  get(folder: string, key: string): [Uint8Array, ObjectInfo];
  delete(folder: string, key: string): void;
  list(folder: string, prefix: string, limit: number, offset: number): ObjectList;
};

export function put(
  folder: string,
  key: string,
  data: Uint8Array,
  contentType: string,
): void {
  storage.put(folder, key, data, contentType);
}

export function get(folder: string, key: string): { data: Uint8Array; info: ObjectInfo } {
  const [data, info] = storage.get(folder, key);
  return { data, info };
}

export function del(folder: string, key: string): void {
  storage.delete(folder, key);
}

export function list(
  folder: string,
  prefix = "",
  limit = 0,
  offset = 0,
): ObjectList {
  return storage.list(folder, prefix, limit, offset);
}
