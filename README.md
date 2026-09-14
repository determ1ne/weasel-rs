# 小狼毫 RS 输入法

基于 中州韵输入法引擎 / Rime Input Method Engine 的 Windows 输入法

项目主页：[https://rime.im](https://rime.im)

项目使用 [BSD-3-Clause](LICENSE) 开源许可，

本项目基于诸多开源组件及技术，其许可清单见 [第三方许可清单](THIRD-PARTY-LICENSES.txt)。

下载最新版本：[Releases](https://github.com/determ1ne/weasel-rs/releases)

## 安装与使用

软件适用于 Windows 10 (1809 版本及以上) ~ Windows 11，
部分功能要求具有 Windows 10 (版本 1903) 或更新版本的操作系统。

安装完成后，选择*输入法指示器*中的 Rime 图标，开始使用小狼毫进行输入。
右键输入法状态指示器 / 系统*托盘区* Rime 图标，
可通过设置 / 用户文件夹对小狼毫及 Rime 输入引擎进行自定义配置。

Rime 配置决定输入法的输入方案与交互，见 [Rime定制指南](https://github.com/rime/home/wiki/CustomizationGuide) 进行配置。

小狼毫配置决定输入法的外观、与操作系统和应用的交互方式，见程序附带【设置】应用或源码中的配置文件 schema 进行配置。

小狼毫 RS **不兼容** 小狼毫 (C/C++版) 的配置文件 (weasel.yaml/weasel.custom.yaml) 。

## 自定义皮肤

小狼毫通过 wasm 提供自定义皮肤接口，但目前并不保证接口稳定。
`themes/wasm/` 下包括 SDK 及自带主题源码，可供参考。

## 构建

开发构建：

```powershell
# 首先下载构建所需要的组件
.\scripts\download_librime.ps1
.\scripts\download_vcredist.ps1
cargo build --workspace --release
```

发布构建：

```powershell
# 构建输入法及安装包
.\scripts\build-release.ps1
.\scripts\build-installer.ps1
```
