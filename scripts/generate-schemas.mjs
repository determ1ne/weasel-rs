// richschema 是人工维护的约束与界面描述；schema 仅包含标准 JSON Schema。
// 默认值来自同名 .json，绝不反向写入默认配置。
import { readFileSync, writeFileSync, readdirSync } from 'node:fs';
import { resolve, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = process.argv.includes('--check');
const excluded = new Set(['.git', 'node_modules', 'target', 'artifacts', 'vendor', 'build']);
function discover(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    if (entry.isSymbolicLink() || excluded.has(entry.name)) return [];
    const path = join(directory, entry.name);
    return entry.isDirectory() ? discover(path) : entry.name.endsWith('.richschema.json') ? [path] : [];
  });
}
function applyDefaults(schema, value) {
  // schema 的 default 只是注解，不负责运行时填充。仅为实际存在的默认字段生成。
  delete schema.default;
  if (value !== undefined) schema.default = structuredClone(value);
  for (const [key, child] of Object.entries(schema.properties ?? {})) {
    applyDefaults(child, value && typeof value === 'object' ? value[key] : undefined);
  }
}
for (const path of discover(root)) {
  const rich = JSON.parse(readFileSync(path, 'utf8'));
  if (rich.formatVersion !== 1 || !rich.schema || rich.schema.type !== 'object') {
    throw new Error(`Invalid richschema: ${path}`);
  }
  // UI 路径只允许指向静态 properties，避免拼错后静默丢失控件提示。
  for (const pointer of Object.keys(rich.ui?.fields ?? {})) {
    let node = rich.schema;
    if (!pointer.startsWith('/')) throw new Error(`Invalid UI pointer: ${pointer}`);
    for (const token of pointer.slice(1).split('/')) {
      node = node?.properties?.[token.replace(/~1/g, '/').replace(/~0/g, '~')];
    }
    if (!node) throw new Error(`Unknown UI field ${pointer}: ${path}`);
  }
  const schema = structuredClone(rich.schema);
  const defaults = JSON.parse(readFileSync(path.replace('.richschema.json', '.json'), 'utf8'));
  applyDefaults(schema, defaults);
  schema.$comment = 'Generated from the matching .richschema.json and .json; do not edit.';
  const output = path.replace('.richschema.json', '.schema.json');
  const text = JSON.stringify(schema, null, 2) + '\n';
  if (check) {
    if (readFileSync(output, 'utf8').replace(/\r\n/g, '\n') !== text) {
      throw new Error(`Generated schema is stale: ${output}`);
    }
  } else writeFileSync(output, text);
  console.log(`${check ? 'Checked' : 'Generated'} ${output}`);
}
