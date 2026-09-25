//! Windows 运行身份与本地对象安全设施。

/// Win32 `SECURITY_ATTRIBUTES` 的公共类型别名。
pub use crate::bindings::SECURITY_ATTRIBUTES as SecurityAttributes;
use crate::bindings::*;
use std::io;
use windows_strings::HSTRING;

/// 验证将要嵌入 Windows 内核对象名称的组件名。
///
/// 合法名称由 1 至 64 个 ASCII 字母、数字、连字符或下划线组成。
pub(crate) fn validate_component(component: &str) -> io::Result<()> {
    if component.is_empty()
        || component.len() > 64
        || !component
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "component must be 1..64 ASCII letters, digits, hyphens or underscores",
        ));
    }
    Ok(())
}

/// 自动关闭的进程访问令牌句柄。
struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
/// 读取一块按指针宽度对齐的访问令牌信息。
fn token_information(token: &Token, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<usize>> {
    let mut size = 0;
    unsafe {
        let _ = GetTokenInformation(token.0, class, None, 0, &mut size);
    }
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut data = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    if !unsafe {
        GetTokenInformation(
            token.0,
            class,
            Some(data.as_mut_ptr().cast()),
            size,
            &mut size,
        )
    }
    .as_bool()
    {
        return Err(io::Error::last_os_error());
    }
    Ok(data)
}

/// 将 Windows SID 转换为规范的字符串表示。
fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = windows_core::PWSTR::null();
    if !unsafe { ConvertSidToStringSidW(sid, &mut text) }.as_bool() {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0;
    unsafe {
        while *text.0.add(len) != 0 {
            len += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text.0, len));
        LocalFree(HANDLE(text.0.cast()));
        Ok(value)
    }
}

/// 当前进程所属的 Windows 用户与登录会话身份。
///
/// 身份取自进程访问令牌，不受线程模拟影响。同一用户的不同登录会话具有不同
/// 的登录 SID，因此远程桌面和快速用户切换产生的会话不会共享本地 IPC 对象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIdentity {
    user_sid: String,
    logon_sid: String,
}

impl RuntimeIdentity {
    /// 从当前进程访问令牌读取用户 SID 和唯一登录 SID。
    pub fn current() -> io::Result<Self> {
        let mut handle = HANDLE::default();
        if !unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY as u32, &mut handle) }
            .as_bool()
        {
            return Err(io::Error::last_os_error());
        }
        let token = Token(handle);
        let user = token_information(&token, TokenUser)?;
        if std::mem::size_of_val(user.as_slice()) < std::mem::size_of::<TOKEN_USER>() {
            return Err(io::Error::other("truncated TokenUser"));
        }
        let user_sid = sid_string(unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid })?;
        let groups = token_information(&token, TokenLogonSid)?;
        if std::mem::size_of_val(groups.as_slice()) < std::mem::size_of::<TOKEN_GROUPS>() {
            return Err(io::Error::other("process token has no logon SID"));
        }
        let groups = unsafe { &*groups.as_ptr().cast::<TOKEN_GROUPS>() };
        if groups.GroupCount != 1
            || groups.Groups[0].Attributes & SE_GROUP_LOGON_ID != SE_GROUP_LOGON_ID
        {
            return Err(io::Error::other("process token has no unique logon SID"));
        }
        let logon_sid = sid_string(groups.Groups[0].Sid)?;
        Ok(Self {
            user_sid,
            logon_sid,
        })
    }
    /// 返回拥有当前进程的 Windows 用户 SID。
    pub fn user_sid(&self) -> &str {
        &self.user_sid
    }

    /// 返回当前登录会话的 SID。
    pub fn logon_sid(&self) -> &str {
        &self.logon_sid
    }

    /// 生成包含用户和登录会话身份的对象名后缀。
    fn suffix(&self, component: &str) -> io::Result<String> {
        validate_component(component)?;
        Ok(format!(
            "weasel-rs-{component}-{}-{}",
            self.user_sid, self.logon_sid
        ))
    }
    /// 生成当前登录会话专用的 `Local\` 互斥体名称。
    pub fn mutex_name(&self, component: &str) -> io::Result<String> {
        Ok(format!(r"Local\{}", self.suffix(component)?))
    }

    /// 生成当前登录会话专用的命名管道路径。
    pub fn pipe_name(&self, component: &str) -> io::Result<String> {
        Ok(format!(r"\\.\pipe\{}", self.suffix(component)?))
    }
}

/// 由 `LocalAlloc` 分配并自动释放的 Windows 安全描述符。
///
/// 创建内核对象时必须保持该值存活，直到使用其安全属性的 Win32 调用返回。
/// 普通内部对象仅允许当前登录会话访问；输入管道则显式允许 AppContainer 和
/// 低完整性宿主，以支持受限应用中的 TIP。
pub struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl LocalSecurityDescriptor {
    /// 为当前登录会话内的普通命名管道创建安全描述符。
    pub fn for_named_pipe(identity: &RuntimeIdentity) -> io::Result<Self> {
        Self::for_logon(identity)
    }

    /// 为 TIP 输入管道创建可供受限宿主访问的安全描述符。
    pub fn for_input_pipe(identity: &RuntimeIdentity) -> io::Result<Self> {
        // 参考 Mozc 的 kSharablePipe 实现
        // - 抑制隐式所有者权限
        // - 授权 System、管理员、App Packages 和当前用户
        // - 设置低完整性标签
        // - 拒绝网络登录
        // - 不向 Everyone 或 Restricted Code 授权
        Self::from_sddl(format!(
            "O:{}D:P(D;;GA;;;NU)(A;;;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)(A;;GA;;;{})S:(ML;;NX;;;LW)",
            identity.user_sid, identity.user_sid
        ))
    }
    /// 为仅限当前登录会话访问的本地对象创建安全描述符。
    pub(crate) fn for_logon(identity: &RuntimeIdentity) -> io::Result<Self> {
        Self::from_sddl(format!(
            "O:{}D:P(D;;GA;;;NU)(A;;GA;;;{})",
            identity.user_sid, identity.logon_sid
        ))
    }
    /// 将 SDDL 字符串解析为拥有所有权的安全描述符。
    fn from_sddl(sddl: String) -> io::Result<Self> {
        let sddl = HSTRING::from(sddl);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        if !unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                windows_core::PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1 as u32,
                &mut descriptor,
                None,
            )
        }
        .as_bool()
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
    /// 构造可传给 Win32 对象创建函数的安全属性。
    ///
    /// 返回值借用本对象持有的安全描述符；调用方必须让对象存活到对象创建
    /// 调用返回。创建命名管道时仍须设置 `PIPE_REJECT_REMOTE_CLIENTS`，并启用
    /// first-instance 保护。
    pub fn security_attributes(&self) -> SecurityAttributes {
        SecurityAttributes {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
}
impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(HANDLE(self.0.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_security() {
        let a = RuntimeIdentity::current().unwrap();
        assert_eq!(a, RuntimeIdentity::current().unwrap());
        assert!(a.pipe_name("server").unwrap().contains(a.logon_sid()));
        assert!(a.mutex_name("bad\\name").is_err());
        let descriptor = LocalSecurityDescriptor::for_named_pipe(&a).unwrap();
        assert!(
            !descriptor
                .security_attributes()
                .lpSecurityDescriptor
                .is_null()
        );
        let mut text = windows_core::PWSTR::null();
        let attributes = descriptor.security_attributes();
        assert!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    PSECURITY_DESCRIPTOR(attributes.lpSecurityDescriptor),
                    SDDL_REVISION_1 as u32,
                    SECURITY_INFORMATION(DACL_SECURITY_INFORMATION as u32),
                    &mut text,
                    None,
                )
            }
            .as_bool()
        );
        let mut len = 0;
        let dacl = unsafe {
            while *text.0.add(len) != 0 {
                len += 1;
            }
            let dacl = String::from_utf16_lossy(std::slice::from_raw_parts(text.0, len));
            LocalFree(HANDLE(text.0.cast()));
            dacl
        };
        assert_eq!(dacl, format!("D:P(D;;GA;;;NU)(A;;GA;;;{})", a.logon_sid()));
        let another_logon = RuntimeIdentity {
            user_sid: a.user_sid.clone(),
            logon_sid: "S-1-5-5-123-456".into(),
        };
        assert_ne!(
            a.pipe_name("server").unwrap(),
            another_logon.pipe_name("server").unwrap()
        );
        assert_ne!(
            a.mutex_name("server").unwrap(),
            another_logon.mutex_name("server").unwrap()
        );
    }
}
