//! Only successfully handshaken children enter the broker's supervision set.
use crate::bindings::{HANDLE, WaitForSingleObject};
use std::{
    io,
    os::windows::io::{AsRawHandle, RawHandle},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::OnceLock,
    time::Duration,
};
use weasel_common::{
    message::PeerRole,
    process::{SingleInstance, SingleInstanceError},
    rpc::{RpcClient, default_pipe_name, default_renderer_pipe_name},
    service_owner::{self, StopSignal},
};

struct Owner {
    token: String,
    birth: u64,
}
static OWNER: OnceLock<Owner> = OnceLock::new();
pub fn initialize() -> io::Result<()> {
    let process = service_owner::open_process(std::process::id())?;
    let _ = OWNER.set(Owner {
        token: service_owner::new_token()?,
        birth: service_owner::birth(&process)?,
    });
    Ok(())
}
fn component(executable: &str) -> io::Result<&'static str> {
    match executable {
        "weasel-server.exe" => Ok("server"),
        "weasel-renderer.exe" => Ok("renderer"),
        _ => Err(io::Error::other("not a managed service")),
    }
}
fn pipe(component: &str) -> String {
    if component == "server" {
        default_pipe_name()
    } else {
        default_renderer_pipe_name()
    }
}
fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

pub struct Child {
    process: std::process::Child,
    stop: StopSignal,
    component: &'static str,
    instance: String,
}
impl Child {
    pub fn id(&self) -> u32 {
        self.process.id()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.process.try_wait()
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.process.wait()
    }
    pub async fn verify(&self, client: &RpcClient) -> Result<(), String> {
        let identity = client.identify_service().await.map_err(|e| e.to_string())?;
        let owner = OWNER.get().ok_or("broker ownership not initialized")?;
        if !matches_identity(
            &identity,
            client.server_pid(),
            self.id(),
            self.component,
            &owner.token,
        ) || (!self.instance.is_empty() && self.instance != identity.instance)
        {
            return Err("pipe belongs to a different service instance".into());
        }
        Ok(())
    }
}
impl AsRawHandle for Child {
    fn as_raw_handle(&self) -> RawHandle {
        self.process.as_raw_handle()
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.stop.signal();
        // No TerminateProcess: let librime finalize on the ordinary shutdown path.
    }
}

fn matches_identity(
    identity: &weasel_common::message::ServiceIdentity,
    actual: u32,
    expected: u32,
    component: &str,
    token: &str,
) -> bool {
    actual != 0
        && actual == expected
        && identity.pid == actual
        && identity.component == component
        && identity.owner_token == token
        && !identity.instance.is_empty()
}

pub fn start(directory: &Path, executable: &str, arguments: &[&str]) -> io::Result<Child> {
    let component = component(executable)?;
    let owner = OWNER
        .get()
        .ok_or_else(|| io::Error::other("broker ownership not initialized"))?;
    // A live foreign instance is a conflict, not a child we can adopt.
    drop(SingleInstance::acquire(component).map_err(io::Error::other)?);
    let stop = StopSignal::new()?;
    let mut command = Command::new(directory.join(executable));
    command
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .env(service_owner::TOKEN_ENV, &owner.token)
        .env(service_owner::PID_ENV, std::process::id().to_string())
        .env(service_owner::BIRTH_ENV, owner.birth.to_string())
        .env(service_owner::STOP_ENV, &stop.name);
    if !weasel_common::runtime_paths::is_development_directory(directory) {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = Child {
        process: command.spawn()?,
        stop,
        component,
        instance: String::new(),
    };
    let result = runtime()?.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = child.try_wait()? {
                    return Err(io::Error::other(format!(
                        "{component} exited before readiness: {status}"
                    )));
                }
                if let Ok(client) = RpcClient::connect_as(pipe(component), PeerRole::Broker).await {
                    if client.server_pid() != child.id() {
                        return Err(io::Error::other(
                            "service pipe was occupied by a different PID",
                        ));
                    }
                    let identity = client.identify_service().await.map_err(io::Error::other)?;
                    if !matches_identity(
                        &identity,
                        client.server_pid(),
                        child.id(),
                        component,
                        &owner.token,
                    ) {
                        return Err(io::Error::other("service ownership handshake failed"));
                    }
                    if identity.ready {
                        child.instance = identity.instance;
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| io::Error::other(format!("{component} readiness timed out")))?
    });
    if let Err(error) = result {
        child.stop.signal();
        unsafe {
            let _ = WaitForSingleObject(HANDLE(child.as_raw_handle()), 5000);
        }
        return Err(error);
    }
    Ok(child)
}

/// Startup only. Never terminate by image name; pin the pipe owner's process
/// handle and validate its installation path before requesting graceful exit.
pub fn clear_stale(directory: &Path, executable: &str) -> io::Result<()> {
    let component = component(executable)?;
    match SingleInstance::acquire(component) {
        Ok(guard) => {
            drop(guard);
            return Ok(());
        }
        Err(SingleInstanceError::AlreadyRunning) => {}
        Err(error) => return Err(io::Error::other(error)),
    }
    let expected = directory.join(executable).canonicalize()?;
    let process = runtime()?.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            let client = RpcClient::connect_as_with_timeout(
                pipe(component),
                PeerRole::Broker,
                Duration::from_secs(2),
            )
            .await
            .map_err(io::Error::other)?;
            let pid = client.server_pid();
            if pid == 0 {
                return Err(io::Error::other("cannot identify stale pipe owner"));
            }
            let process = service_owner::open_process(pid)?;
            let path = service_owner::process_path(&process)?.canonicalize()?;
            if !path
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy())
            {
                return Err(io::Error::other(format!(
                    "{component} belongs to another installation: {}",
                    path.display()
                )));
            }
            // Older services lack IdentifyService. Kernel PID + exact executable
            // path are sufficient for cleanup, but never for readiness/adoption.
            if let Ok(Ok(identity)) =
                tokio::time::timeout(Duration::from_millis(500), client.identify_service()).await
            {
                if identity.pid != pid || identity.component != component {
                    return Err(io::Error::other("stale service identity mismatch"));
                }
            }
            drop(client);
            let client = RpcClient::connect_as(pipe(component), PeerRole::Broker)
                .await
                .map_err(io::Error::other)?;
            if client.server_pid() != pid
                || unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 0) } == 0
            {
                return Err(io::Error::other("stale service changed during cleanup"));
            }
            let response = client
                .shutdown("broker startup is replacing an unmanaged service")
                .await
                .map_err(io::Error::other)?;
            if !response.accepted {
                return Err(io::Error::other(response.message));
            }
            Ok(process)
        })
        .await
        .map_err(|_| io::Error::other("stale service shutdown timed out"))?
    })?;
    if unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 5000) } != 0 {
        return Err(io::Error::other(format!(
            "{component} did not exit; startup cancelled"
        )));
    }
    drop(SingleInstance::acquire(component).map_err(io::Error::other)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_requires_kernel_pid_owner_and_component() {
        let mut id = weasel_common::message::ServiceIdentity {
            pid: 7,
            component: "server".into(),
            owner_token: "owner".into(),
            instance: "instance".into(),
            ready: true,
        };
        assert!(matches_identity(&id, 7, 7, "server", "owner"));
        assert!(!matches_identity(&id, 8, 7, "server", "owner"));
        assert!(!matches_identity(&id, 7, 7, "renderer", "owner"));
        assert!(!matches_identity(&id, 7, 7, "server", "different"));
        id.instance.clear();
        assert!(!matches_identity(&id, 7, 7, "server", "owner"));
    }
}
