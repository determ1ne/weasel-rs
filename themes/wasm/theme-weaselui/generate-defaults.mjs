// 构建时生成字段校验所用的类型化回退常量；完整默认JSON只在元数据中打包。
import { readFileSync, writeFileSync } from 'node:fs';
const config = JSON.parse(readFileSync(new URL('./config.json', import.meta.url), 'utf8'));
if (!config || Array.isArray(config) || typeof config !== 'object') throw new Error('config must be an object');
// 同一份配置同时生成类型化常量。颜色在构建时转换为 ARGB，字号仍保留 pt。
function constants(object, prefix = '') {
  return Object.entries(object).filter(([key]) => key !== '$schema').map(([key, value]) => {
    if (!/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(key)) throw new Error(`invalid config key: ${key}`);
    const name = prefix + key.toUpperCase();
    if (value && typeof value === 'object' && !Array.isArray(value)) return constants(value, name + '_');
    let type;
    let literal;
    if (prefix === 'COLOR_') {
      if (typeof value !== 'string' || !/^#?(?:[0-9a-f]{6}|[0-9a-f]{8})$/i.test(value)) throw new Error(`invalid default color: ${key}`);
      const hex = value.replace(/^#/, '');
      type = 'u32'; literal = '0x' + (hex.length === 6 ? 'ff' : '') + hex;
    } else if (typeof value === 'boolean') {
      type = 'bool'; literal = String(value);
    } else if (typeof value === 'number' && Number.isFinite(value)) {
      type = 'f32'; literal = String(value);
    } else if (typeof value === 'string') {
      type = 'string'; literal = JSON.stringify(value);
    } else throw new Error(`unsupported default: ${name}`);
    return `export const ${name}: ${type} = ${literal};\n`;
  }).join('');
}
writeFileSync(new URL('./assembly/defaults.ts', import.meta.url),
  `// Generated from config.json — do not edit.\n` +
  `// 由 config.json 生成：宿主读取 JSON，主题使用类型化默认值；请勿手工修改。\n` +
  constants(config));
