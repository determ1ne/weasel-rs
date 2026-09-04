//! Redirect before the deployment process initializes Rust or native DLL CRTs.
//! Changing tracing alone (or only SetStdHandle after CRT initialization) cannot
//! reliably silence librime. The child retains file logging and owns the locks.
use std::{
    ffi::OsString,
    os::windows::process::CommandExt,
    process::{Command, ExitCode, Stdio},
};

pub fn valid_arguments(args: &[OsString]) -> bool {
    args.len() == 2
        && args.iter().any(|arg| arg == "--deploy")
        && args.iter().any(|arg| arg == "--silent")
}

fn status(command: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(crate::bindings::CREATE_NO_WINDOW as u32)
        .status()
}

pub fn run() -> ExitCode {
    let result = std::env::current_exe().and_then(|exe| {
        // Do not forward --silent: only this parent is the redirection wrapper.
        status(Command::new(exe).arg("--deploy"))
    });
    match result {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_is_only_valid_for_plain_deployment() {
        let valid =
            |args: &[&str]| valid_arguments(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert!(valid(&["--deploy", "--silent"]));
        assert!(valid(&["--silent", "--deploy"]));
        assert!(!valid(&["--silent"]));
        assert!(!valid(&["--deploy-ui", "--silent"]));
        assert!(!valid(&["--deploy", "--silent", "--deploy-ui"]));
    }

    #[test]
    fn redirected_child_preserves_failure_status() {
        // A shell fixture only: no Rime loading, deployment or service startup.
        let shell = std::env::var_os("COMSPEC").expect("Windows command processor");
        let result = status(Command::new(shell).args([
            "/D",
            "/C",
            "echo stdout & echo stderr 1>&2 & exit /b 7",
        ]))
        .expect("run redirected fixture");
        assert_eq!(result.code(), Some(7));
    }
}
