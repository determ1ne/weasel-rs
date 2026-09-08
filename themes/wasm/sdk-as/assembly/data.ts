// Typed read-only host data. Paths use JSON Pointer, not JSON text or jq.
@external("weasel", "data_kind")
declare function kind(scope: i32, path: usize, len: i32): i32;
@external("weasel", "data_len")
declare function length(scope: i32, path: usize, len: i32): i32;
@external("weasel", "data_i64")
declare function integer(scope: i32, path: usize, len: i32): i64;
@external("weasel", "data_number")
declare function number(scope: i32, path: usize, len: i32): f64;
@external("weasel", "data_string")
declare function stringCopy(scope: i32, path: usize, len: i32, dst: usize, capacity: i32): i32;

export const MISSING: i32 = 0;
export const NULL: i32 = 1;
export const BOOLEAN: i32 = 2;
export const NUMBER: i32 = 3;
export const STRING: i32 = 4;
export const ARRAY: i32 = 5;
export const OBJECT: i32 = 6;

export class Data {
  constructor(public scope: i32) {}

  kind(path: string): i32 {
    const bytes = String.UTF8.encode(path);
    return kind(this.scope, changetype<usize>(bytes), bytes.byteLength);
  }
  length(path: string): i32 {
    const bytes = String.UTF8.encode(path);
    return length(this.scope, changetype<usize>(bytes), bytes.byteLength);
  }
  integer(path: string): i64 {
    const bytes = String.UTF8.encode(path);
    return integer(this.scope, changetype<usize>(bytes), bytes.byteLength);
  }
  boolean(path: string): bool { return this.integer(path) != 0; }
  number(path: string, fallback: f64 = 0): f64 {
    if (this.kind(path) != NUMBER) return fallback;
    const bytes = String.UTF8.encode(path);
    return number(this.scope, changetype<usize>(bytes), bytes.byteLength);
  }
  string(path: string, fallback: string = ""): string {
    if (this.kind(path) != STRING) return fallback;
    const size = this.length(path);
    const output = new ArrayBuffer(size);
    const bytes = String.UTF8.encode(path);
    const copied = stringCopy(this.scope, changetype<usize>(bytes), bytes.byteLength,
      changetype<usize>(output), size);
    assert(copied == size);
    return String.UTF8.decode(output);
  }
}

export const candidate = new Data(0);
// Only themeSettings.wasm.modules.<id>.config is exposed, not unrelated application settings.
export const options = new Data(1);
// Host-provided global presentation settings; independent of module defaults.
export const settings = new Data(2);
