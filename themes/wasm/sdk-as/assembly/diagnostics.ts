/** 日志不会弹通知；仅需要用户处理的问题使用 report_notice。 */
import * as raw from "./raw";
import { LogLevel } from "./types";
export { LogLevel } from "./types";
export function log(message:string,level:LogLevel=LogLevel.Info):void {
  const bytes=String.UTF8.encode(message);
  raw.log(level,changetype<i32>(bytes),bytes.byteLength);
}
export function report_notice(message:string):void {
  const bytes=String.UTF8.encode(message);
  raw.report_notice(changetype<i32>(bytes),bytes.byteLength);
}
