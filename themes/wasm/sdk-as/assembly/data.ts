import { ConfigScope, DataKind } from "./types";
// Typed read-only host data. Paths use JSON Pointer, not JSON text or jq.
import { data_kind as kind, data_len as length, data_i64 as integer, data_number as number, data_string as stringCopy } from "./raw";

export const MISSING: i32 = DataKind.Missing;
export const NULL: i32 = DataKind.Null;
export const BOOLEAN: i32 = DataKind.Bool;
export const NUMBER: i32 = DataKind.Number;
export const STRING: i32 = DataKind.String;
export const ARRAY: i32 = DataKind.Array;
export const OBJECT: i32 = DataKind.Object;

export class Data {
  constructor(public scope: ConfigScope) {}

  kind(path: string): i32 {
    const bytes = String.UTF8.encode(path);
    return kind(this.scope, changetype<i32>(bytes), bytes.byteLength);
  }
  length(path: string): i32 {
    const bytes = String.UTF8.encode(path);
    return length(this.scope, changetype<i32>(bytes), bytes.byteLength);
  }
  integer(path: string): i64 {
    const bytes = String.UTF8.encode(path);
    return integer(this.scope, changetype<i32>(bytes), bytes.byteLength);
  }
  boolean(path: string): bool { return this.integer(path) != 0; }
  number(path: string, fallback: f64 = 0): f64 {
    if (this.kind(path) != NUMBER) return fallback;
    const bytes = String.UTF8.encode(path);
    return number(this.scope, changetype<i32>(bytes), bytes.byteLength);
  }
  string(path: string, fallback: string = ""): string {
    if (this.kind(path) != STRING) return fallback;
    const size = this.length(path);
    const output = new ArrayBuffer(size);
    const bytes = String.UTF8.encode(path);
    const copied = stringCopy(this.scope, changetype<i32>(bytes), bytes.byteLength,
      changetype<i32>(output), size);
    assert(copied == size);
    return String.UTF8.decode(output);
  }
}

// Only themeSettings.wasm.modules.<id>.config is exposed, not unrelated application settings.
export const options = new Data(ConfigScope.Module);
// Host-provided global presentation settings; independent of module defaults.
export const settings = new Data(ConfigScope.Global);
