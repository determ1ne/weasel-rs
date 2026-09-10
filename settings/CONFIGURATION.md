# 配置描述文件

- `xxx.json`：默认配置的唯一来源。
- `xxx.richschema.json`：人工维护的标准约束和界面提示。
- `xxx.schema.json`：生成给编辑器使用的标准 JSON Schema，不手工修改。

运行 `node scripts/generate-schemas.mjs` 生成，传 `--check` 检查生成结果是否过期。

richschema 的 `formatVersion` 当前为 1；`schema` 使用标准 JSON Schema，
`ui.fields` 以主题局部的 JSON Pointer 为键，存放排序、控件、分组等界面提示。
字段名称、说明仍放在 schema 的 title/description，不在两处重复维护。
后续条件显示采用声明式规则，不允许描述文件执行脚本或外部命令。

主题描述不包含宿主配置路径；主题目录注册与实际挂载位置由设置应用负责。
本次仅迁移描述来源和生成流程，尚未实现动态表单及主题目录发现。
