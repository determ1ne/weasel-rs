pub use crate::bindings::SECURITY_ATTRIBUTES as SecurityAttributes;
use crate::bindings::*;
use std::{ffi::c_void, io};
use windows_strings::HSTRING;

struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
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

/// Process token identity, independent of thread impersonation. A logon SID
/// distinguishes separate logons of the same account, including terminal sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIdentity {
    user_sid: String,
    logon_sid: String,
}

impl RuntimeIdentity {
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
    pub fn user_sid(&self) -> &str {
        &self.user_sid
    }
    pub fn logon_sid(&self) -> &str {
        &self.logon_sid
    }
    fn suffix(&self, component: &str) -> io::Result<String> {
        super::validate_component(component)?;
        Ok(format!(
            "weasel-rs-{component}-{}-{}",
            self.user_sid, self.logon_sid
        ))
    }
    pub fn mutex_name(&self, component: &str) -> io::Result<String> {
        Ok(format!(r"Local\{}", self.suffix(component)?))
    }
    pub fn pipe_name(&self, component: &str) -> io::Result<String> {
        Ok(format!(r"\\.\pipe\{}", self.suffix(component)?))
    }
}

/// Owned LocalAlloc descriptor. Keep alive through the object creation call.
/// Internal objects remain logon-private; the input pipe explicitly supports
/// AppContainer and low-integrity hosts using Mozc's sharable-pipe policy.
pub struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl LocalSecurityDescriptor {
    pub fn for_named_pipe(identity: &RuntimeIdentity) -> io::Result<Self> {
        Self::for_logon(identity)
    }
    pub fn for_input_pipe(identity: &RuntimeIdentity) -> io::Result<Self> {
        // Mozc kSharablePipe: suppress implicit owner rights, grant System,
        // administrators, app packages and the user; label low integrity.
        // Keep network denial. No Everyone or Restricted Code grant.
        // This is deliberately not a logon-only ACL: the logon suffix in the
        // name is routing, not authentication, and peer roles are self-reported.
        Self::from_sddl(format!(
            "O:{}D:P(D;;GA;;;NU)(A;;;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AC)(A;;GA;;;{})S:(ML;;NX;;;LW)",
            identity.user_sid, identity.user_sid
        ))
    }
    pub(crate) fn for_logon(identity: &RuntimeIdentity) -> io::Result<Self> {
        Self::from_sddl(format!(
            "O:{}D:P(D;;GA;;;NU)(A;;GA;;;{})",
            identity.user_sid, identity.logon_sid
        ))
    }
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
    /// Pass to the pipe API's raw security-attributes argument. Caller must also
    /// set PIPE_REJECT_REMOTE_CLIENTS and first-instance protection on creation.
    pub fn security_attributes(&self) -> SecurityAttributes {
        SecurityAttributes {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
    pub fn as_ptr(&self) -> *mut c_void {
        self.0.0
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
        assert!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    PSECURITY_DESCRIPTOR(descriptor.as_ptr()),
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
