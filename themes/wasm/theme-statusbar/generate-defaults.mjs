// 构建时把默认配置编码进 WASM；解析与用户覆盖仍由宿主完成。
import { readFileSync, writeFileSync } from 'node:fs';

const config = JSON.parse(readFileSync(new URL('./config.json', import.meta.url), 'utf8'));
if (!config || Array.isArray(config) || typeof config !== 'object') {
  throw new Error('config must be an object');
}

const values = {
  POSITION_X: config.position_x,
  POSITION_Y: config.position_y,
  WIDTH: config.width,
  FONT_FACE: config.font_face,
  FONT_SIZE: config.font_size,
};
for (const [name, value] of Object.entries(values)) {
  if (typeof value === 'number' && !Number.isFinite(value)) throw new Error(`${name} must be finite`);
  if (!['number', 'string'].includes(typeof value)) throw new Error(`${name} has an unsupported type`);
}
const declarations = Object.entries(values).map(([name, value]) =>
  `export const ${name}: ${typeof value === 'string' ? 'string' : 'f32'} = ${JSON.stringify(value)};`
).join('\n');
writeFileSync(new URL('./assembly/defaults.ts', import.meta.url),
  `// Generated from config.json — do not edit.\n` +
  `// 由 config.json 生成，请勿手工修改。\n${declarations}\n\n` +
  `const bytes = String.UTF8.encode(${JSON.stringify(JSON.stringify(config))});\n` +
  `export function default_config(): i64 { return (<i64>bytes.byteLength << 32) | <i64>changetype<u32>(bytes); }\n`
);
