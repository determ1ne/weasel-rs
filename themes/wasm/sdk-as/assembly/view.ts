import { ViewField, ViewStringField } from "./types";
// Host snapshot adapter. No JSON parser or fixed-address transfer arena.
import { view_i64 as integer, view_string as copy } from "./raw";

function text(field:ViewStringField,index:i32 = 0): string {
  const n = copy(field,index,0,0);
  if (n < 0) return "";
  assert(n <= 1024*1024);
  const bytes = new ArrayBuffer(n);
  assert(copy(field,index,changetype<i32>(bytes),n) == n);
  return String.UTF8.decode(bytes);
}

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
  active: bool = false;
  hasAsciiMode: bool = false;
  asciiMode: bool = true;
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
  if (integer(ViewField.HasSnapshot,0) == 0) return null;
  const v = new View();
  v.contentId = integer(ViewField.ContentId,0);
  v.active = integer(ViewField.Active,0) != 0;
  v.visible = integer(ViewField.Visible,0) != 0;
  const ascii = integer(ViewField.AsciiMode,0);
  v.hasAsciiMode = ascii >= 0;
  v.asciiMode = ascii != 0;
  v.selectedIndex = <i32>integer(ViewField.SelectedIndex,0);
  v.pageStart = <i32>integer(ViewField.PageStart,0);
  v.totalItemCount = <i32>integer(ViewField.TotalItemCount,0);
  v.canPagePrevious = integer(ViewField.CanPagePrevious,0) != 0;
  v.canPageNext = integer(ViewField.CanPageNext,0) != 0;
  v.hasPreedit = integer(ViewField.HasPreedit,0) != 0;
  v.preeditCursor = <i32>integer(ViewField.CursorUtf16,0);
  if (v.hasPreedit) v.preedit = text(ViewStringField.Preedit);
  v.anchorValid = integer(ViewField.AnchorValid,0) != 0;
  v.anchorLeft = <i32>integer(ViewField.AnchorLeft,0);
  v.anchorTop = <i32>integer(ViewField.AnchorTop,0);
  v.anchorRight = <i32>integer(ViewField.AnchorRight,0);
  v.anchorBottom = <i32>integer(ViewField.AnchorBottom,0);
  const count = <i32>integer(ViewField.ItemCount,0);
  assert(count >= 0 && count <= 4096);
  for (let i=0;i<count;i++) {
    const item = new ViewItem();
    item.primary = text(ViewStringField.Primary,i);
    item.secondary = text(ViewStringField.Secondary,i);
    item.enabled = integer(ViewField.ItemEnabled,i) != 0;
    v.items.push(item);
  }
  return v;
}
