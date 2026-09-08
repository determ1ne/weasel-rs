//! Opt-in, metadata-only tracing to the debugger. No file/pipe queues are added
//! to the input path, and no input strings are formatted by the call sites.
use std::sync::atomic::{AtomicBool, Ordering};
static ENABLED: AtomicBool = AtomicBool::new(false);
pub fn enable(development: bool) {
    ENABLED.store(development, Ordering::Relaxed);
}
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}
pub fn record(args: std::fmt::Arguments<'_>) {
    if !enabled() {
        return;
    }
    let line = windows_strings::HSTRING::from(format!(
        "{} DEBUG weasel-input: pid={} thread={:?} {}\n",
        crate::logging::timestamp(std::time::SystemTime::now()),
        std::process::id(),
        std::thread::current().id(),
        args
    ));
    unsafe {
        crate::bindings::OutputDebugStringW(windows_strings::PCWSTR(line.as_ptr()));
    }
}
#[macro_export]
macro_rules! input_trace {
    ($($args:tt)*) => {
        if $crate::input_diagnostics::enabled() {
            $crate::input_diagnostics::record(format_args!($($args)*));
        }
    };
}
