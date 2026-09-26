/**
 * 小狼毫 RS 的 AssemblyScript WASM 主题 SDK。
 *
 * 生命周期为：宿主查询 ABI 与能力 → `theme_create` 初始化配置和资源 → 多次
 * `theme_event` → 可选 `theme_destroy`。宿主自动管理每个事件的展示事务；主题返回
 * `FrameResult.Present` 才提交本次画面，返回 `Keep` 则保留上一帧。
 *
 * `theme_abi_version` 和 `theme_capabilities` 是无副作用的声明查询，不能调用宿主 API；
 * `theme_create` 用于读取配置、注册字体和加载长寿命资源，不能绘图。事件的典型处理为：
 *
 * - `View`：读取最新快照，排版并绘制完整画面，返回 `Present`；
 * - `Appearance`：更新深浅色资源，有画面时完整重绘；
 * - `Pointer`：根据 `PointerPhase` 更新悬停/按下状态或发送语义动作；
 * - `Hide`：清除临时交互状态，宿主负责强制隐藏；
 * - `Animation`：按单调时钟推进动画，必要时提交新画面。
 *
 * `Present` 会提交本事件的主画面、命中区域和表面修改；`Keep` 丢弃这些暂存修改并保留
 * 上一帧。负错误码、非法返回值或 trap 同样不会发布半帧。宿主自动开始和结束事务，
 * 不存在需要主题配对调用的 `frame_begin` / `frame_submit`。裁剪与变换栈仍须在回调返回
 * 前恢复平衡。
 *
 * 推荐按职责从本入口导入生命周期、配置、视图、绘制、表面、交互、诊断和资源 API。
 * 动画与保留图层分别从 `assembly/animation`、`assembly/layers` 导入。原始 ABI 位于
 * `assembly/raw`，只供 SDK 实现或确实需要底层能力的主题使用，不从公共根入口转出。
 *
 * 字体在 `theme_create` 中通过 `set_font(slot, family, weight)` 配置；保存其返回的
 * `FontSlot`，后续把句柄传给 `measure_text`、`line_height` 和 `draw_text`。配置路径使用
 * JSON Pointer。每次完整绘制先读取 `View`，再设置表面尺寸、绘制并声明命中区域。
 * 字体、图片和 `TextLayout` 可跨帧缓存；AS 资源不再使用时调用 `dispose`，宿主会在
 * 主题实例销毁时兜底回收。长度统一使用 DIP，颜色为非预乘 `0xAARRGGBB`，preedit
 * 光标是 UTF-16 单元索引，跨 ABI 的字符串为 UTF-8。
 *
 * 异形窗口使用 `frame_geometry` 定义完整透明表面、`panel_bounds` 定义材质与阴影矩形；
 * 图片可超出 panel，但不能超出完整表面。命中区域使用内容或图层局部坐标，不继承绘制
 * 变换。所有资源、图层、命令、配置路径和字符串复制都受宿主配额约束。
 */
export * from "./lifecycle";
export * from "./config";
export * from "./view";
export * from "./draw";
export * from "./surface";
export * from "./interaction";
export * from "./diagnostics";
export * from "./resources";
