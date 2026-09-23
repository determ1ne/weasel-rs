# 发布更新（WinSparkle）

更新检查运行在 broker 中，TIP 语言栏和托盘的“检查更新”均转发到 broker。安装包必选安装 x64 `WinSparkle.dll`；若 DLL 或发布配置缺失，输入法仍可运行，手动检查会说明不可用。WinSparkle 在第二次启动 broker 时询问是否允许自动检查；手动检查不依赖自动检查开关。

## 一次性配置

1. 用 `scripts/download_winsparkle.ps1` 下载固定的 WinSparkle 0.9.4，脚本校验官方发布包 SHA-256。若使用默认公钥，必须配置与它配对的私钥；若另行运行 `artifacts/winsparkle/winsparkle-tool.exe generate-key --file <仓库外的私钥路径>` 生成密钥，则必须覆盖默认公钥。私钥需单独备份，**不能提交到仓库**。
2. `build-release.ps1` 未收到环境变量时默认使用 `https://rimers.sigsegv.top/appcast.xml` 和公钥 `GAmmGQQoLRtAPyTj3s2gtK9NfVnlcqPHaGJbDF3Zc9E=`；可用 GitHub 仓库变量 `WINSPARKLE_APPCAST_URL`、`WINSPARKLE_PUBLIC_KEY` 覆盖。设置 Secret `WINSPARKLE_PRIVATE_KEY`（与有效公钥对应的私钥文件原文）；Release 签名若缺少 Secret 或密钥不匹配会失败。
3. 创建单独的 Cloudflare Pages Direct Upload 项目，并安装 Wrangler。发布机准备环境变量 `CLOUDFLARE_ACCOUNT_ID`、`CLOUDFLARE_API_TOKEN`、`WINSPARKLE_PAGES_PROJECT`、`WINSPARKLE_PUBLIC_KEY`、`WINSPARKLE_APPCAST_URL`。Pages URL 应与 GitHub 仓库变量中的 feed URL 一致。Pages 项目需允许 Wrangler 部署 `main` 分支。

## 每次发布

1. 将 Cargo 版本与 `vMAJOR.MINOR.PATCH` tag 对齐并推送 tag。GitHub Actions 构建完整与 Mini 安装包，使用 Secret 对 **Mini 安装包的最终字节**签名，验证后将 `*.edSignature` 连同安装包上传到草稿 Release。
2. 检查草稿资源，正式发布 GitHub Release。发布前不要上传 appcast。
3. 运行 `./scripts/publish-appcast.ps1 -Tag vX.Y.Z -DryRun` 检查资源、签名及生成的 `artifacts/appcast/appcast.xml`；确认后运行 `./scripts/publish-appcast.ps1 -Tag vX.Y.Z` 部署到 Pages。脚本仅接受最新、非预发布且已发布的 Release，并重新下载 Mini 安装包核验签名。

Appcast 指向 GitHub Release 的 Mini 安装包；WinSparkle 校验 EdDSA 签名。安装器仍按正常交互流程运行，不传 NSIS `/S`。Feed 设置最低系统版本 Windows 10 build 17763，下载目标为 Windows x64。若未来变更签名密钥，需另行设计密钥轮换；直接覆盖已有公钥会使旧安装无法验证后续更新。
