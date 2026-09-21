import { ResourceMetric } from "./types";
import * as raw from "./raw";

/**
 * AS没有确定性析构；资源不再使用时必须dispose()。
 * 不要在每帧重新加载图片。创建失败的handle是负错误码，不可继续绘制。
 * dispose只释放guest句柄，已提交帧由host持有，卸载模块时host兜底释放全部。
 */
export class Resource {
  constructor(public handle:i32) {}
  get valid():bool {return this.handle>0;}
  dispose():void {
    if (this.handle>0) raw.resource_release(this.handle);
    this.handle=0;
  }
}
export class Font extends Resource {
  static create(family:string,size:f32,weight:i32=400):Font {
    const b=String.UTF8.encode(family);
    return new Font(raw.font_create(changetype<i32>(b),b.byteLength,size,weight));
  }
}
export class TextLayout extends Resource {
  get width():f32 {return raw.resource_metric(this.handle,ResourceMetric.Width);}
  get height():f32 {return raw.resource_metric(this.handle,ResourceMetric.Height);}
  static create(font:Font,text:string,width:f32=65536,height:f32=4096,wrap:bool=false):TextLayout {
    const b=String.UTF8.encode(text);
    return new TextLayout(raw.text_layout_create(font.handle,changetype<i32>(b),b.byteLength,width,height,wrap?1:0));
  }
  get baseline():f32 {return raw.resource_metric(this.handle,ResourceMetric.Baseline);}
  draw(x:f32,y:f32,color:u32,glow:f32=0,glowColor:u32=0):void {
    raw.draw_layout(this.handle,x,y,color,glow,glowColor);
  }
}
export class Image extends Resource {
  get width():f32 {return raw.resource_metric(this.handle,ResourceMetric.Width);}
  get height():f32 {return raw.resource_metric(this.handle,ResourceMetric.Height);}
  static load(name:string):Image {
    const b=String.UTF8.encode(name);
    return new Image(raw.image_load(changetype<i32>(b),b.byteLength));
  }
  static fromPNG(bytes:ArrayBuffer):Image {
    return new Image(raw.image_create(changetype<i32>(bytes),bytes.byteLength));
  }
  draw(x:f32,y:f32,width:f32,height:f32,opacity:f32=1):void {
    raw.draw_image(this.handle,x,y,width,height,opacity);
  }
}
