/**
 * 持久表面属性：create 可初始化；事件内修改只有 Present 才生效。
 * 内容范围包含所有像素，anchor 用于定位，panel 用于材质/阴影。
 * frame_geometry 后不要再调用 set_size 覆盖它；这些坐标不继承绘制变换。
 */
export { set_size, frame_geometry, panel_bounds, set_panel, set_backdrop, set_fixed_position } from "./raw";
import * as raw from "./raw";
export function set_visible(visible:bool):void { raw.set_visible(visible?1:0); }
