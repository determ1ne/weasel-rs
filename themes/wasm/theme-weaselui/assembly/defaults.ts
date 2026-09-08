// Generated from config.json — do not edit.
// 由 config.json 生成：宿主读取 JSON，主题使用类型化默认值；请勿手工修改。
export const NAME: string = "碧水 / Aqua";
export const AUTHOR: string = "佛振 <chen.sst@gmail.com>";
export const HORIZONTAL: bool = false;
export const FONT_FACE: string = "Microsoft YaHei";
export const LABEL_FONT_FACE: string = "Microsoft YaHei";
export const COMMENT_FONT_FACE: string = "Microsoft YaHei";
export const FONT_POINT: f32 = 14;
export const LABEL_FONT_POINT: f32 = 14;
export const COMMENT_FONT_POINT: f32 = 14;
export const LABEL_FORMAT: string = "%s.";
export const LAYOUT_MIN_WIDTH: f32 = 160;
export const LAYOUT_MAX_WIDTH: f32 = 0;
export const LAYOUT_MIN_HEIGHT: f32 = 0;
export const LAYOUT_MAX_HEIGHT: f32 = 0;
export const LAYOUT_BORDER_WIDTH: f32 = 3;
export const LAYOUT_MARGIN_X: f32 = 12;
export const LAYOUT_MARGIN_Y: f32 = 12;
export const LAYOUT_SPACING: f32 = 10;
export const LAYOUT_CANDIDATE_SPACING: f32 = 5;
export const LAYOUT_CORNER_RADIUS: f32 = 4;
export const LAYOUT_HILITE_SPACING: f32 = 4;
export const LAYOUT_HILITE_PADDING: f32 = 2;
export const LAYOUT_HILITE_CORNER_RADIUS: f32 = 4;
export const LAYOUT_SHADOW_RADIUS: f32 = 0;
export const LAYOUT_SHADOW_OFFSET_X: f32 = 4;
export const LAYOUT_SHADOW_OFFSET_Y: f32 = 4;
export const COLOR_SHADOW: u32 = 0x40000000;
export const COLOR_TEXT: u32 = 0xff000000;
export const COLOR_BACK: u32 = 0xffeeeeec;
export const COLOR_BORDER: u32 = 0xffe0e0e0;
export const COLOR_CANDIDATE_TEXT: u32 = 0xff000000;
export const COLOR_COMMENT_TEXT: u32 = 0xff4f4f4e;
export const COLOR_LABEL: u32 = 0xff4f4f4e;
export const COLOR_HILITED_TEXT: u32 = 0xff000000;
export const COLOR_HILITED_BACK: u32 = 0xffd4d4d4;
export const COLOR_HILITED_CANDIDATE_TEXT: u32 = 0xffffffff;
export const COLOR_HILITED_CANDIDATE_BACK: u32 = 0xff0a3afa;
export const COLOR_HILITED_COMMENT_TEXT: u32 = 0xff4f4f4e;
export const COLOR_HILITED_LABEL: u32 = 0xffadbdfd;

const bytes = String.UTF8.encode("{\"$schema\":\"config.schema.json\",\"name\":\"碧水 / Aqua\",\"author\":\"佛振 <chen.sst@gmail.com>\",\"horizontal\":false,\"font_face\":\"Microsoft YaHei\",\"label_font_face\":\"Microsoft YaHei\",\"comment_font_face\":\"Microsoft YaHei\",\"font_point\":14,\"label_font_point\":14,\"comment_font_point\":14,\"label_format\":\"%s.\",\"layout\":{\"min_width\":160,\"max_width\":0,\"min_height\":0,\"max_height\":0,\"border_width\":3,\"margin_x\":12,\"margin_y\":12,\"spacing\":10,\"candidate_spacing\":5,\"corner_radius\":4,\"hilite_spacing\":4,\"hilite_padding\":2,\"hilite_corner_radius\":4,\"shadow_radius\":0,\"shadow_offset_x\":4,\"shadow_offset_y\":4},\"color\":{\"shadow\":\"#40000000\",\"text\":\"#000000\",\"back\":\"#eeeeec\",\"border\":\"#e0e0e0\",\"candidate_text\":\"#000000\",\"comment_text\":\"#4f4f4e\",\"label\":\"#4f4f4e\",\"hilited_text\":\"#000000\",\"hilited_back\":\"#d4d4d4\",\"hilited_candidate_text\":\"#ffffff\",\"hilited_candidate_back\":\"#0a3afa\",\"hilited_comment_text\":\"#4f4f4e\",\"hilited_label\":\"#adbdfd\"}}");
export function default_config(): i64 { return (<i64>bytes.byteLength << 32) | <i64>changetype<u32>(bytes); }
