# 配置

broker 启动时读取安装目录的 `weasel.json`，再用用户数据目录的
`weasel.custom.json` 覆盖。对象递归合并，数组和其他值整体替换。
缺失文件使用已有默认值；格式或主题值错误的文件会被忽略，并记录到 broker 日志。
配置文件使用 UTF-8，允许 BOM，大小上限为 1 MiB。

用户数据目录通常是 `%APPDATA%\Weasel-RS\Rime`。程序目录存在 `.dev` 时，
改用程序目录下的 `user-data`。安装和升级不会创建或覆盖用户配置。

例如，在 `weasel.custom.json` 中优先选择 Direct2D 主题：

```json
{
  "theme": "ten"
}
```

`theme` 可选 `eleven`（默认，XAML）和 `ten`（Direct2D）。renderer 启动时
通过 Protobuf RPC 向 broker 查询配置，优先初始化所选主题，失败后尝试另一个。
无法查询 broker 时，最多等待 2 秒，然后按 `eleven`、`ten` 的顺序初始化。
配置修改后需退出并重新启动 broker；当前不支持热重载。
