// Generated from themes/wasm/abi.json. DO NOT EDIT.
//! ABI签名与语义编码的唯一来源；生成SDK声明和枚举，禁止手工修改生成文件。
//! 仅支持wasm32：内存参数是u32线性内存偏移/长度，仅在同步调用内有效；memory64明确拒绝。坐标DIP，颜色ARGB，字符串UTF-8，preedit光标UTF-16。
//! 必须独立导出theme_abi_version() -> i32和theme_capabilities() -> i32；前者返回2，后者返回Capability位集合。声明查询不得调用host接口。
//! 默认配置唯一来源为weasel.settings自定义段的defaults对象；宿主解析后覆盖用户设置。无段使用空对象，损坏/重复/超限报错。
//! host自动开始事件展示事务；返回FrameResult.Present发布整帧，Keep保留旧画面，负错误或trap回滚。
//! 创建返回ErrorCode.Success；可选theme_destroy释放前调用。资源随实例兜底回收。
#[link(wasm_import_module = "weasel_v2")]
unsafe extern "C" {
    /// 分组：draw。绘制状态栈，最多32层；只影响绘制，不影响panel/anchor/hit_region。
    /// 变换为局部到父坐标的仿射矩阵(m11,m12,m21,m22,dx,dy)，可嵌套。
    pub fn push_transform(m11: f32, m12: f32, m21: f32, m22: f32, dx: f32, dy: f32);
    /// 分组：draw。当前坐标系下的矩形裁剪。旋转后的裁剪使用其轴对齐包围框。
    pub fn push_clip(x: f32, y: f32, width: f32, height: f32);
    /// 分组：draw。弹出最近一次clip或transform；提交前必须全部弹出，栈空/未闭合会trap。
    pub fn pop_draw_state();
    /// 分组：view。读取ViewField。content_id保留u64位模式；无快照返回0（AsciiMode/TotalItemCount返回-1）。非法字段或索引trap。
    pub fn view_i64(field: i32, index: i32) -> i64;
    /// 分组：view。读取ViewStringField。缺失返回-1；UTF-8无终止符；capacity=0查询长度，容量不足不写入。
    pub fn view_string(field: i32, index: i32, dst: *mut u8, capacity: i32) -> i32;
    /// 分组：resources。返回正句柄或负错误码：-1缺失/-2参数/-3句柄/-6配额/-7宿主错误。
    pub fn font_create(ptr: *const u8, len: i32, size: f32, weight: i32) -> i32;
    /// 分组：resources。字体不可变；布局可跨帧复用。width/height为DIP，wrap为0或1。
    pub fn text_layout_create(font: i32, ptr: *const u8, len: i32, width: f32, height: f32, wrap: i32) -> i32;
    /// 分组：resources。加载“模块名.assets”目录内的相对PNG，不支持绝对路径、父目录、ADS和APNG。
    pub fn image_load(ptr: *const u8, len: i32) -> i32;
    /// 分组：resources。从内嵌PNG字节创建图片；不保留guest指针。编码最多8MiB，尺寸至多4096。
    pub fn image_create(ptr: *const u8, len: i32) -> i32;
    /// 分组：resources。读取ResourceMetric。无效句柄、字段或资源类型会trap。
    pub fn resource_metric(handle: i32, field: i32) -> f32;
    /// 分组：resources。释放句柄；已提交帧仍持有资源。成功0，重复释放-3；句柄不复用。
    pub fn resource_release(handle: i32) -> i32;
    /// 分组：draw。
    pub fn draw_layout(handle: i32, x: f32, y: f32, color: u32, glow: f32, glow_color: u32);
    /// 分组：draw。
    pub fn draw_image(handle: i32, x: f32, y: f32, width: f32, height: f32, opacity: f32);
    /// 分组：surface。内容表面范围(0,0,width,height)；anchor矩形决定与应用光标的对齐，不含装饰。
    pub fn frame_geometry(width: f32, height: f32, anchor_x: f32, anchor_y: f32, anchor_w: f32, anchor_h: f32);
    /// 分组：surface。独立面板矩形，仅控制材质、圆角mask和阴影。必须包含在内容表面内。
    pub fn panel_bounds(x: f32, y: f32, width: f32, height: f32);
    /// 分组：interaction。注册当前绘制目标中的局部命中区域；图层重绘清空旧区域。区域ID在目标内唯一，总数最多256；命中按图层顺序、变换和裁剪计算。
    pub fn hit_region(id: i32, x: f32, y: f32, w: f32, h: f32, radius: f32);
    /// 分组：interaction。当前指针事件命中区域；-1未命中，0默认面板。不承诺跨进程穿透。
    pub fn pointer_region() -> i32;
    /// 分组：config。按ConfigScope读取JSON Pointer，返回DataKind。
    pub fn data_kind(scope: i32, path: *const u8, len: i32) -> i32;
    /// 分组：config。字符串UTF-8字节数或容器元素数；类型不符为-1。
    pub fn data_len(scope: i32, path: *const u8, len: i32) -> i32;
    /// 分组：config。整数/bool读取，u64保留位模式；先查kind区分缺失与合法0。
    pub fn data_i64(scope: i32, path: *const u8, len: i32) -> i64;
    /// 分组：config。数值读取；缺失或类型错误返回NaN。
    pub fn data_number(scope: i32, path: *const u8, len: i32) -> f64;
    /// 分组：config。复制UTF-8，无终止符；容量不足不写入，返回所需字节数；类型错误为-1。
    pub fn data_string(scope: i32, path: *const u8, len: i32, dst: *mut u8, capacity: i32) -> i32;
    /// 分组：draw。绘制坐标为DIP，颜色为非预乘ARGB；宿主负责转换为目标像素格式。
    pub fn fill_rect(x: f32, y: f32, w: f32, h: f32, color: u32);
    /// 分组：draw。
    pub fn fill_rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32, color: u32);
    /// 分组：draw。
    pub fn stroke_rect(x: f32, y: f32, w: f32, h: f32, color: u32, width: f32);
    /// 分组：surface。原生圆角、阴影半径、偏移和ARGB颜色；阴影半径0禁用。
    pub fn set_panel(radius: f32, shadow_radius: f32, offset_x: f32, offset_y: f32, color: i32);
    /// 分组：surface。宿主材质合成，三个非负权重之和必须为1；不可用时使用不透明回退色。
    pub fn set_backdrop(enabled: i32, tint: i32, blur_sigma: f32, backdrop_balance: f32, afterglow_balance: f32, color_balance: f32, fallback_color: i32);
    /// 分组：surface。随帧提交可见性，0隐藏1显示；主机失焦隐藏优先于主题。
    pub fn set_visible(visible: i32);
    /// 分组：surface。固定在主屏工作区，x/y为DIP偏移；窗口移动由host管理。
    pub fn set_fixed_position(x: f32, y: f32);
    /// 分组：interaction。仅当前pointer-down可请求原生拖动固定窗口。
    pub fn begin_drag();
    /// 分组：surface。声明内容尺寸DIP，不含宿主阴影；上限8192。
    pub fn set_size(w: f32, h: f32);
    /// 分组：interaction。按Action请求语义动作，仅允许PointerPhase.Down/Up，绑定已展示快照。
    pub fn send_action(action: i32, index: i32);
    /// 分组：animation。合并的下一动画帧请求；未再次请求则停止。
    pub fn request_frame();
    /// 分组：animation。宿主进程单调毫秒，同theme_event的now；不是Unix日期。
    pub fn time_ms() -> f64;
    /// 分组：diagnostics。普通日志，level为LogLevel；UTF-8最多4096字节，不弹用户通知。
    pub fn log(level: i32, ptr: *const u8, len: i32);
    /// 分组：diagnostics。需要用户处理的问题，进入broker通知通道；不要用于调试日志。
    pub fn report_notice(ptr: *const u8, len: i32);
    /// 分组：animation。请求单调毫秒 deadline 后唤醒；重复请求取最早时间，不补发过期帧，最多24小时。
    pub fn request_wakeup(deadline_ms: f64);
    /// 分组：animation。取消待处理 WASM 唤醒；不停止独立的宿主图层动画。
    pub fn cancel_wakeup();
    /// 分组：layers。选择并清空此装饰层命令，正ID稳定且最多8层；id=0回到主画面。只能在事件内调用，切换时绘图栈必须平衡。
    pub fn layer_content(id: i32, width: f32, height: f32);
    /// 分组：layers。移除装饰层及动画；随Present生效。
    pub fn layer_remove(id: i32);
    /// 分组：layers。立即设置属性并替换该属性动画；随Present生效。
    pub fn layer_set(id: i32, property: i32, value: f32);
    /// 分组：layers。从当前显示值转向新目标；同属性替换不排队。duration_ms最多3600000；时长为零时立即到目标。
    pub fn layer_animate(id: i32, property: i32, to: f32, duration_ms: f64, easing: i32);
    /// 分组：layers。停止属性动画；behavior为LayerStop。
    pub fn layer_stop(id: i32, property: i32, behavior: i32);
    /// 分组：layers。设置内容坐标系中的固定矩形裁剪；enabled=0取消。裁剪不随图层变换，影响绘制与命中。
    pub fn layer_clip(id: i32, enabled: i32, x: f32, y: f32, width: f32, height: f32);
    /// 分组：layers。显式启停图层交互，默认关闭，与透明度独立。图层内hit_region为局部坐标，随位移和缩放命中。
    pub fn layer_interactive(id: i32, enabled: i32);
    /// 分组：interaction。当前鼠标事件命中图层，0表示主画面或无命中。区域ID只需在各图层内唯一。
    pub fn pointer_layer() -> i32;
    /// 分组：layers。设置图层叠放顺序，默认0，支持有符号i32，越大越靠前；相同值按创建顺序。仅对图层排序，始终在主画面上方。Present提交，不重建内容或动画。
    pub fn layer_z_index(id: i32, z_index: i32);
}
