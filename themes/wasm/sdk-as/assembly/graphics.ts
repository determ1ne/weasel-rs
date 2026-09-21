import { fill_rounded_rect } from "./raw";
import { Font, TextLayout } from "./resources";

// 简便函数共用有界字体/布局缓存；精细控制生命周期时直接使用Font/TextLayout。
// 测量和绘制按同一个key命中同一个原生布局，不在每次悬停重绘时重新排版。
const families = new Map<i32,string>();
const fonts = new Map<string,Font>();
const layouts = new Map<string,TextLayout>();
function clearLayouts():void {
  const entries=layouts.values();
  for(let i=0;i<entries.length;i++) entries[i].dispose();
  layouts.clear();
}
function clearFonts():void {
  clearLayouts();
  const entries=fonts.values();
  for(let i=0;i<entries.length;i++) entries[i].dispose();
  fonts.clear();
}
export function set_font(slot:i32,ptr:i32,len:i32):void {
  assert(slot>=0 && slot<=3);
  families.set(slot,String.UTF8.decodeUnsafe(<usize>ptr,len));
  clearFonts();
}
function layout(text:string,font:i32,size:f32):TextLayout {
  assert(font>=0 && font<=4);
  const fk=font.toString()+":"+size.toString();
  if(!fonts.has(fk)){
    if(fonts.size>=64) clearFonts();
    const familySlot=font==4?0:font;
    const family=families.has(familySlot)?families.get(familySlot):
      (font==1?"Segoe UI":font==3?"Segoe MDL2 Assets":"Microsoft YaHei UI");
    const created=Font.create(family,size,font==4?700:400);
    assert(created.valid);
    fonts.set(fk,created);
  }
  const key=fk+":"+text;
  if(!layouts.has(key)){
    if(layouts.size>=128) clearLayouts();
    const created=TextLayout.create(fonts.get(fk),text);
    assert(created.valid);
    layouts.set(key,created);
  }
  return layouts.get(key);
}
export function measure(text:string,font:i32,size:f32):f32 {
  return layout(text,font,size).width;
}
export function line_height(font:i32,size:f32):f32 {
  return layout("M中",font,size).height;
}
export function draw(text:string,x:f32,y:f32,font:i32,size:f32,color:u32,glowRadius:f32=0,glowColor:u32=0):void {
  layout(text,font,size).draw(x,y,color,glowRadius,glowColor);
}
export function roundedRect(x:f32,y:f32,w:f32,h:f32,radius:f32,color:u32):void {
  fill_rounded_rect(x,y,w,h,radius,color);
}
