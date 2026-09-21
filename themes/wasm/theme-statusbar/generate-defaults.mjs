// 构建时生成字段校验所用的类型化回退常量；完整默认JSON只在元数据中打包。
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
  `// 由 config.json 生成，请勿手工修改。\n${declarations}\n`
);
