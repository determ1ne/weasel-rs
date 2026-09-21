# WASM 主题 SDK：从这里开始

普通候选主题只需要 lifecycle、config、view、draw、surface、interaction。
底层 raw 是高级入口，不需要在普通主题中操作指针或句柄。

## 默认配置打包

`config.json` 保存默认值，`config.richschema.json` 保存约束和设置界面说明。
两者作为 `weasel.settings` 自定义段中的 `defaults` / `richschema` 一起打包，运行时不调用主题函数获取JSON。
AS预置主题由 `npm run build` 自动打包。Rust的Orbit通过 `build_support/wasm_theme_metadata.rs`
在构建时生成静态自定义段，直接 `cargo build` 即可；参见其build.rs和include声明。
其他独立主题也可以在编译后执行 `node scripts/package-theme-metadata.mjs --wasm module.wasm config.json`。
重复打包会替换原段，不会叠加；运行时拒绝重复段、非法JSON/格式和超限元数据。
无段的主题默认值为空对象。主题仍应校验用户设置，必要时使用字段级回退；无需再内嵌完整JSON。

## 生命周期

读取元数据 → 检查ABI版本 → 查询能力 → 宿主合并配置 → theme_create → theme_event（多次）→ theme_destroy（可选）

- theme_abi_version() → i32：返回 ABI_VERSION（仍为2）。
- theme_capabilities() → i32：返回 Capability 位集合，与版本独立；查询无副作用、不依赖创建。
  默认值从 weasel.settings 自定义段读取并由宿主合并；无元数据使用空对象，损坏元数据报错。
  不要在声明查询或模块顶层初始化时调用宿主接口。当前只支持wasm32，内存参数为u32偏移/长度；memory64被明确拒绝。
- theme_create(mode, dark)：读取配置、缓存字体/图片、初始化主题状态；返回 ErrorCode.Success。
  此时不能绘图。初始化失败返回负 ErrorCode。
- theme_event(kind, detail, x, y, now)：按 EventKind 分派，见下表。
- theme_destroy（可选）：清理主题状态；也可能在 create 失败后调用。
  不依赖它成功执行：宿主会兜底回收资源。

| 事件 | 典型处理 | 返回 |
| --- | --- | --- |
| View | 读取最新 View；排版、设置尺寸、绘制完整画面、声明命中区域 | Present |
| Appearance | detail 为深色标志；更新配色，有可用视图时完整重绘 | Present 或 Keep |
| Pointer | detail 为 PointerPhase；处理命中、按下/释放/取消；需要时重绘 | Keep 或 Present |
| Hide | 清除按下、悬停等临时状态；宿主强制隐藏 | 通常 Keep |
| Animation | 用单调时钟采样动画、读取最新视图并完整重绘 | Keep 或 Present |

事件不保证总是先有可用候选内容；视图可能缺失或候选数为零。
指针坐标是内容区域内的 DIP，不能当作屏幕坐标。Cancel/Leave 不应执行点击动作。

## 一次完整绘制

1. 从 view 读取输入状态，不修改宿主的候选数据。
2. 用缓存字体和 TextLayout 测量，计算内容大小。
3. surface.set_size 设置普通矩形；异形窗口改用 frame_geometry + panel_bounds。
4. draw 绘制背景、文字、图片；interaction.hit_region 声明本帧交互区域。
5. 返回 FrameResult.Present。

普通 Present 替换主命令流和命中区域；仅含图层编辑且无主绘制的 Present 保留旧主画面、视图/动作身份和命中区域。
首次 View 必须绘制主画面。无图层编辑、无绘制命令的普通 Present 表示清屏。
Keep 丢弃本次展示修改，保留旧画面和旧动作身份，但仍允许合法动作、日志及请求下一帧。
负错误码、非法返回值或 trap 丢弃展示修改与动作，不回滚 WASM 自己的变量及资源操作。
没有 frame_begin/frame_submit；宿主自动管理事务。裁剪/变换栈必须配对恢复，即使返回 Keep。

## 分组与状态寿命

| 模块 | 用途 / 使用时机 |
| --- | --- |
| lifecycle | 版本、能力、事件和返回码；入口函数签名见最小示例 |
| config | create/事件中读取已合并的主题 options/OPTIONS 和全局 settings/SETTINGS；路径是 JSON Pointer，不是 jq |
| view | 在事件中读取当前快照；发送候选动作时宿主绑定已展示快照 |
| resources | Font、TextLayout、Image；资源可跨帧缓存 |
| draw | 仅 theme_event 内绘制；命令与绘制栈每次事件重新开始 |
| surface | 尺寸、定位、面板、材质、可见性；设置跨帧保留，事件内修改须 Present 才生效 |
| interaction | 每次 Present 重建命中区域；send_action 只允许 Pointer Down/Up，begin_drag 只允许 Down |
| diagnostics | 普通日志不弹窗；report_notice 用于需要用户处理的问题 |
| raw | 原始 ABI，仅高级用法；签名由 abi.json 生成 |

资源不是每帧重新加载：字体/图片通常在 create 缓存，文本变化时创建布局，悬停重绘复用布局。
Rust 资源用 Drop；AS 不再使用时调用 dispose。检查 Rust Result / AS valid 后再绘制。
宿主保留已提交画面的资源引用，主题实例销毁时最终兜底释放。不要无限缓存文本。

## 坐标、颜色与异形窗口

长度为 DIP；颜色为 0xAARRGGBB；preedit 光标是 UTF-16 单元，其余跨边界字符串为 UTF-8。
内容区域包含所有像素；anchor 决定相对输入位置的定位；panel 决定材质/阴影范围。
例如内容 360×80，panel/anchor 为 (0,24,280,48)，图片可绘制到 (260,0,100,80)。
图片可以超出 panel，但不能超出内容区域；设置 frame_geometry 后不要再用 set_size 覆盖它。
图片相对路径从模块旁的“模块名.assets”目录读取，不允许任意文件访问。
绘制命令的变换/裁剪不影响 panel、anchor 或命中区域；主区域使用内容坐标，图层区域使用局部坐标，由图层变换与固定裁剪统一处理。
透明装饰不是跨进程鼠标穿透承诺。

## 阅读顺序

先读 examples/quickstart.ts（AS）或 examples/minimal.rs（Rust），再按功能查看分组模块。
AS 的 examples/minimal.ts 是较完整的列表交互示例，并非最短入门示例。
旧的根入口/host 平铺导出暂保留供内置主题使用，新主题优先按模块导入。

## 动画模块入口

通过 `animation::{Timeline, Tween, Easing}` 导入时间工具，以 `Timeline::new(start_ms, duration_ms)`、
`Tween::new(value)` 构造。调度函数也从 animation 模块导入；旧根入口的 request_frame/time_ms 保持不变。
图层从 layers 模块导入，作用域写作 `with_layer(id, width, height, || { /* 绘制 */ })`。

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

### 裁剪与交互

`layer_clip(id, Some([x,y,w,h]))` 设置内容坐标系的固定矩形裁剪，`None` 清除；它不随该层位移/缩放，绘制和命中均受其约束。
在 `with_layer` 中调用 `hit_region` 注册局部坐标区域，之后用 `layer_interactive(id, true)` 启用交互（默认关闭）。重绘图层会清除旧区域，区域ID只须在各目标内唯一；主画面和所有图层合计最多256个。
图层变换由宿主逆变换命中，绘制命令中的 push_transform 不会变换区域。图层尺寸之外不可命中，缩放为零时不可命中。透明度与交互开关独立；退场页必须显式关闭交互。
鼠标事件用 `pointer_layer()` + `pointer_region()` 区分目标，0层是主画面。顶层优先；无区域命中的装饰层不阻挡下层。删除/禁用图层、隐藏或切换候选快照会失效旧按下目标；主题仍应在 Leave/Cancel 清除自身 hover/pressed 状态。
命中依据宿主单调时钟估算当前动画，不读取GPU呈现帧，可能有少量时间差。主题不需要逐帧更新点击区域。
原生翻页推荐：新旧页各绘制一次，固定裁剪，旧页禁用交互，以 layer_animate 改 OffsetX/Opacity，结束时仅唤醒一次移除旧页。没有内容变化就不调用 with_layer，不需要16ms循环。

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

## 动画示例构建

`examples/animated.rs` 演示主画面完整重绘，`examples/layer-pulse.rs` 演示有限原生图层动画。
从仓库根目录运行
`cargo build --manifest-path themes/wasm/sdk-rust/Cargo.toml --examples --target wasm32-unknown-unknown`。
时间数学和 retarget 连续性由 SDK 单元测试覆盖。
