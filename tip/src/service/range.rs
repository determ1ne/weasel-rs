//! 在 TSF 编辑会话许可范围内操作文本范围和选区。
use super::*;

impl TextService {
    /// 按原样恢复宿主选区及其样式；调用者须处于有效编辑会话内。
    pub(super) fn restore_selection(
        &self,
        context: &ITfContext,
        ec: TfEditCookie,
        range: ITfRange,
        style: TF_SELECTIONSTYLE,
    ) -> Result<()> {
        let mut selection = TF_SELECTION {
            range: std::mem::ManuallyDrop::new(Some(range)),
            style,
        };
        self.edit_mutated.store(true, Ordering::Release);
        let result = unsafe { context.SetSelection(ec, 1, &selection) };
        unsafe {
            std::mem::ManuallyDrop::drop(&mut selection.range);
        }
        result.ok()
    }

    /// 将 UTF-8 字符串编码为 UTF-16 后替换指定 TSF 范围的文本。
    ///
    /// `ec` 必须是授予当前会话的编辑 cookie；空文本按 TSF 约定传空指针。
    pub(super) fn set_range_text(
        &self,
        range: &ITfRange,
        ec: TfEditCookie,
        text: &str,
    ) -> Result<()> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let text_ptr = if utf16.is_empty() {
            ptr::null()
        } else {
            utf16.as_ptr()
        };
        let len = i32::try_from(utf16.len()).map_err(|_| Error::from_hresult(boundary::E_FAIL))?;
        self.edit_mutated.store(true, Ordering::Release);
        let hr = unsafe { range.SetText(ec, 0, text_ptr, len) };
        if hr.is_ok() {
            Ok(())
        } else {
            Err(Error::from_hresult(hr))
        }
    }

    /// 将折叠到范围起点后的光标移动指定 UTF-16 单位数并设置插入选区。
    ///
    /// 若 TSF 实际移动量不足请求值则失败，防止光标落到范围之外或代理项内部。
    pub(super) fn set_selection(
        &self,
        context: &ITfContext,
        ec: TfEditCookie,
        range: &ITfRange,
        cursor: usize,
    ) -> Result<()> {
        let owned_range = unsafe { range.Clone()? };
        let range = &owned_range;
        let mut moved = 0;
        let hr = unsafe { range.Collapse(ec, TF_ANCHOR_START) };
        if hr.is_err() {
            return Err(Error::from_hresult(hr));
        }
        let cursor = i32::try_from(cursor).map_err(|_| Error::from_hresult(boundary::E_FAIL))?;
        let hr = unsafe { range.ShiftStart(ec, cursor, &mut moved, ptr::null()) };
        if hr.is_err() {
            return Err(Error::from_hresult(hr));
        }
        if moved != cursor {
            return Err(Error::from_hresult(boundary::E_FAIL));
        }
        let mut selection = TF_SELECTION {
            range: std::mem::ManuallyDrop::new(Some(range.clone())),
            style: TF_SELECTIONSTYLE {
                ase: TF_AE_NONE,
                fInterimChar: BOOL(0),
            },
        };
        self.edit_mutated.store(true, Ordering::Release);
        let hr = unsafe { context.SetSelection(ec, 1, &selection) };
        // TF_SELECTION is an ABI structure, not an owning Rust wrapper.
        unsafe { std::mem::ManuallyDrop::drop(&mut selection.range) };
        if hr.is_ok() {
            Ok(())
        } else {
            Err(Error::from_hresult(hr))
        }
    }

    /// 折叠范围至末尾；仅能在对应的 TSF 编辑会话中调用。
    pub(super) fn collapse_end(&self, range: &ITfRange, ec: TfEditCookie) -> Result<()> {
        let hr = unsafe { range.Collapse(ec, TF_ANCHOR_END) };
        if hr.is_ok() {
            Ok(())
        } else {
            Err(Error::from_hresult(hr))
        }
    }
}
