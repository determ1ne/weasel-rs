# WASM 主题 SDK：动画与装饰图层

生命周期、完整绘制和资源说明见 `assembly/index.ts`。新主题按模块导入：
`assembly/animation` 提供时间工具，`assembly/layers` 提供保留装饰层。
两个模块不在根入口通配导出，避免与旧的 `request_frame` 等名称冲突。
使用 `new Timeline(start_ms, duration_ms)`、`new Tween(value)` 构造时间工具。
AS 的 with_layer 回调写作 `(): void => { /* 绘制 */ }`；不能捕获局部变量，需要时使用模块状态。

## 时间与调度

事件的 `now` 和 animation 模块的 `time_ms()` 使用同一个宿主单调时钟，单位是绝对毫秒。
按实际经过的时间采样，不使用墙上时钟，也不按回调次数累加假定帧间隔。
`request_frame()` 请求 Animation 事件；`request_wakeup(deadline_ms)` 接收绝对截止时间，不是延迟量。
待处理请求取最早时间，后续较晚请求不能推迟它。`cancel_wakeup()` 清除全部待处理 WASM 唤醒
（包括帧请求）；取消后可以重新请求。Hide 自动取消，Keep 保留请求，过期排队定时器会被忽略。
宿主最小调度间隔为 16 ms，不保证精确定时。SDK 忽略非有限或负截止时间；超过当前时间 24 小时的截止时间会在宿主 trap。

主题若提供自己的动画开关，应主动选择静态/最终画面并取消待处理唤醒。
时间数学工具不访问宿主，不自动请求帧。

Timeline 提供 `progress(now)`、`finished(now)` 和 `restart(start_ms, duration_ms)`。
Tween 还提供 `value(now)`、`snap(value)`、`retarget(target, now, duration_ms, easing)`。
retarget 从当前采样值转向新目标，保持数值连续，不保证速度连续；只在目标变化时调用，不要每帧重启。

独立函数 `progress(now, start, duration)` 将进度限制到 [0,1]；非有限时刻、非有限时长或时长 <= 0 立即完成。
`lerp(from, to, t)`、`ease(t, easing)` 同样限制进度，非有限进度视为 1。
非有限 from 归零，非有限 to 使用归一化后的 from；Tween 构造/snap 的非有限值归零，retarget 的无效目标保持当前值。
缓动直接使用生成的 Easing，与原生图层一致：Linear=0（t）、SmoothStep=1（t*t*(3-2*t)）、EaseIn=2（t*t）、EaseOut=3（t*(2-t)）。

animated 示例展示完整主画面重绘：每次 Animation 读取最新视图，重建全部主绘制命令和命中区域，返回 Present。
空视图/隐藏时取消并重置。每段过渡持续 250 ms，结束后休眠至下一个绝对 750 ms 相位边界。
直接向主画面绘制时，Present 替换主命令流；若只画移动装饰，旧文字会被清掉。这不适用于下面的仅图层更新。

## 保留装饰图层

layers 模块提供 `with_layer`、`layer_set`、`layer_animate`、`layer_stop`、`layer_remove`，
并重导出生成的 LayerProperty、LayerStop、Easing 枚举。
`with_layer(id, width, height, callback)` 选择并替换图层内容，回调返回时切回主画面。
不要嵌套作用域；进入前和回调返回前，裁剪/变换栈都必须平衡。回调内可声明局部命中区域，但不能修改窗口属性。
宿主在每个事件开始时重置绘制目标，trap 后也如此。

ID 可以是任意正 i32，最多同时保留 8 层，宽高为正数，跨 Present 保留直到移除。
`layer_z_index(id, z)` 设置层级（有符号 i32，默认0）：越大越靠前，同值按创建顺序。绘制与命中共用此顺序；重绘不改变创建顺序，删除重建则获得新的顺序。负值也只影响图层之间的顺序，所有图层仍在主画面之上。修改层级不会重建表面或重启动画，随 Present 提交。
图层可用于装饰或交互内容。图层 API 仅在事件内调用；Present 提交，Keep/错误/trap 丢弃编辑，WASM 自身变量不会回滚。

`layer_clip(id,x,y,w,h)` 设置内容坐标系的固定裁剪，`layer_clear_clip(id)` 清除；不随图层变换，同时限制绘制与命中。
`with_layer` 中的 `hit_region` 使用局部坐标，重绘时清除旧区域。用 `layer_interactive(id,true)` 启用交互（默认关闭，与透明度独立）。区域ID在各目标内唯一，总数最多256个。
鼠标回调使用 `pointer_layer()` + `pointer_region()` 判断目标；宿主逆变换位移与缩放并按图层顺序命中，零缩放不可点击。命中根据单调时钟近似合成器位置，可能与屏幕呈现帧有少量时间差。
删除/禁用图层、隐藏和新候选快照使旧按下目标失效；主题仍须处理 Leave/Cancel 的 hover/pressed。退场页显式禁用交互。原生翻页只绘制新旧页一次，通过 OffsetX/Opacity 动画呈现，结束后唤醒一次移除旧页，无需16ms重绘。

仅编辑图层、没有主绘制的 Present 保留原主命令、视图/动作身份及命中区域；包含主绘制的 Present 则照常替换主画面和命中区域。
首次 View 必须绘制主画面。Hide 清空保留图层，主题需清除“图层已存在”标记，在下次显示时重建。
若移除图层的同时要清空主画面，可像 layer-pulse 示例一样显式绘制透明主矩形，触发主画面替换。

`layer_set` 立即设置属性并替换该属性动画；`layer_animate` 从当前显示值转向新目标，同属性替换、不排队，无需 WASM 帧循环。
同一事件内对同一属性的重复编辑以最后一次目标/动画编辑为准，不能用连续 set/animate 表达多个阶段。
属性为 Opacity=0、OffsetX=1、OffsetY=2、ScaleX=3、ScaleY=4；`layer_stop` 的行为为 Current=0 或 End=1。
参数非法会 trap；动画时长必须有限、非负且不超过 3,600,000 ms。时长为零时立即到达目标。
`cancel_wakeup()` 只取消 WASM 回调，不停止原生图层动画。
layer-pulse 示例仅在创建时设置初始透明度，后续 View 在 1 和 0.25 之间切换目标，从当前显示值连续转向。
每次过渡持续 450 ms，不自动循环，也不请求完成回调。

字体/图片跨帧缓存，文本或格式变化时才更新布局。示例使用 draw 的有界缓存，仅改颜色会复用布局。
不要每次 Animation 调用 set_font、加载 PNG 或重建布局；手动 Rust 资源使用 Drop，AS 资源替换时调用 dispose，不要无限缓存文本。

## 示例与构建

```ts
import { Tween, Easing } from "../assembly/animation";
const opacity = new Tween(0);
opacity.retarget(1, 1000, 200, Easing.SmoothStep);
assert(opacity.value(1100) == 0.5);
opacity.retarget(0, 1100, 200, Easing.EaseOut);
assert(opacity.value(1100) == 0.5);
```

`npm run asbuild` 构建原有示例及 `examples/animated.ts`、`examples/layer-pulse.ts`；
也可单独运行 `npm run asbuild:animated` 或 `npm run asbuild:layer-pulse`。
`npm test` 保持原最小示例回归测试，`npm run test:animation` 检查时间数学、调度封装和图层作用域。
