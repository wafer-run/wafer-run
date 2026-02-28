// Crypto service — ergonomic wrappers over the WIT-generated host imports.

export declare const crypto: {
  hash(password: string): string;
  compareHash(password: string, hash: string): void;
  sign(claims: string, expirySecs: bigint): string;
  verify(token: string): string;
  randomBytes(n: number): Uint8Array;
};

export function hash(password: string): string {
  return crypto.hash(password);
}

export function compareHash(password: string, hash: string): void {
  crypto.compareHash(password, hash);
}

export function sign(claims: unknown, expirySecs: number): string {
  return crypto.sign(JSON.stringify(claims), BigInt(expirySecs));
}

export function verify(token: string): unknown {
  const raw = crypto.verify(token);
  return JSON.parse(raw);
}

export function randomBytes(n: number): Uint8Array {
  return crypto.randomBytes(n);
}
