// 最短入门：配置 → 读取第一项 → 完整绘制 → 点击。完整列表见 minimal.ts。
import { ABI_VERSION, EventKind, FrameResult, ErrorCode } from "../assembly/lifecycle";
import { options } from "../assembly/config";
import { readView } from "../assembly/view";
import { draw, fill_rect, set_font } from "../assembly/draw";
import { set_size } from "../assembly/surface";
import { hit_region, pointer_region, send_action, Action, PointerPhase } from "../assembly/interaction";

export function theme_abi_version(): i32 { return ABI_VERSION; }
export function theme_capabilities(): i32 { return 0; } // 不声明 preedit / resident
export function theme_create(_mode:i32,_dark:i32):i32 {
  set_font(0,options.string("/font","Microsoft YaHei UI"));
  return ErrorCode.Success; // 仅初始化，不能绘图
}
function paint():i32 {
  const view=readView();
  if (view===null || view.items.length===0) return FrameResult.Present; // 空帧清屏
  set_size(240,48);
  fill_rect(0,0,240,48,0xff202020);
  draw(view.items[0].primary,8,8,0,20,0xffffffff);
  if (view.items[0].enabled) hit_region(1,0,0,240,48,0);
  return FrameResult.Present;
}
export function theme_event(kind:i32,detail:i32,_x:f32,_y:f32,_now:f64):i32 {
  switch (kind) {
    case EventKind.View:
    case EventKind.Appearance: return paint();
    case EventKind.Pointer:
      // 为简明起见示例在按下时选中；完整按钮交互应跟踪 Down/Up/Cancel。
      if (detail===PointerPhase.Down && pointer_region()===1) send_action(Action.Item,0);
      return FrameResult.Keep; // 发动作不必重绘
    case EventKind.Hide:
    case EventKind.Animation: return FrameResult.Keep;
    default: return ErrorCode.InvalidArgument;
  }
}
