import { ConfigScope, DataKind } from "./types";
// Typed read-only host data. Paths use JSON Pointer, not JSON text or jq.
import { data_kind as kind, data_len as length, data_i64 as integer, data_number as number, data_string as stringCopy } from "./raw";

const MAX_PATH_BYTES: i32 = 1024;
const MAX_STRING_BYTES: i32 = 1024 * 1024;

function pathBytes(path: string): ArrayBuffer | null {
  const bytes = String.UTF8.encode(path);
  return bytes.byteLength <= MAX_PATH_BYTES ? bytes : null;
}

/**
 * 对宿主配置树的有界只读视图。路径使用 JSON Pointer。
 *
 * 类型化读取器在字段缺失、类型不符或复制超限时返回调用方给出的 fallback，不会 assert；
 * 需要区分“真实 fallback 值”和读取失败时，应先调用 `kind` 检查 `DataKind`。
 */
export class Data {
  constructor(public scope: ConfigScope) {}

  kind(path: string): i32 {
    const bytes = pathBytes(path);
    if (bytes == null) return DataKind.Missing;
    return kind(this.scope, changetype<i32>(bytes), bytes.byteLength);
  }
  length(path: string): i32 {
    const bytes = pathBytes(path);
    if (bytes == null) return -1;
    return length(this.scope, changetype<i32>(bytes), bytes.byteLength);
  }
  integer(path: string, fallback: i64 = 0): i64 {
    if (this.kind(path) != DataKind.Number) return fallback;
    const bytes = pathBytes(path);
    if (bytes == null) return fallback;
    return integer(this.scope, changetype<i32>(bytes), bytes.byteLength);
  }
  boolean(path: string, fallback: bool = false): bool {
    if (this.kind(path) != DataKind.Bool) return fallback;
    const bytes = pathBytes(path);
    if (bytes == null) return fallback;
    return integer(this.scope, changetype<i32>(bytes), bytes.byteLength) != 0;
  }
  number(path: string, fallback: f64 = 0): f64 {
    if (this.kind(path) != DataKind.Number) return fallback;
    const bytes = pathBytes(path);
    if (bytes == null) return fallback;
    return number(this.scope, changetype<i32>(bytes), bytes.byteLength);
  }
  string(path: string, fallback: string = ""): string {
    if (this.kind(path) != DataKind.String) return fallback;
    const size = this.length(path);
    if (size < 0 || size > MAX_STRING_BYTES) return fallback;
    if (size == 0) return "";
    const output = new ArrayBuffer(size);
    const bytes = pathBytes(path);
    if (bytes == null) return fallback;
    const copied = stringCopy(this.scope, changetype<i32>(bytes), bytes.byteLength,
      changetype<i32>(output), size);
    if (copied != size) return fallback;
    return String.UTF8.decode(output);
  }
}

// Only themeSettings.wasm.modules.<id>.config is exposed, not unrelated application settings.
export const options = new Data(ConfigScope.Module);
// Host-provided global presentation settings; independent of module defaults.
export const settings = new Data(ConfigScope.Global);
