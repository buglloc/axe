use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use russh::server;
use tokio::task::JoinSet;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
const MAX_AGENT_CONNECTIONS: usize = 16;

pub(super) struct AgentForward {
    socket: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

struct AgentDirectory(PathBuf);

impl Drop for AgentDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join("agent"));
        let _ = std::fs::remove_dir(&self.0);
    }
}

impl AgentForward {
    pub(super) fn start(handle: server::Handle) -> io::Result<Self> {
        let directory = loop {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("axe-agent-{}-{sequence}", std::process::id()));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => break AgentDirectory(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let socket = directory.0.join("agent");
        let listener = tokio::net::UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let task = tokio::spawn(async move {
            let _directory = directory;
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    Some(result) = connections.join_next(), if !connections.is_empty() => {
                        if let Err(error) = result {
                            eprintln!("sshd: agent forwarding task failed: {error}");
                        }
                    }
                    accepted = listener.accept(), if connections.len() < MAX_AGENT_CONNECTIONS => {
                        let (stream, _) = match accepted {
                            Ok(accepted) => accepted,
                            Err(error) => {
                                eprintln!("sshd: accept agent connection: {error}");
                                break;
                            }
                        };
                        let handle = handle.clone();
                        connections.spawn(async move {
                            match handle.channel_open_agent().await {
                                Ok(channel) => super::forward_stream(channel.into_stream(), stream, "auth-agent").await,
                                Err(error) => eprintln!("sshd: open agent channel: {error}"),
                            }
                        });
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self { socket, task })
    }

    pub(super) fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for AgentForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}
