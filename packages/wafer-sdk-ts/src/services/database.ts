// Database service — ergonomic wrappers over the WIT-generated host imports.
//
// In a componentized block these functions delegate to the host-imported
// `wafer:block-world/database` interface. The actual import binding is injected
// by jco during componentization.

/** A database record with JSON-encoded data. */
export interface DbRecord {
  id: string;
  /** JSON-encoded map of field name → value. */
  data: string;
}

export interface RecordList {
  records: DbRecord[];
  totalCount: number;
  page: number;
  pageSize: number;
}

export enum FilterOp {
  Eq = 0,
  Neq,
  Gt,
  Gte,
  Lt,
  Lte,
  Like,
  In,
  IsNull,
  IsNotNull,
}

export interface Filter {
  field: string;
  operator: FilterOp;
  value: string;
}

export interface SortField {
  field: string;
  desc: boolean;
}

export interface ListOptions {
  filters: Filter[];
  sort: SortField[];
  limit: number;
  offset: number;
}

// The WIT-generated import functions. These are populated by the component
// model runtime (jco / wasmtime). Block authors call the wrapper functions below.
export declare const database: {
  get(collection: string, id: string): DbRecord;
  list(collection: string, options: ListOptions): RecordList;
  create(collection: string, data: string): DbRecord;
  update(collection: string, id: string, data: string): DbRecord;
  delete(collection: string, id: string): void;
  count(collection: string, filters: Filter[]): number;
  queryRaw(query: string, args: string): DbRecord[];
  execRaw(query: string, args: string): number;
};

export function get(collection: string, id: string): DbRecord {
  return database.get(collection, id);
}

export function getInto<T>(collection: string, id: string): T {
  const rec = database.get(collection, id);
  return JSON.parse(rec.data) as T;
}

export function list(collection: string, options?: Partial<ListOptions>): RecordList {
  const opts: ListOptions = {
    filters: options?.filters ?? [],
    sort: options?.sort ?? [],
    limit: options?.limit ?? 0,
    offset: options?.offset ?? 0,
  };
  return database.list(collection, opts);
}

export function create(collection: string, data: unknown): DbRecord {
  return database.create(collection, JSON.stringify(data));
}

export function update(collection: string, id: string, data: unknown): DbRecord {
  return database.update(collection, id, JSON.stringify(data));
}

export function del(collection: string, id: string): void {
  database.delete(collection, id);
}

export function count(collection: string, filters?: Filter[]): number {
  return database.count(collection, filters ?? []);
}

export function queryRaw(query: string, ...args: unknown[]): DbRecord[] {
  return database.queryRaw(query, JSON.stringify(args));
}

export function execRaw(query: string, ...args: unknown[]): number {
  return database.execRaw(query, JSON.stringify(args));
}
