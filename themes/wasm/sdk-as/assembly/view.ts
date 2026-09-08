// Host snapshot adapter. No JSON parser or fixed-address transfer arena.
import { candidate as data, OBJECT, NUMBER } from "./data";

export class ViewItem {
  primary: string = "";
  secondary: string = "";
  enabled: bool = true;
}

export class View {
  preedit: string = "";
  preeditCursor: i32 = 0;
  hasPreedit: bool = false;
  contentId: i64 = 0;
  visible: bool = true;
  anchorValid: bool = false;
  anchorLeft: i32 = 0;
  anchorTop: i32 = 0;
  anchorRight: i32 = 0;
  anchorBottom: i32 = 0;
  items: ViewItem[] = [];
  /** 页内 0 基选中行（< items.length）。 */
  selectedIndex: i32 = 0;
  /** 页首候选的全局偏移（用于显示序号）。 */
  pageStart: i32 = 0;
  /** -1 表示 total_item_count 为 null。 */
  totalItemCount: i32 = -1;
  canPagePrevious: bool = false;
  canPageNext: bool = false;
}


export function readView(): View | null {
  if (data.kind("") != OBJECT) return null;
  const v = new View();
  v.contentId = data.integer("/content_id");
  v.visible = data.boolean("/visible");
  v.hasPreedit = data.kind("/preedit") == OBJECT;
  if (v.hasPreedit) {
    v.preedit = data.string("/preedit/text");
    v.preeditCursor = <i32>data.integer("/preedit/cursor");
  }
  v.anchorValid = data.boolean("/anchor/valid");
  v.anchorLeft = <i32>data.integer("/anchor/left");
  v.anchorTop = <i32>data.integer("/anchor/top");
  v.anchorRight = <i32>data.integer("/anchor/right");
  v.anchorBottom = <i32>data.integer("/anchor/bottom");
  const count = data.length("/items");
  for (let i = 0; i < count; i++) {
    const path = "/items/" + i.toString();
    const item = new ViewItem();
    item.primary = data.string(path + "/primary_text");
    item.secondary = data.string(path + "/secondary_text");
    item.enabled = data.boolean(path + "/enabled");
    v.items.push(item);
  }
  v.selectedIndex = <i32>data.integer("/selected_index");
  v.pageStart = <i32>data.integer("/page_start");
  if (data.kind("/total_item_count") == NUMBER) {
    v.totalItemCount = <i32>data.integer("/total_item_count");
  }
  v.canPagePrevious = data.boolean("/can_page_previous");
  v.canPageNext = data.boolean("/can_page_next");
  return v;
}
