// Generated from config.json — do not edit.
// 由 config.json 生成，请勿手工修改。
export const POSITION_X: f32 = 24;
export const POSITION_Y: f32 = 24;
export const WIDTH: f32 = 452;
export const FONT_FACE: string = "Microsoft YaHei UI";
export const FONT_SIZE: f32 = 18;

const bytes = String.UTF8.encode("{\"$schema\":\"config.schema.json\",\"position_x\":24,\"position_y\":24,\"width\":452,\"font_face\":\"Microsoft YaHei UI\",\"font_size\":18}");
export function default_config(): i64 { return (<i64>bytes.byteLength << 32) | <i64>changetype<u32>(bytes); }
