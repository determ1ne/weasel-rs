/**
 * 持久表面属性：create 可初始化；事件内修改只有 Present 才生效。
 * 内容范围包含所有像素，anchor 用于定位，panel 用于材质/阴影。
 * frame_geometry 后不要再调用 set_size 覆盖它；这些坐标不继承绘制变换。
 */
export { set_size, frame_geometry, panel_bounds, set_panel, set_backdrop, set_fixed_position } from "./raw";
import * as raw from "./raw";
import { SurfaceKind } from "./types";
export { SurfaceKind } from "./types";
export function set_visible(visible:bool):void { raw.set_visible(visible?1:0); }

/** 主表面的稳定 ID。旧主题不选择表面时始终绘制到这里。 */
export const PRIMARY_SURFACE:i32 = 0;

/** 创建辅助宿主表面；返回正 ID 或负 ErrorCode。 */
export function create_surface(kind:SurfaceKind):i32 {
  return raw.surface_create(<i32>kind);
}

/** 删除辅助表面；Primary 不可删除。 */
export function destroy_surface(surface:i32):i32 {
  return raw.surface_destroy(surface);
}

/** 选择后续绘制、图层、几何与命中区调用的目标表面。 */
export function select_surface(surface:i32):i32 {
  return raw.surface_select(surface);
}

/** 当前指针事件来自哪个表面；非指针事件返回 Primary。 */
export function event_surface():i32 {
  return raw.event_surface();
}
