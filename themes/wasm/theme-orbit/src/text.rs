//! 当前页的有界布局缓存。测量和绘制共用对象，避免大页面挤出便捷SDK缓存后重复排版。
//! 只保留当前候选，字符串改变或页面缩短时旧资源由Drop释放。
use weasel_wasm_sdk::{
    resources::{Font, TextLayout},
    view::Item,
};

pub struct Text {
    source: String,
    pub layout: TextLayout,
}
impl Text {
    fn new(font: &Font, source: &str) -> Result<Self, i32> {
        Ok(Self {
            source: source.into(),
            layout: TextLayout::new(font, source, 65536.0, 4096.0, false)?,
        })
    }
    fn update(&mut self, font: &Font, source: &str) -> Result<(), i32> {
        if self.source != source {
            *self = Self::new(font, source)?;
        }
        Ok(())
    }
}
pub struct ItemText {
    pub primary: Text,
    pub secondary: Text,
}
pub struct TextCache {
    primary: Font,
    secondary: Font,
    pub items: Vec<ItemText>,
}
impl TextCache {
    pub fn new(size: f32) -> Result<Self, i32> {
        Ok(Self {
            primary: Font::new("Microsoft YaHei UI", size, 400)?,
            secondary: Font::new("Microsoft YaHei UI", (size - 3.0).max(11.0), 400)?,
            items: Vec::new(),
        })
    }
    pub fn update(&mut self, items: &[Item]) -> Result<(), i32> {
        self.items.truncate(items.len());
        for (i, item) in items.iter().enumerate() {
            if let Some(old) = self.items.get_mut(i) {
                old.primary.update(&self.primary, &item.primary)?;
                old.secondary.update(&self.secondary, &item.secondary)?;
            } else {
                self.items.push(ItemText {
                    primary: Text::new(&self.primary, &item.primary)?,
                    secondary: Text::new(&self.secondary, &item.secondary)?,
                });
            }
        }
        Ok(())
    }
}
