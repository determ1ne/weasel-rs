# AGENTS.md

## 项目概览

小狼毫 RS 是仅支持 Windows 的 Rime TSF 输入法，主要使用 Rust 2024 edition，并包含 NSIS 安装器、原生主题 DLL、WASM 主题和 Slint 设置应用。

仓库只以 Windows 为目标平台。不要为非 Windows 平台添加空实现或散布 `#[cfg(windows)]`。

## Workspace 结构

- `tip/`：加载进宿主进程的 TSF DLL。负责 TSF/COM 生命周期、按键、composition、布局、语言栏和与 Server 的 RPC。
- `server/`：Rime 算法服务及部署逻辑，拥有 librime 状态并产生输入结果和渲染快照。
- `broker/`：进程与生命周期管理、托盘菜单、配置加载、通知、更新和子进程所有权。
- `renderer/`：候选窗运行时、通信、主题选择和动态主题 DLL 加载。
- `settings/`：设置应用。
- `common/`：跨组件消息、protobuf、RPC、日志、Windows 安全/COM 运行时及共享工具。
- `theme-api/`：Renderer 与原生主题 DLL 之间的统一接口和数据模型。
- `themes/support/`：原生主题共享的 Windows/D2D 支持。
- `themes/{ten,eleven,abc,void}/`：独立编译的原生主题 DLL。
- `themes/wasm/`：WASM 宿主、ABI、Rust/AssemblyScript SDK 和内置 WASM 主题。
- `installer/`：NSIS 安装/卸载逻辑。
- `scripts/`：依赖下载、代码生成、发布构建、安装器、签名和 appcast 工具。
- `assets/`：图标、Rime 数据等安装资源。
- `target/`、`artifacts/`、各 SDK 的 `node_modules/` 或 `target/`：生成物，不作为源码编辑。

## 项目约定

1. 术语约定：专用术语使用英文单词，如 ticket 不要翻译为票据、 COM compartment 不要翻译为公寓。
2. 注释风格：使用简洁的中文进行注释。修复我们之前的 bug 时，不要在注释中编写修复历史问题的理由，除非我们新添加的代码为了解决某个特定问题而令人困惑，必须要进行解释。
3. 程序中避免 magic number；优先使用 Windows 常量、protobuf 枚举或有语义的本地常量。
4. 避免为一次性细节或其他简单的代码加入大量测试。
5. TIP 运行在任意宿主进程中，禁止 `unwrap`、`expect` 和 `panic`；COM 边界必须把错误和 panic 隔离在 DLL 内。
6. 修改与 tip 相关的 proto 文件时，需要保持向前兼容性，避免更新程序后，带有旧 tip 的程序无法正常运行或崩溃。

## 配置与 schema

配置由 Broker 从安装目录的 `weasel.json` 读取，再用用户数据目录中的 `weasel.custom.json` 覆盖。
消费者负责验证自己使用的值、记录问题并采用回退值；不要让通用 overlay 层替所有组件解释业务枚举。

配置元数据约定：

- `*.json`：默认值。
- `*.richschema.json`：人工维护的约束、中文说明和设置 UI 元数据，是 schema 的源文件。
- `*.schema.json`：生成的标准 JSON Schema，禁止手工编辑。

默认配置或 richschema 改动后运行：

```powershell
node .\scripts\generate-schemas.mjs
node .\scripts\generate-schemas.mjs --check
```

主题专属设置位于 `themeSettings.<theme>`。WASM 模块的元数据会随模块打包并由设置应用热加载。

## WASM ABI 与 SDK

`themes/wasm/abi.json` 是 WASM ABI 的唯一声明来源。生成器不解析 Rust 或 AssemblyScript 源码。
当前可以不保证 ABI 稳定或向前/向后兼容。修改 ABI 后运行：

```powershell
node .\scripts\generate-wasm-abi.mjs
node .\scripts\generate-wasm-abi.mjs --check
```

## 构建与验证

按改动范围选择最小但充分的验证。常用命令：

```powershell
# 格式与快速检查
cargo fmt --all
cargo check -p <package>
cargo test -p <package>

# 全 workspace（耗时较长）
cargo check --workspace --locked
cargo test --workspace --locked --no-fail-fast

# 检查生成文件是否同步
node .\scripts\generate-schemas.mjs --check
node .\scripts\generate-wasm-abi.mjs --check

# 本地发布构建；-Dev 保留 release 产物但启用 Rust incremental
.\scripts\build-release.ps1 -Dev
.\scripts\build-release.ps1 -Dev -SkipThemeWasm
.\scripts\build-release.ps1 -Architecture arm64 -Dev

# 在 release 产物齐全后构建安装器
.\scripts\build-installer.ps1 -Dev
.\scripts\build-installer.ps1 -Mini -Dev
.\scripts\build-installer.ps1 -Architecture arm64 -Dev
```
