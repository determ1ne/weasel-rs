//! Per-user installation steps. The elevated NSIS process launches these
//! commands with the interactive shell's token and environment.

use std::{path::PathBuf, process::Command};

use windows_registry::CURRENT_USER;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "小狼毫RS算法服务";

fn executable_directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(std::env::current_exe()?
        .parent()
        .ok_or("broker executable has no parent directory")?
        .to_owned())
}

fn startup_command(directory: &std::path::Path) -> String {
    format!("\"{}\"", directory.join("weasel-broker.exe").display())
}

pub(crate) fn post_install() -> Result<(), Box<dyn std::error::Error>> {
    let directory = executable_directory()?;
    let status = Command::new(directory.join("weasel-server.exe"))
        .args(["--deploy", "--silent"])
        .current_dir(&directory)
        .status()?;
    if !status.success() {
        return Err(format!("Rime deployment failed with {status}").into());
    }

    let run = CURRENT_USER.create(RUN_KEY)?;
    let expected = startup_command(&directory);
    // Do not overwrite a command the user deliberately put under our value
    // name. Fresh installs have no value; ordinary upgrades already contain
    // the exact expected command.
    if run.get_string(RUN_VALUE).is_err() {
        run.set_string(RUN_VALUE, expected)?;
    }
    Ok(())
}

pub(crate) fn post_uninstall() -> Result<(), Box<dyn std::error::Error>> {
    let directory = executable_directory()?;
    let Ok(run) = CURRENT_USER.open(RUN_KEY) else {
        return Ok(());
    };
    let expected = startup_command(&directory);
    if run.get_string(RUN_VALUE).ok().as_deref() == Some(expected.as_str()) {
        run.remove_value(RUN_VALUE)?;
    }
    Ok(())
}
