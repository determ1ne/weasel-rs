# 小狼毫 RS

小狼毫 RS （Weasel-RS） 是面向 Windows 的 Rime Rust 前端项目。

- `tip`：输入法 TIP
- `broker`：组件协调与进程间通信
- `server`：算法服务
- `renderer`：候选词及输入界面渲染
- `common`：各组件共享的库代码

## 许可证

项目使用 [BSD-3-Clause](LICENSE) 开源许可，
第三方材料详见 [第三方许可清单](THIRD-PARTY-LICENSES.txt)。

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
