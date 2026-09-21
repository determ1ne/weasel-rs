/**
 * # WASM 主题 SDK：从这里开始
 *
 * 普通候选主题只需要 lifecycle、config、view、draw、surface、interaction。
 * 底层 raw 是高级入口，不需要在普通主题中操作指针或句柄。
 *
 * ## 生命周期
 *
 * 配置：config.json与config.richschema.json通过构建脚本打包到weasel.settings自定义段。
 * AS预置主题的npm run build已包含打包；独立主题可使用scripts/package-theme-metadata.mjs。
 * 没有声明式配置的主题可省略该段；不要通过导出函数传递JSON地址。
 *
 * 读取元数据 → 检查ABI版本 → 查询能力 → 宿主合并配置 → theme_create → theme_event（多次）→ theme_destroy（可选）
 *
 * - theme_abi_version() → i32：返回 ABI_VERSION（仍为2）。
 * - theme_capabilities() → i32：返回 Capability 位集合，与版本独立；查询无副作用、不依赖创建。
 *   默认值从 weasel.settings 自定义段读取并由宿主合并；无元数据使用空对象，损坏元数据报错。
 *   不要在声明查询或模块顶层初始化时调用宿主接口。当前只支持wasm32，内存参数为u32偏移/长度；memory64被明确拒绝。
 * - theme_create(mode, dark)：读取配置、缓存字体/图片、初始化主题状态；返回 ErrorCode.Success。
 *   此时不能绘图。初始化失败返回负 ErrorCode。
 * - theme_event(kind, detail, x, y, now)：按 EventKind 分派，见下表。
 * - theme_destroy（可选）：清理主题状态；也可能在 create 失败后调用。
 *   不依赖它成功执行：宿主会兜底回收资源。
 *
 * | 事件 | 典型处理 | 返回 |
 * | --- | --- | --- |
 * | View | 读取最新 View；排版、设置尺寸、绘制完整画面、声明命中区域 | Present |
 * | Appearance | detail 为深色标志；更新配色，有可用视图时完整重绘 | Present 或 Keep |
 * | Pointer | detail 为 PointerPhase；处理命中、按下/释放/取消；需要时重绘 | Keep 或 Present |
 * | Hide | 清除按下、悬停等临时状态；宿主强制隐藏 | 通常 Keep |
 * | Animation | 用单调时钟采样动画、读取最新视图并完整重绘；见 animation 模块和 README.md | Keep 或 Present |
 *
 * 事件不保证总是先有可用候选内容；视图可能缺失或候选数为零。
 * 指针坐标是内容区域内的 DIP，不能当作屏幕坐标。Cancel/Leave 不应执行点击动作。
 *
 * ## 一次完整绘制
 *
 * 1. 从 view 读取输入状态，不修改宿主的候选数据。
 * 2. 用缓存字体和 TextLayout 测量，计算内容大小。
 * 3. surface.set_size 设置普通矩形；异形窗口改用 frame_geometry + panel_bounds。
 * 4. draw 绘制背景、文字、图片；interaction.hit_region 声明本帧交互区域。
 * 5. 返回 FrameResult.Present。
 *
 * 普通 Present 替换主命令流和命中区域；仅含图层编辑且无主绘制的 Present 保留旧主画面、视图/动作身份和命中区域。
 * 首次 View 必须绘制主画面。无图层编辑、无绘制命令的普通 Present 表示清屏。
 * Keep 丢弃本次展示修改，保留旧画面和旧动作身份，但仍允许合法动作、日志及请求下一帧。
 * 负错误码、非法返回值或 trap 丢弃展示修改与动作，不回滚 WASM 自己的变量及资源操作。
 * 没有 frame_begin/frame_submit；宿主自动管理事务。裁剪/变换栈必须配对恢复，即使返回 Keep。
 *
 * ## 分组与状态寿命
 *
 * | 模块 | 用途 / 使用时机 |
 * | --- | --- |
 * | lifecycle | 版本、能力、事件和返回码；入口函数签名见最小示例 |
 * | config | create/事件中读取已合并的主题 options/OPTIONS 和全局 settings/SETTINGS；路径是 JSON Pointer，不是 jq |
 * | view | 在事件中读取当前快照；发送候选动作时宿主绑定已展示快照 |
 * | resources | Font、TextLayout、Image；资源可跨帧缓存 |
 * | draw | 仅 theme_event 内绘制；命令与绘制栈每次事件重新开始 |
 * | surface | 尺寸、定位、面板、材质、可见性；设置跨帧保留，事件内修改须 Present 才生效 |
 * | interaction | 每次 Present 重建命中区域；send_action 只允许 Pointer Down/Up，begin_drag 只允许 Down |
 * | diagnostics | 普通日志不弹窗；report_notice 用于需要用户处理的问题 |
 * | animation | 独立导入；Timeline/Tween、缓动、帧请求及绝对时间唤醒；见 examples/animated.ts |
 * | layers | 独立导入；with_layer 作用域及保留装饰层属性动画；见 examples/layer-pulse.ts |
 * | raw | 原始 ABI，仅高级用法；签名由 abi.json 生成 |
 *
 * 资源不是每帧重新加载：字体/图片通常在 create 缓存，文本变化时创建布局，悬停重绘复用布局。
 * Rust 资源用 Drop；AS 不再使用时调用 dispose。检查 Rust Result / AS valid 后再绘制。
 * 宿主保留已提交画面的资源引用，主题实例销毁时最终兜底释放。不要无限缓存文本。
 *
 * ## 坐标、颜色与异形窗口
 *
 * 长度为 DIP；颜色为 0xAARRGGBB；preedit 光标是 UTF-16 单元，其余跨边界字符串为 UTF-8。
 * 内容区域包含所有像素；anchor 决定相对输入位置的定位；panel 决定材质/阴影范围。
 * 例如内容 360×80，panel/anchor 为 (0,24,280,48)，图片可绘制到 (260,0,100,80)。
 * 图片可以超出 panel，但不能超出内容区域；设置 frame_geometry 后不要再用 set_size 覆盖它。
 * 图片相对路径从模块旁的“模块名.assets”目录读取，不允许任意文件访问。
 * 绘制命令变换不影响命中；主区域是内容坐标，图层区域是局部坐标，由图层变换与固定裁剪处理。
 * 透明装饰不是跨进程鼠标穿透承诺。
 *
 * ## 阅读顺序
 *
 * 先读 examples/quickstart.ts（AS）或 examples/minimal.rs（Rust），再按功能查看分组模块。
 * AS 的 examples/minimal.ts 是较完整的列表交互示例，并非最短入门示例。
 * 旧的根入口/host 平铺导出暂保留供内置主题使用，新主题优先按模块导入。
 */
// Public SDK surface. Import from @weasel-rs/sdk-as/assembly.
export * from "./host";
export * from "./data";
export * from "./view";
export * from "./graphics";
export * from "./resources";
