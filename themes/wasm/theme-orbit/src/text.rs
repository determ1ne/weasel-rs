//! 当前候选页的文字资源缓存。
//!
//! 测量与绘制共用同一个布局对象；按候选位置复用未变化的文字，字符串变化时重建对应布局，
//! 页面缩短时由所有权自动释放尾部资源。主题作者可沿用此模式避免每次快照重复排版。
use weasel_wasm_sdk::{
    resources::{Font, TextLayout},
    view::Item,
};

/// 一段文字及其已测量布局；内容变化时整体替换，保持源字符串与布局一致。
pub struct Text {
    source: String,
    pub layout: TextLayout,
}
impl Text {
    /// 按给定字体创建可复用布局。
    fn new(font: &Font, source: &str) -> Result<Self, i32> {
        Ok(Self {
            source: source.into(),
            layout: TextLayout::new(font, source, 65536.0, 4096.0, false)?,
        })
    }
    /// 仅在文字变化时重建布局，原内容相同则保留缓存对象。
    fn update(&mut self, font: &Font, source: &str) -> Result<(), i32> {
        if self.source != source {
            *self = Self::new(font, source)?;
        }
        Ok(())
    }
}
/// 候选主标签和次标签各自的文字布局。
pub struct ItemText {
    pub primary: Text,
    pub secondary: Text,
}
/// 一页候选共用的字体和逐项文字缓存。
///
/// 更新顺序按候选索引对齐：先截去多余项，再更新已有布局并为新增项建档。页面切换使用新
/// 缓存承接新页，因此退场页仍可在过渡期间显示自己的文字。
pub struct TextCache {
    primary: Font,
    secondary: Font,
    pub items: Vec<ItemText>,
}
impl TextCache {
    /// 创建主、副标签字体及空候选缓存。
    pub fn new(size: f32) -> Result<Self, i32> {
        Ok(Self {
            primary: Font::new("Microsoft YaHei UI", size, 400)?,
            secondary: Font::new("Microsoft YaHei UI", (size - 3.0).max(11.0), 400)?,
            items: Vec::new(),
        })
    }
    /// 将缓存同步到最新候选切片，复用同位置且未变化的文字布局。
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
