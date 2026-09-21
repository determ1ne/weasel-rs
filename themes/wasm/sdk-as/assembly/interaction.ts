/**
 * 主帧重建主区域；with_layer 重建该层局部区域。层交互默认关闭，需 layer_interactive。
 * pointer_layer + pointer_region 标识目标。send_action 只允许 Pointer Down/Up，
 * begin_drag 只允许 Down；Cancel/Leave 只清除交互状态，不执行点击。
 * index 是已展示快照的页内索引，不能用于任意系统输入。
 */
import * as raw from "./raw";
import { Action } from "./types";
export { Action, PointerPhase } from "./types";
export { hit_region, pointer_region, pointer_layer, begin_drag } from "./raw";
export function send_action(action:Action,index:i32=0):void { raw.send_action(action,index); }
