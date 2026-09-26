use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
#[cfg(unix)]
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use russh::keys::{
    Certificate, PrivateKey,
    ssh_key::{self, certificate::CertType},
};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, ChannelOpenFailure, server};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, oneshot};
#[cfg(feature = "on-demand")]
use tokio::sync::{Notify, watch};

const HOST_KEY: &[u8] = crate::embedded::SSH_HOST_KEY;
const NO_SHELL_EXECUTABLE: &str = "sshd: cannot locate AXE executable for shell sessions";
const SHELL_WELCOME: &[u8] = b"AXE shell | agent context: skill://axe | runtime: doctor --json | host evidence: vzik capabilities\r\n---\r\n\r\n";

#[derive(Parser)]
#[command(
    name = "sshd",
    version,
    about = "Serve the bundled Brush shell over SSH"
)]
struct Options {
    #[arg(short = 'p', long = "listen", default_value = "[::]:6969")]
    listen: String,
    #[arg(long, env = "AXE_WORK_DIR")]
    workdir: Option<PathBuf>,
    #[arg(long, env = "AXE_STORE_DIR")]
    store_dir: Option<PathBuf>,
    /// AXE Store policy inherited by SSH shell and exec sessions.
    #[arg(long, env = "AXE_STORE_MODE", default_value_t)]
    store_mode: crate::registry::StoreMode,
    /// Allowed SSH certificate principals, separated by commas.
    #[arg(long, env = "AXE_SSHD_PRINCIPALS", value_delimiter = ',')]
    principals: Option<Vec<String>>,
    #[arg(long)]
    daemon: bool,
    #[arg(long)]
    log_file: Option<OsString>,
    #[arg(long, value_enum, default_value_t)]
    relay_transport: super::relay::RelayTransport,
    #[arg(
        long,
        conflicts_with = "no_relay",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    relay: Option<String>,
    #[arg(long, conflicts_with = "relay")]
    no_relay: bool,
    #[arg(long, default_value = "")]
    relay_target: String,
    #[arg(long, default_value = "")]
    relay_id: String,
    #[arg(
        long,
        env = "AXE_RELAY_TOKEN",
        default_value = crate::relay_config::token(),
        hide_default_value = true
    )]
    relay_token: String,
    #[arg(long, env = "AXE_RELAY_QUIC_SERVER_CERT_FILE", value_name = "FILE")]
    relay_quic_server_cert: Option<PathBuf>,
    #[arg(long, env = "AXE_RELAY_QUIC_CLIENT_CERT_FILE", value_name = "FILE")]
    relay_quic_client_cert: Option<PathBuf>,
    #[arg(long, env = "AXE_RELAY_QUIC_CLIENT_KEY_FILE", value_name = "FILE")]
    relay_quic_client_key: Option<PathBuf>,
    #[arg(long, hide = true)]
    axe_supervisor: bool,
    #[arg(long, hide = true)]
    axe_worker: bool,
}

pub fn sshd(args: Vec<OsString>) -> i32 {
    let mut options = match Options::try_parse_from(&args) {
        Ok(options) => options,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            return code;
        }
    };
    if let Some(principals) = &options.principals
        && let Err(error) = validate_principals(principals)
    {
        eprintln!("sshd: {error}");
        return 2;
    }

    if let Err(error) = prepare(&mut options) {
        eprintln!("sshd: {error}");
        return 1;
    }

    if options.daemon && !options.axe_supervisor && !options.axe_worker {
        match crate::supervisor::daemonize("sshd", &args[1..], options.log_file.as_deref()) {
            Ok(pid) => {
                println!("sshd: started supervisor pid {pid}");
                return 0;
            }
            Err(error) => {
                eprintln!("sshd: cannot daemonize: {error}");
                return 1;
            }
        }
    }

    if options.axe_supervisor || crate::supervisor::is_supervisor(&args) {
        return crate::supervisor::supervise_applet("sshd", &args[1..]);
    }
    if options.relay.is_some() && options.relay_id.is_empty() {
        options.relay_id = default_relay_id();
    }
    let ready = match crate::supervisor::take_ready_notifier() {
        Ok(ready) => ready,
        Err(error) => {
            eprintln!("sshd: readiness channel: {error}");
            return 1;
        }
    };
    start_server(options, crate::executable::current(), ready)
}

fn start_server(
    options: Options,
    executable: Arc<crate::executable::Executable>,
    ready: Option<crate::supervisor::ReadyNotifier>,
) -> i32 {
    if let Err(error) = require_shell_executable(&executable) {
        eprintln!("{error}");
        return 1;
    }

    // Publish the applet bridge from the serving process. Proc-backed targets
    // survive replacement or unlinking; filesystem targets are revalidated by
    // the shared executable capability before publication.
    let bridge_path = executable.bridge_path();
    if let Some(workdir) = options.workdir.as_deref()
        && let Err(error) =
            publish_applet_bridge(workdir, bridge_path.as_deref(), options.store_mode)
    {
        eprintln!("sshd: applet PATH bridge unavailable: {error}");
    }

    let runtime = match build_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("sshd: create runtime: {error}");
            return 1;
        }
    };

    match runtime.block_on(run(options, executable, ready)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("sshd: {error}");
            1
        }
    }
}

fn build_runtime() -> io::Result<tokio::runtime::Runtime> {
    let workers = runtime_worker_threads();
    let mut builder = if workers == 1 {
        tokio::runtime::Builder::new_current_thread()
    } else {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.worker_threads(workers);
        builder
    };
    builder.enable_all().build()
}

fn runtime_worker_threads() -> usize {
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    #[cfg(target_os = "linux")]
    {
        cgroup_quota_workers()
            .map(|quota| quota.min(available))
            .unwrap_or(available)
    }
    #[cfg(not(target_os = "linux"))]
    {
        available
    }
}

#[cfg(target_os = "linux")]
fn cgroup_quota_workers() -> Option<usize> {
    let membership = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let root = Path::new("/sys/fs/cgroup");
    let mut current = root.join(relative.trim_start_matches('/'));
    let mut effective = None;

    loop {
        if let Ok(cpu_max) = std::fs::read_to_string(current.join("cpu.max"))
            && let Some(workers) = cpu_max_workers(&cpu_max)
        {
            effective = Some(effective.map_or(workers, |limit: usize| limit.min(workers)));
        }
        if current == root {
            break;
        }
        let parent = current.parent()?;
        if !parent.starts_with(root) {
            break;
        }
        current = parent.to_path_buf();
    }
    effective
}

#[cfg(target_os = "linux")]
fn cpu_max_workers(cpu_max: &str) -> Option<usize> {
    let mut fields = cpu_max.split_ascii_whitespace();
    let quota = fields.next()?;
    if quota == "max" {
        return None;
    }
    let quota = quota.parse::<u64>().ok()?;
    let period = fields.next()?.parse::<u64>().ok()?;
    if quota == 0 || period == 0 || fields.next().is_some() {
        return None;
    }
    Some(usize::try_from(quota.div_ceil(period)).unwrap_or(usize::MAX))
}
fn require_shell_executable(
    executable: &crate::executable::Executable,
) -> Result<(), &'static str> {
    executable
        .can_reexec()
        .then_some(())
        .ok_or(NO_SHELL_EXECUTABLE)
}

fn validate_principals(principals: &[String]) -> Result<(), String> {
    if principals.is_empty() {
        return Err("principals override must not be empty".into());
    }

    let mut unique = HashSet::with_capacity(principals.len());
    for principal in principals {
        if principal.is_empty() {
            return Err("principals override must not contain empty names".into());
        }
        if !unique.insert(principal) {
            return Err(format!(
                "principals override contains duplicate principal '{principal}'"
            ));
        }
    }

    Ok(())
}

fn prepare(options: &mut Options) -> io::Result<()> {
    let inherited_store_mode = crate::registry::StoreMode::from_environment()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    options.store_mode = inherited_store_mode.restrict(options.store_mode);

    let workdir = match options.workdir.take() {
        Some(path) => path.canonicalize()?,
        None => crate::executable::current()
            .runtime_root()
            .map(Path::to_owned)
            .map_err(|failure| {
                io::Error::other(format!("{}: {}", failure.operation, failure.message))
            })?,
    };
    let store_dir = if options.store_mode == crate::registry::StoreMode::Off {
        None
    } else {
        match options.store_dir.take() {
            Some(path) => {
                std::fs::create_dir_all(&path)?;
                Some(path.canonicalize()?)
            }
            None => prepare_derived_store(&workdir),
        }
    };
    options.workdir = Some(workdir);
    options.store_dir = store_dir;

    // SAFETY: preparation finishes before daemonization, the Tokio runtime, or
    // any other SSH worker threads start. The daemon-owned Store client and
    // every session child must resolve the same normalized roots and policy.
    unsafe {
        std::env::set_var(
            "AXE_WORK_DIR",
            options
                .workdir
                .as_deref()
                .expect("sshd workdir was prepared above"),
        );
        std::env::set_var("AXE_STORE_MODE", options.store_mode.as_str());
        match options.store_dir.as_deref() {
            Some(store_dir) => std::env::set_var("AXE_STORE_DIR", store_dir),
            None => std::env::remove_var("AXE_STORE_DIR"),
        }
    }

    // Connection and authentication state remain memory-only.
    if options.no_relay {
        options.relay = None;
    } else if options.relay.is_none() && crate::relay_config::enabled_by_default() {
        options.relay = Some(
            match options.relay_transport {
                super::relay::RelayTransport::Tcp => crate::relay_config::tcp_endpoint(),
                super::relay::RelayTransport::Quic => crate::relay_config::quic_endpoint(),
            }
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "relay is enabled by default but config/relay.json has no {} endpoint",
                        options.relay_transport
                    ),
                )
            })?
            .to_owned(),
        );
    }
    if options.relay.is_some()
        && options.relay_transport == super::relay::RelayTransport::Tcp
        && options.relay_token.len() < 32
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TCP relay requires AXE_RELAY_TOKEN or an embedded token with at least 32 bytes",
        ));
    }
    Ok(())
}

fn default_relay_id() -> String {
    let username = whoami::username().ok();
    let hostname = whoami::hostname().ok();
    // SAFETY: geteuid has no pointer arguments and returns the current process identity.
    let uid = unsafe { libc::geteuid() };
    format_relay_id(
        pid_namespace_id(),
        uid,
        username.as_deref(),
        hostname.as_deref(),
    )
}

/// Returns the inode of the calling process's PID namespace.
///
/// Every process in the same PID namespace — typically one container —
/// reports the same value across restarts, so it distinguishes containers
/// rather than individual processes. The inode is not globally unique: two
/// hosts can reuse it after a container is removed, and without a visible
/// procfs the namespace cannot be identified at all; the `-1` fallback marks
/// that case instead of pretending a process-scoped value is unique.
fn pid_namespace_id() -> i64 {
    std::fs::metadata("/proc/self/ns/pid")
        .ok()
        .and_then(|metadata| {
            use std::os::unix::fs::MetadataExt as _;
            i64::try_from(metadata.ino()).ok()
        })
        .unwrap_or(-1)
}

fn format_relay_id(
    pidns: i64,
    uid: libc::uid_t,
    username: Option<&str>,
    hostname: Option<&str>,
) -> String {
    let hostname = hostname
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    match username.filter(|value| !value.is_empty()) {
        Some(username) => format!("{pidns}@{username}@{hostname}"),
        None => format!("{pidns}@{uid}@{hostname}"),
    }
}

fn prepare_derived_store(workdir: &std::path::Path) -> Option<PathBuf> {
    let store = workdir.join(".axe-store");
    axe_paths::ensure_writable_root(&store, 0).ok()?;
    if axe_paths::path_is_noexec(&store) {
        // On-demand tools must execute from their storage root; without exec
        // permission the AXE Store client selects a usable root on its own.
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    store.canonicalize().ok().or(Some(store))
}

fn publish_applet_bridge(
    workdir: &Path,
    executable: Option<&Path>,
    store_mode: crate::registry::StoreMode,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let names = crate::registry::bridge_command_names(store_mode);
        match executable {
            Some(executable) => {
                crate::path_bridge::publish_server_bridge(workdir, &names, executable)?;
            }
            None => crate::path_bridge::clear_environment()?,
        }
    }
    #[cfg(not(unix))]
    let _ = (workdir, executable, store_mode);
    Ok(())
}

async fn run(
    options: Options,
    executable: Arc<crate::executable::Executable>,
    ready: Option<crate::supervisor::ReadyNotifier>,
) -> io::Result<()> {
    let relay_quic_config = if options.relay.is_some()
        && options.relay_transport == super::relay::RelayTransport::Quic
    {
        Some(
            super::relay::load_quic_client_config(
                options.relay_quic_server_cert.as_deref(),
                options.relay_quic_client_cert.as_deref(),
                options.relay_quic_client_key.as_deref(),
            )
            .await?,
        )
    } else {
        None
    };
    let mut shutdown = crate::supervisor::ShutdownSignals::new()?;
    let host_key = load_host_key()?;
    let principals: Arc<[String]> = options
        .principals
        .unwrap_or_else(|| crate::sshd_config::principals().to_vec())
        .into();
    let ca_fingerprints = crate::sshd_config::ca_fingerprints().to_vec();

    let config = Arc::new(server::Config {
        inactivity_timeout: Some(Duration::from_secs(3600)),
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::ZERO),
        server_id: russh::SshId::Standard("SSH-2.0-axe".into()),
        keys: vec![host_key],
        ..Default::default()
    });

    let listener = tokio::net::TcpListener::bind(&options.listen).await?;
    let local = listener.local_addr()?;

    let workdir = Arc::new(
        options
            .workdir
            .expect("sshd options were prepared before starting the server"),
    );
    #[cfg(feature = "on-demand")]
    let index_refresher =
        (options.store_mode == crate::registry::StoreMode::Auto).then(IdleIndexRefresher::start);
    let mut factory = SshServer {
        principals,
        ca_fingerprints: Arc::new(ca_fingerprints),
        workdir,
        store_dir: Arc::new(options.store_dir),
        store_mode: options.store_mode,
        #[cfg(feature = "on-demand")]
        index_activity: index_refresher.as_ref().map(IdleIndexRefresher::activity),
        inputs: Arc::new(Mutex::new(HashMap::new())),
        session_channels: Arc::new(Mutex::new(HashMap::new())),
        sftp_tasks: Arc::new(Mutex::new(HashMap::new())),
        tcp_forwards: Arc::new(Mutex::new(HashMap::new())),
        #[cfg(unix)]
        ptys: Arc::new(Mutex::new(HashMap::new())),
        #[cfg(unix)]
        unix_forwards: Arc::new(Mutex::new(HashMap::new())),
        executable,
        active_connections: Arc::new(AtomicUsize::new(0)),
        welcome_sent: false,
        connection: None,
    };

    let mut server = factory.run_on_socket(config, &listener);
    let handle = server.handle();

    ready
        .map(crate::supervisor::ReadyNotifier::notify)
        .transpose()?;

    println!("sshd: serving on {local}");
    #[cfg(feature = "on-demand")]
    if let Some(refresher) = &index_refresher {
        refresher.refresh_if_idle();
    }

    let relay_task = if let Some(relay) = options.relay.filter(|relay| !relay.is_empty()) {
        let target = if options.relay_target.is_empty() {
            loopback_address(local)
        } else {
            options.relay_target
        };
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            super::relay::run_client(
                options.relay_transport,
                &relay,
                &target,
                &options.relay_id,
                &options.relay_token,
                relay_quic_config,
                shutdown_receiver,
            )
            .await;
        });
        Some((shutdown, task))
    } else {
        None
    };

    let result = tokio::select! {
        result = &mut server => result.map_err(io::Error::other),
        result = shutdown.recv() => match result {
            Ok(()) => {
                handle.shutdown("server shutting down".into());
                server.await.map_err(io::Error::other)
            }
            Err(error) => Err(error),
        },
    };

    if let Some((shutdown, mut relay_task)) = relay_task {
        let _ = shutdown.send(());
        if tokio::time::timeout(Duration::from_secs(2), &mut relay_task)
            .await
            .is_err()
        {
            relay_task.abort();
            let _ = relay_task.await;
        }
    }

    #[cfg(feature = "on-demand")]
    if let Some(refresher) = index_refresher {
        refresher.shutdown().await;
    }

    result
}

type ProcessInput = Box<dyn AsyncWrite + Send + Unpin>;
type ProcessOutput = Box<dyn AsyncRead + Send + Unpin>;

struct ChannelProcess {
    input: Option<Arc<Mutex<ProcessInput>>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl Drop for ChannelProcess {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

#[cfg(unix)]
struct PtyRequest {
    term: String,
    size: PtySize,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct PtySize {
    columns: u32,
    rows: u32,
    pixel_width: u32,
    pixel_height: u32,
}

#[cfg(unix)]
enum PtyState {
    Requested(PtyRequest),
    Active(OwnedFd),
}

struct SpawnedShell {
    child: tokio::process::Child,
    input: ProcessInput,
    stdout: ProcessOutput,
    stderr: Option<ProcessOutput>,
    #[cfg(unix)]
    pty: Option<OwnedFd>,
}

type CancelMap<K> = Arc<Mutex<HashMap<K, oneshot::Sender<()>>>>;
#[cfg(feature = "on-demand")]
#[derive(Default)]
struct SessionActivity {
    active: AtomicUsize,
    idle: Notify,
}

#[cfg(feature = "on-demand")]
impl SessionActivity {
    fn start(self: &Arc<Self>) -> ActiveSession {
        self.active.fetch_add(1, Ordering::AcqRel);
        ActiveSession {
            activity: Arc::clone(self),
        }
    }

    fn notify_if_idle(&self) {
        if self.active.load(Ordering::Acquire) == 0 {
            self.idle.notify_one();
        }
    }
}

#[cfg(feature = "on-demand")]
struct ActiveSession {
    activity: Arc<SessionActivity>,
}

#[cfg(feature = "on-demand")]
impl Drop for ActiveSession {
    fn drop(&mut self) {
        let previous = self.activity.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "active SSH session count underflowed");
        if previous == 1 {
            self.activity.idle.notify_one();
        }
    }
}

#[cfg(feature = "on-demand")]
struct IdleIndexRefresher {
    activity: Arc<SessionActivity>,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

#[cfg(feature = "on-demand")]
impl IdleIndexRefresher {
    fn start() -> Self {
        let activity = Arc::new(SessionActivity::default());
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(refresh_index_while_idle(Arc::clone(&activity), receiver));

        Self {
            activity,
            shutdown,
            task,
        }
    }

    fn activity(&self) -> Arc<SessionActivity> {
        Arc::clone(&self.activity)
    }

    fn refresh_if_idle(&self) {
        self.activity.notify_if_idle();
    }

    async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        if let Err(error) = self.task.await {
            eprintln!("sshd: Index refresher task failed: {error}");
        }
    }
}

#[cfg(feature = "on-demand")]
async fn refresh_index_while_idle(
    activity: Arc<SessionActivity>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
            () = activity.idle.notified() => {}
        }

        if activity.active.load(Ordering::Acquire) != 0 {
            continue;
        }

        match tokio::task::spawn_blocking(crate::ondemand::revalidate_index_if_stale).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("sshd: Index refresh failed: {error}"),
            Err(error) => eprintln!("sshd: Index refresh task failed: {error}"),
        }
    }
}

struct ConnectionLifecycle {
    peer: String,
    pending_user: Option<String>,
    user: Option<String>,
    active_connections: Arc<AtomicUsize>,
    connected_at: Instant,
}

impl ConnectionLifecycle {
    fn open(peer: Option<std::net::SocketAddr>, active_connections: Arc<AtomicUsize>) -> Self {
        let peer = peer.map_or_else(|| "<unknown>".to_owned(), |peer| peer.to_string());
        // This atomic is an observability counter and does not publish other state.
        let active = active_connections.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!("sshd: connection from {peer}; active_connections={active}");
        Self {
            peer,
            pending_user: None,
            user: None,
            active_connections,
            connected_at: Instant::now(),
        }
    }

    fn remember_user(&mut self, user: &str) {
        self.pending_user = Some(user.to_owned());
    }

    fn authenticated(&mut self) {
        if self.user.is_some() {
            return;
        }
        let Some(user) = self.pending_user.take() else {
            return;
        };
        eprintln!("sshd: authenticated user={user:?} from {}", self.peer);
        self.user = Some(user);
    }
}

fn write_disconnect_log(
    writer: &mut impl io::Write,
    peer: &str,
    user: Option<&str>,
    connected_seconds: u64,
    active_connections: usize,
) -> io::Result<()> {
    match user {
        Some(user) => writeln!(
            writer,
            "sshd: disconnected {peer} user={user:?}; connected={connected_seconds}s; active_connections={active_connections}"
        ),
        None => writeln!(
            writer,
            "sshd: disconnected {peer} unauthenticated; connected={connected_seconds}s; active_connections={active_connections}"
        ),
    }
}

fn write_session_error(writer: &mut impl io::Write, error: &russh::Error) -> io::Result<()> {
    if matches!(
        error,
        russh::Error::IO(source) if source.kind() == io::ErrorKind::UnexpectedEof
    ) {
        return Ok(());
    }
    writeln!(writer, "sshd: session error: {error}")
}

impl Drop for ConnectionLifecycle {
    fn drop(&mut self) {
        let previous = self.active_connections.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0, "active SSH connection count underflowed");
        let stderr = io::stderr();
        let _ = write_disconnect_log(
            &mut stderr.lock(),
            &self.peer,
            self.user.as_deref(),
            self.connected_at.elapsed().as_secs(),
            previous.saturating_sub(1),
        );
    }
}

struct SshServer {
    principals: Arc<[String]>,
    ca_fingerprints: Arc<Vec<ssh_key::Fingerprint>>,
    workdir: Arc<PathBuf>,
    store_dir: Arc<Option<PathBuf>>,
    store_mode: crate::registry::StoreMode,
    #[cfg(feature = "on-demand")]
    index_activity: Option<Arc<SessionActivity>>,
    inputs: Arc<Mutex<HashMap<ChannelId, ChannelProcess>>>,
    session_channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    sftp_tasks: Arc<Mutex<HashMap<ChannelId, tokio::task::JoinHandle<io::Result<()>>>>>,
    tcp_forwards: CancelMap<(String, u32)>,
    #[cfg(unix)]
    ptys: Arc<Mutex<HashMap<ChannelId, PtyState>>>,
    #[cfg(unix)]
    unix_forwards: CancelMap<PathBuf>,
    executable: Arc<crate::executable::Executable>,
    active_connections: Arc<AtomicUsize>,
    welcome_sent: bool,
    connection: Option<ConnectionLifecycle>,
}

impl server::Server for SshServer {
    type Handler = Self;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Self {
        let active_connections = Arc::clone(&self.active_connections);
        let connection = Some(ConnectionLifecycle::open(
            peer,
            Arc::clone(&active_connections),
        ));
        Self {
            principals: self.principals.clone(),
            ca_fingerprints: self.ca_fingerprints.clone(),
            workdir: self.workdir.clone(),
            store_dir: self.store_dir.clone(),
            store_mode: self.store_mode,
            #[cfg(feature = "on-demand")]
            index_activity: self.index_activity.clone(),
            executable: self.executable.clone(),
            inputs: Arc::new(Mutex::new(HashMap::new())),
            session_channels: Arc::new(Mutex::new(HashMap::new())),
            sftp_tasks: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(unix)]
            ptys: Arc::new(Mutex::new(HashMap::new())),
            tcp_forwards: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(unix)]
            unix_forwards: Arc::new(Mutex::new(HashMap::new())),
            active_connections,
            welcome_sent: false,
            connection,
        }
    }

    fn handle_session_error(&mut self, error: <Self::Handler as server::Handler>::Error) {
        let stderr = io::stderr();
        let _ = write_session_error(&mut stderr.lock(), &error);
    }
}

impl server::Handler for SshServer {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _: &str,
        _: &ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn auth_openssh_certificate(
        &mut self,
        user: &str,
        certificate: &Certificate,
    ) -> Result<Auth, Self::Error> {
        let trusted = self.principals.iter().any(|principal| principal == user)
            && certificate.cert_type() == CertType::User
            && certificate.critical_options().is_empty()
            && certificate
                .valid_principals()
                .iter()
                .any(|principal| principal == user)
            && certificate.validate(self.ca_fingerprints.iter()).is_ok();

        if trusted && let Some(connection) = &mut self.connection {
            connection.remember_user(user);
        }

        Ok(if trusted {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_succeeded(&mut self, _: &mut Session) -> Result<(), Self::Error> {
        if let Some(connection) = &mut self.connection {
            connection.authenticated();
        }
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.session_channels
            .lock()
            .await
            .insert(channel.id(), channel);

        reply.accept().await;

        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(port) = u16::try_from(port_to_connect) else {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };

        match tokio::net::TcpStream::connect((host_to_connect, port)).await {
            Ok(stream) => {
                reply.accept().await;
                tokio::spawn(forward_stream(
                    channel.into_stream(),
                    stream,
                    "direct-tcpip",
                ));
            }
            Err(error) => {
                eprintln!("sshd: connect {host_to_connect}:{port}: {error}");
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
            }
        }

        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        columns: u32,
        rows: u32,
        pixel_width: u32,
        pixel_height: u32,
        _: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        #[cfg(unix)]
        {
            self.ptys.lock().await.insert(
                channel,
                PtyState::Requested(PtyRequest {
                    term: term.to_owned(),
                    size: PtySize {
                        columns,
                        rows,
                        pixel_width,
                        pixel_height,
                    },
                }),
            );
            let _ = session.channel_success(channel);
        }
        #[cfg(not(unix))]
        {
            let _ = (term, columns, rows, pixel_width, pixel_height);
            let _ = session.channel_failure(channel);
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        columns: u32,
        rows: u32,
        pixel_width: u32,
        pixel_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        #[cfg(unix)]
        {
            let size = PtySize {
                columns,
                rows,
                pixel_width,
                pixel_height,
            };
            let result = match self.ptys.lock().await.get_mut(&channel) {
                Some(PtyState::Requested(request)) => {
                    request.size = size;
                    Ok(())
                }
                Some(PtyState::Active(pty)) => resize_pty(pty, size),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "channel has no pseudo-terminal",
                )),
            };
            if result.is_ok() {
                let _ = session.channel_success(channel);
            } else {
                let _ = session.channel_failure(channel);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (channel, columns, rows, pixel_width, pixel_height);
            let _ = session.channel_failure(channel);
        }
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let ssh_channel = self.session_channels.lock().await.remove(&channel);

        if name == "sftp"
            && let Some(ssh_channel) = ssh_channel
        {
            let _ = session.channel_success(channel);
            let filesystem = super::sftp::Filesystem::new((*self.workdir).clone());
            let task = tokio::spawn(async move {
                super::sftp::run(ssh_channel.into_stream(), filesystem).await
            });
            self.sftp_tasks.lock().await.insert(channel, task);
        } else {
            let _ = session.channel_failure(channel);
        }

        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.session_channels.lock().await.remove(&channel);

        #[cfg(unix)]
        let show_welcome = !self.welcome_sent
            && matches!(
                self.ptys.lock().await.get(&channel),
                Some(PtyState::Requested(_))
            );
        #[cfg(not(unix))]
        let show_welcome = false;
        let handle = session.handle();

        match self.spawn_shell(channel, None, handle, show_welcome).await {
            Ok(()) => {
                self.welcome_sent |= show_welcome;
                let _ = session.channel_success(channel);
            }
            Err(error) => {
                eprintln!("sshd: launch shell: {error}");
                let _ = session.channel_failure(channel);
            }
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.session_channels.lock().await.remove(&channel);

        let command = match std::str::from_utf8(data) {
            Ok(command) => command,
            Err(_) => {
                let _ = session.channel_failure(channel);
                return Ok(());
            }
        };

        let handle = session.handle();

        match self
            .spawn_shell(channel, Some(command), handle, false)
            .await
        {
            Ok(()) => {
                let _ = session.channel_success(channel);
            }
            Err(error) => {
                eprintln!("sshd: launch command: {error}");
                let _ = session.channel_failure(channel);
            }
        }
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        let input = self
            .inputs
            .lock()
            .await
            .get(&channel)
            .and_then(|process| process.input.clone());

        if let Some(input) = input {
            let _ = input.lock().await.write_all(data).await;
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.session_channels.lock().await.remove(&channel);

        if let Some(process) = self.inputs.lock().await.get_mut(&channel) {
            process.input.take();
        }

        // A client that finished its SFTP session sends EOF and waits for the
        // exit status before accepting the channel close.
        if let Some(task) = self.sftp_tasks.lock().await.remove(&channel) {
            task.abort();
            let _ = task.await;
            let _ = session.exit_status_request(channel, 0);
            let _ = session.eof(channel);
        }

        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.inputs.lock().await.remove(&channel);
        #[cfg(unix)]
        self.ptys.lock().await.remove(&channel);
        if let Some(task) = self.sftp_tasks.lock().await.remove(&channel) {
            task.abort();
            let _ = task.await;
        }

        self.session_channels.lock().await.remove(&channel);
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        match self
            .start_tcp_forward(address, *port, session.handle())
            .await
        {
            Ok(assigned_port) => {
                *port = assigned_port;
                Ok(true)
            }
            Err(error) => {
                eprintln!("sshd: cannot forward {address}:{port}: {error}");
                Ok(false)
            }
        }
    }

    async fn cancel_tcpip_forward(
        &mut self,
        address: &str,
        port: u32,
        _: &mut Session,
    ) -> Result<bool, Self::Error> {
        let cancel = self
            .tcp_forwards
            .lock()
            .await
            .remove(&(address.to_owned(), port));

        if let Some(cancel) = cancel {
            let _ = cancel.send(());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn channel_open_direct_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        #[cfg(unix)]
        match tokio::net::UnixStream::connect(socket_path).await {
            Ok(stream) => {
                reply.accept().await;
                tokio::spawn(forward_stream(
                    channel.into_stream(),
                    stream,
                    "direct-streamlocal",
                ));
            }
            Err(error) => {
                eprintln!("sshd: connect Unix socket {socket_path}: {error}");
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (channel, socket_path);
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
        }
        Ok(())
    }

    async fn streamlocal_forward(
        &mut self,
        socket_path: &str,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        #[cfg(unix)]
        {
            if let Err(error) = self
                .start_unix_forward(PathBuf::from(socket_path), session.handle())
                .await
            {
                eprintln!("sshd: cannot forward Unix socket {socket_path}: {error}");
                return Ok(false);
            }
            Ok(true)
        }
        #[cfg(not(unix))]
        {
            let _ = (socket_path, session);
            Ok(false)
        }
    }

    async fn cancel_streamlocal_forward(
        &mut self,
        socket_path: &str,
        _: &mut Session,
    ) -> Result<bool, Self::Error> {
        #[cfg(unix)]
        {
            let cancel = self
                .unix_forwards
                .lock()
                .await
                .remove(PathBuf::from(socket_path).as_path());

            if let Some(cancel) = cancel {
                let _ = cancel.send(());
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl SshServer {
    async fn spawn_shell(
        &self,
        channel: ChannelId,
        command: Option<&str>,
        handle: server::Handle,
        show_welcome: bool,
    ) -> io::Result<()> {
        let mut process = self.executable.tokio_command()?;

        // The daemon owns Index freshness; the session remains online for
        // Store tool delivery but builds its registry from the verified cache.
        process
            .arg("--norc")
            .arg("--noprofile")
            .arg("--no-config")
            .current_dir(&*self.workdir)
            .env("AXE", "true")
            .env("AXE_WORK_DIR", &*self.workdir)
            .env("AXE_STORE_MODE", self.store_mode.as_str())
            .env(crate::registry::INDEX_CACHE_ONLY_ENV, "1")
            .kill_on_drop(true);

        if let Some(store_dir) = self.store_dir.as_ref() {
            process.env("AXE_STORE_DIR", store_dir);
        }

        if let Some(command) = command {
            process.arg("-c").arg(command);
        }

        #[cfg(unix)]
        let requested_pty = match self.ptys.lock().await.remove(&channel) {
            Some(PtyState::Requested(request)) => Some(request),
            _ => None,
        };
        #[cfg(unix)]
        let mut spawned = match requested_pty {
            Some(request) => spawn_pty_shell(process, request)?,
            None => spawn_pipe_shell(process)?,
        };
        #[cfg(not(unix))]
        let mut spawned = spawn_pipe_shell(process)?;
        #[cfg(feature = "on-demand")]
        let active_session = self
            .index_activity
            .as_ref()
            .map(|activity| activity.start());

        #[cfg(unix)]
        if let Some(pty) = spawned.pty.take() {
            self.ptys
                .lock()
                .await
                .insert(channel, PtyState::Active(pty));
        }

        let (cancel, cancelled) = oneshot::channel();
        self.inputs.lock().await.insert(
            channel,
            ChannelProcess {
                input: Some(Arc::new(Mutex::new(spawned.input))),
                cancel: Some(cancel),
            },
        );

        let stdout_handle = handle.clone();
        let stdout_task = tokio::spawn(pump(
            spawned.stdout,
            stdout_handle,
            channel,
            None,
            show_welcome.then_some(SHELL_WELCOME),
        ));
        let stderr_task = spawned.stderr.map(|stderr| {
            let stderr_handle = handle.clone();
            tokio::spawn(pump(stderr, stderr_handle, channel, Some(1), None))
        });

        let inputs = Arc::downgrade(&self.inputs);
        #[cfg(unix)]
        let ptys = Arc::downgrade(&self.ptys);
        tokio::spawn(async move {
            #[cfg(feature = "on-demand")]
            let _active_session = active_session;
            let mut child = spawned.child;
            let status = tokio::select! {
                status = child.wait() => Some(status),
                _ = cancelled => None,
            };
            let status = match status {
                Some(status) => status,
                None => {
                    if let Err(error) = child.start_kill() {
                        eprintln!("sshd: child termination failed: {error}");
                    }
                    child.wait().await
                }
            };

            if let Some(inputs) = inputs.upgrade() {
                inputs.lock().await.remove(&channel);
            }
            #[cfg(unix)]
            if let Some(ptys) = ptys.upgrade() {
                ptys.lock().await.remove(&channel);
            }

            if let Err(error) = stdout_task.await {
                eprintln!("sshd: stdout forwarding task failed: {error}");
            }
            if let Some(stderr_task) = stderr_task
                && let Err(error) = stderr_task.await
            {
                eprintln!("sshd: stderr forwarding task failed: {error}");
            }

            let code = match status {
                Ok(status) => status.code().unwrap_or(255) as u32,
                Err(error) => {
                    eprintln!("sshd: child wait failed: {error}");
                    255
                }
            };

            let _ = handle.exit_status_request(channel, code).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }

    async fn start_tcp_forward(
        &self,
        address: &str,
        port: u32,
        handle: server::Handle,
    ) -> io::Result<u32> {
        let port = u16::try_from(port)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TCP port exceeds 65535"))?;

        let bind_address = match address {
            "" | "*" => "::",
            address => address,
        };

        let listener = tokio::net::TcpListener::bind((bind_address, port)).await?;
        let assigned_port = u32::from(listener.local_addr()?.port());

        let key = (address.to_owned(), assigned_port);
        let (cancel, cancelled) = oneshot::channel();

        let mut forwards = self.tcp_forwards.lock().await;
        if forwards.contains_key(&key) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "TCP forward is already active",
            ));
        }

        forwards.insert(key, cancel);
        drop(forwards);

        tokio::spawn(serve_tcp_forward(listener, handle, cancelled));
        Ok(assigned_port)
    }

    #[cfg(unix)]
    async fn start_unix_forward(
        &self,
        socket_path: PathBuf,
        handle: server::Handle,
    ) -> io::Result<()> {
        let listener = tokio::net::UnixListener::bind(&socket_path)?;
        let cleanup = UnixSocketPath(socket_path.clone());

        let (cancel, cancelled) = oneshot::channel();

        let mut forwards = self.unix_forwards.lock().await;
        if forwards.contains_key(&socket_path) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "Unix forward is already active",
            ));
        }

        forwards.insert(socket_path.clone(), cancel);
        drop(forwards);

        tokio::spawn(serve_unix_forward(
            listener,
            socket_path,
            handle,
            cancelled,
            cleanup,
        ));
        Ok(())
    }
}

fn spawn_pipe_shell(
    mut process: crate::executable::TokioExecutableCommand,
) -> io::Result<SpawnedShell> {
    process
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = process.spawn()?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing child stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing child stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing child stderr"))?;

    Ok(SpawnedShell {
        child,
        input: Box::new(input),
        stdout: Box::new(stdout),
        stderr: Some(Box::new(stderr)),
        #[cfg(unix)]
        pty: None,
    })
}

#[cfg(unix)]
fn spawn_pty_shell(
    mut process: crate::executable::TokioExecutableCommand,
    request: PtyRequest,
) -> io::Result<SpawnedShell> {
    use nix::pty::OpenptyResult;

    let size = pty_winsize(request.size);
    let OpenptyResult { master, slave } =
        nix::pty::openpty(Some(&size), None::<&nix::sys::termios::Termios>)
            .map_err(io::Error::from)?;
    let input = nix::unistd::dup(&master).map_err(io::Error::from)?;
    let control = nix::unistd::dup(&master).map_err(io::Error::from)?;
    let child_stdin = nix::unistd::dup(&slave).map_err(io::Error::from)?;
    let child_stdout = nix::unistd::dup(&slave).map_err(io::Error::from)?;

    process
        .env("TERM", request.term)
        .stdin(std::process::Stdio::from(child_stdin))
        .stdout(std::process::Stdio::from(child_stdout))
        .stderr(std::process::Stdio::from(slave));

    // SAFETY: only async-signal-safe libc calls run after fork. Standard input
    // already refers to the PTY slave; setsid creates a new session before
    // TIOCSCTTY makes that slave the controlling terminal.
    unsafe {
        process.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = process.spawn()?;
    let input = tokio::fs::File::from_std(std::fs::File::from(input));
    let stdout = tokio::fs::File::from_std(std::fs::File::from(master));

    Ok(SpawnedShell {
        child,
        input: Box::new(input),
        stdout: Box::new(stdout),
        stderr: None,
        pty: Some(control),
    })
}

#[cfg(unix)]
fn resize_pty(pty: &OwnedFd, size: PtySize) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    let size = pty_winsize(size);
    // SAFETY: pty owns a valid master descriptor and size points to a fully
    // initialized winsize for the duration of the ioctl.
    if unsafe { libc::ioctl(pty.as_raw_fd(), libc::TIOCSWINSZ as _, &size) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn pty_winsize(size: PtySize) -> nix::pty::Winsize {
    let bounded = |value: u32| value.min(u32::from(u16::MAX)) as u16;
    nix::pty::Winsize {
        ws_row: bounded(size.rows),
        ws_col: bounded(size.columns),
        ws_xpixel: bounded(size.pixel_width),
        ws_ypixel: bounded(size.pixel_height),
    }
}

async fn forward_stream(
    mut ssh: impl AsyncRead + AsyncWrite + Unpin,
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    kind: &'static str,
) {
    if let Err(error) = tokio::io::copy_bidirectional(&mut ssh, &mut stream).await {
        eprintln!("sshd: {kind} stream failed: {error}");
    }
}

async fn serve_tcp_forward(
    listener: tokio::net::TcpListener,
    handle: server::Handle,
    mut cancelled: oneshot::Receiver<()>,
) {
    let connected = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("sshd: inspect forwarded TCP listener: {error}");
            return;
        }
    };

    loop {
        tokio::select! {
            _ = &mut cancelled => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, originator)) => {
                    let handle = handle.clone();
                    tokio::spawn(async move {
                        match handle
                            .channel_open_forwarded_tcpip(
                                connected.ip().to_string(),
                                u32::from(connected.port()),
                                originator.ip().to_string(),
                                u32::from(originator.port()),
                            )
                            .await
                        {
                            Ok(channel) => {
                                forward_stream(channel.into_stream(), stream, "forwarded-tcpip").await;
                            }
                            Err(error) => eprintln!("sshd: open forwarded-tcpip channel: {error}"),
                        }
                    });
                }
                Err(error) => {
                    eprintln!("sshd: accept forwarded TCP connection: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

#[cfg(unix)]
struct UnixSocketPath(PathBuf);

#[cfg(unix)]
impl Drop for UnixSocketPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
async fn serve_unix_forward(
    listener: tokio::net::UnixListener,
    socket_path: PathBuf,
    handle: server::Handle,
    mut cancelled: oneshot::Receiver<()>,
    _cleanup: UnixSocketPath,
) {
    loop {
        tokio::select! {
            _ = &mut cancelled => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let handle = handle.clone();
                    let socket_path = socket_path.clone();

                    tokio::spawn(async move {
                        match handle.channel_open_forwarded_streamlocal(
                            socket_path.to_string_lossy().into_owned(),
                        ).await {
                            Ok(channel) => {
                                forward_stream(
                                    channel.into_stream(),
                                    stream,
                                    "forwarded-streamlocal",
                                ).await;
                            }
                            Err(error) => {
                                eprintln!("sshd: open forwarded-streamlocal channel: {error}");
                            }
                        }
                    });
                }
                Err(error) => {
                    eprintln!("sshd: accept forwarded Unix connection: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

async fn pump(
    mut reader: impl AsyncRead + Unpin,
    handle: server::Handle,
    channel: ChannelId,
    extended: Option<u32>,
    prefix: Option<&'static [u8]>,
) {
    let mut buffer = vec![0_u8; 16 * 1024];

    let mut connected = match prefix {
        Some(data) => handle.data(channel, data.to_vec()).await.is_ok(),
        None => true,
    };
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if !connected {
                    continue;
                }
                let data = buffer[..read].to_vec();
                let result = if let Some(code) = extended {
                    handle.extended_data(channel, code, data).await
                } else {
                    handle.data(channel, data).await
                };
                connected = result.is_ok();
            }
        }
    }
}

fn load_host_key() -> io::Result<PrivateKey> {
    PrivateKey::from_openssh(HOST_KEY).map_err(io::Error::other)
}

fn loopback_address(address: std::net::SocketAddr) -> String {
    let host = if address.is_ipv4() {
        "127.0.0.1"
    } else {
        "::1"
    };

    format!("{host}:{}", address.port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;
    use russh::keys::HashAlg;

    #[test]
    fn help_hides_embedded_relay_token() {
        let help = Options::command().render_long_help().to_string();
        let token = crate::relay_config::token();
        assert!(token.is_empty() || !help.contains(token));
    }

    #[test]
    fn relay_enable_and_disable_flags_conflict() {
        let Err(error) =
            Options::try_parse_from(["sshd", "--relay", "relay.example:6999", "--no-relay"])
        else {
            panic!("relay overrides must conflict");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn default_relay_id_uses_pidns_and_degrades_without_os_identity() {
        assert_eq!(
            format_relay_id(402_653_258_612, 1001, Some("alice"), Some("node")),
            "402653258612@alice@node"
        );
        assert_eq!(format_relay_id(-1, 1001, None, None), "-1@1001@unknown");
        assert_eq!(
            format_relay_id(-1, 1001, Some(""), Some("")),
            "-1@1001@unknown"
        );
    }

    #[test]
    fn disconnect_log_distinguishes_authenticated_and_anonymous_connections() {
        let mut output = Vec::new();
        write_disconnect_log(&mut output, "[::1]:60810", Some("axe-user"), 14, 0)
            .expect("write authenticated disconnect");
        write_disconnect_log(&mut output, "[::1]:60811", None, 1, 0)
            .expect("write anonymous disconnect");

        assert_eq!(
            String::from_utf8(output).expect("disconnect log is UTF-8"),
            concat!(
                "sshd: disconnected [::1]:60810 user=\"axe-user\"; ",
                "connected=14s; active_connections=0\n",
                "sshd: disconnected [::1]:60811 unauthenticated; ",
                "connected=1s; active_connections=0\n",
            )
        );
    }

    #[test]
    fn routine_early_eof_is_not_logged_as_session_error() {
        let mut output = Vec::new();
        let eof = russh::Error::IO(io::Error::new(io::ErrorKind::UnexpectedEof, "early eof"));
        write_session_error(&mut output, &eof).expect("ignore routine EOF");
        assert!(output.is_empty());

        write_session_error(&mut output, &russh::Error::NotAuthenticated)
            .expect("write actionable session error");
        assert_eq!(output, b"sshd: session error: Not yet authenticated\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_quota_rounds_up_to_bounded_runtime_workers() {
        assert_eq!(cpu_max_workers("20000 100000"), Some(1));
        assert_eq!(cpu_max_workers("200000 100000"), Some(2));
        assert_eq!(cpu_max_workers("200001 100000"), Some(3));
        assert_eq!(cpu_max_workers("max 100000"), None);
        assert_eq!(cpu_max_workers("0 100000"), None);
        assert_eq!(cpu_max_workers("20000 0"), None);
        assert_eq!(cpu_max_workers("invalid"), None);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn preserved_executable_survives_unlink() {
        const CHILD_MARKER: &str = "AXE_TEST_INHERITED_EXECUTABLE";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let inherited = crate::executable::current();
            let output = inherited
                .tokio_command()
                .expect("locate inherited executable")
                .arg("--list")
                .output()
                .await
                .expect("launch second generation");
            assert!(output.status.success());
            return;
        }

        let copy =
            std::env::temp_dir().join(format!("axe-preserved-executable-{}", std::process::id()));
        let _ = std::fs::remove_file(&copy);

        std::fs::copy(std::env::current_exe().expect("locate test binary"), &copy)
            .expect("copy test binary");
        let executable = std::fs::File::open(&copy).expect("open copied executable");
        std::fs::remove_file(&copy).expect("unlink copied executable");

        use std::os::fd::AsRawFd as _;

        let output =
            tokio::process::Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()))
                .args([
                    "--exact",
                    "applets::sshd::tests::preserved_executable_survives_unlink",
                    "--nocapture",
                ])
                .env(CHILD_MARKER, "1")
                .output()
                .await
                .expect("execute unlinked binary");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(feature = "on-demand")]
    #[tokio::test]
    async fn index_refresh_waits_for_last_active_session() {
        let activity = Arc::new(SessionActivity::default());
        let first = activity.start();
        let second = activity.start();

        drop(first);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), activity.idle.notified())
                .await
                .is_err()
        );

        drop(second);
        tokio::time::timeout(Duration::from_secs(1), activity.idle.notified())
            .await
            .expect("last completed SSH session did not request an Index refresh");
    }

    #[test]
    fn unavailable_executable_is_rejected_before_bind() {
        let executable = crate::executable::Executable::unavailable();

        assert_eq!(
            require_shell_executable(&executable),
            Err("sshd: cannot locate AXE executable for shell sessions")
        );

        let options = Options::try_parse_from(["sshd"]).expect("parse defaults");
        assert_eq!(start_server(options, Arc::new(executable), None), 1);
    }

    #[tokio::test]
    async fn filesystem_executable_runs_bundled_exec_session() {
        let directory =
            std::env::temp_dir().join(format!("axe-sshd-filesystem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).expect("create SSH filesystem test directory");
        let axe = std::env::current_exe()
            .expect("locate test executable")
            .parent()
            .and_then(Path::parent)
            .expect("locate target directory")
            .join("axe")
            .canonicalize()
            .expect("locate built AXE binary");

        let (client, _, server) = test_connection_with_store_mode(
            directory.clone(),
            None,
            Some(axe.clone()),
            crate::registry::StoreMode::Auto,
        )
        .await;
        let mut channel = client
            .channel_open_session()
            .await
            .expect("open SSH exec channel");
        channel
            .exec(
                true,
                b"printf 'b\\na\\na\\n' | sort | uniq; printf 'x\\n' | xargs -n1 printf '<%s>\\n'; printf 'store:%s\\n' \"$AXE_STORE_MODE\"",
            )
            .await
            .expect("request bundled pipeline");

        let mut stdout = Vec::new();
        let exit_status = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match channel.wait().await {
                    Some(russh::ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
                    Some(russh::ChannelMsg::ExitStatus { exit_status }) => break exit_status,
                    Some(_) => {}
                    None => panic!("SSH exec channel closed without exit status"),
                }
            }
        })
        .await
        .expect("bundled SSH exec did not exit");

        assert_eq!(exit_status, 0);
        assert_eq!(stdout, b"a\nb\n<x>\nstore:auto\n");
        close_test_connection(client, server).await;
        std::fs::remove_dir_all(directory).expect("remove SSH filesystem test directory");
    }

    enum ForwardedChannel {
        Tcp(Channel<russh::client::Msg>),
        #[cfg(unix)]
        Unix(Channel<russh::client::Msg>),
    }

    struct TestClient {
        forwarded: tokio::sync::mpsc::UnboundedSender<ForwardedChannel>,
    }

    impl russh::client::Handler for TestClient {
        type Error = russh::Error;

        async fn check_server_key(
            &mut self,
            _: &russh::keys::PublicKeyOrCertificate,
        ) -> Result<bool, Self::Error> {
            Ok(true)
        }

        async fn server_channel_open_forwarded_tcpip(
            &mut self,
            channel: Channel<russh::client::Msg>,
            _: &str,
            _: u32,
            _: &str,
            _: u32,
            reply: russh::client::ChannelOpenHandle,
            _: &mut russh::client::Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            let _ = self.forwarded.send(ForwardedChannel::Tcp(channel));
            Ok(())
        }

        #[cfg(unix)]
        async fn server_channel_open_forwarded_streamlocal(
            &mut self,
            channel: Channel<russh::client::Msg>,
            _: &str,
            reply: russh::client::ChannelOpenHandle,
            _: &mut russh::client::Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            let _ = self.forwarded.send(ForwardedChannel::Unix(channel));
            Ok(())
        }
    }

    async fn test_connection(
        workdir: PathBuf,
        store_dir: Option<PathBuf>,
        executable: Option<PathBuf>,
    ) -> (
        russh::client::Handle<TestClient>,
        tokio::sync::mpsc::UnboundedReceiver<ForwardedChannel>,
        tokio::task::JoinHandle<()>,
    ) {
        test_connection_with_store_mode(
            workdir,
            store_dir,
            executable,
            crate::registry::StoreMode::Auto,
        )
        .await
    }

    async fn test_connection_with_store_mode(
        workdir: PathBuf,
        store_dir: Option<PathBuf>,
        executable: Option<PathBuf>,
        store_mode: crate::registry::StoreMode,
    ) -> (
        russh::client::Handle<TestClient>,
        tokio::sync::mpsc::UnboundedReceiver<ForwardedChannel>,
        tokio::task::JoinHandle<()>,
    ) {
        test_connection_as(
            workdir,
            store_dir,
            executable,
            store_mode,
            "axe-user",
            Arc::from([String::from("axe-user")]),
        )
        .await
    }

    async fn test_connection_as(
        workdir: PathBuf,
        store_dir: Option<PathBuf>,
        executable: Option<PathBuf>,
        store_mode: crate::registry::StoreMode,
        principal: &str,
        principals: Arc<[String]>,
    ) -> (
        russh::client::Handle<TestClient>,
        tokio::sync::mpsc::UnboundedReceiver<ForwardedChannel>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let ca_key = PrivateKey::from_openssh(HOST_KEY).expect("parse test CA key");
        let user_key = PrivateKey::from_openssh(HOST_KEY).expect("parse test user key");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_secs();

        let mut builder = ssh_key::certificate::Builder::new(
            vec![7; 32],
            user_key.public_key(),
            now.saturating_sub(60),
            now + 3600,
        )
        .expect("create test certificate");
        builder
            .cert_type(CertType::User)
            .expect("set certificate type");
        builder
            .valid_principal(principal)
            .expect("set certificate principal");
        let certificate = builder.sign(&ca_key).expect("sign test certificate");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test SSH server");
        let address = listener.local_addr().expect("read SSH server address");

        let config = Arc::new(server::Config {
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            keys: vec![PrivateKey::from_openssh(HOST_KEY).expect("parse test host key")],
            ..Default::default()
        });

        let handler = SshServer {
            ca_fingerprints: Arc::new(vec![ca_key.public_key().fingerprint(HashAlg::Sha256)]),
            principals,
            workdir: Arc::new(workdir),
            store_dir: Arc::new(store_dir),
            store_mode,
            #[cfg(feature = "on-demand")]
            index_activity: None,
            inputs: Arc::new(Mutex::new(HashMap::new())),
            session_channels: Arc::new(Mutex::new(HashMap::new())),
            sftp_tasks: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(unix)]
            ptys: Arc::new(Mutex::new(HashMap::new())),
            tcp_forwards: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(unix)]
            unix_forwards: Arc::new(Mutex::new(HashMap::new())),
            executable: match executable {
                Some(path) => Arc::new(
                    crate::executable::Executable::from_filesystem_path(&path)
                        .expect("open fixture executable"),
                ),
                None => crate::executable::current(),
            },
            active_connections: Arc::new(AtomicUsize::new(0)),
            welcome_sent: false,
            connection: None,
        };

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept SSH client");
            let session = server::run_stream(config, stream, handler)
                .await
                .expect("start SSH session");
            let _ = session.await;
        });

        let (forwarded, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut client = russh::client::connect(
            Arc::new(russh::client::Config::default()),
            address,
            TestClient { forwarded },
        )
        .await
        .expect("connect SSH client");
        let auth = client
            .authenticate_openssh_cert(principal, Arc::new(user_key), certificate)
            .await
            .expect("authenticate SSH certificate");
        assert!(auth.success());

        (client, receiver, server)
    }

    #[tokio::test]
    async fn cli_principals_replace_embedded_defaults() {
        const PRINCIPAL: &str = "axe-runtime-principal-test";

        assert!(
            !crate::sshd_config::principals()
                .iter()
                .any(|principal| principal == PRINCIPAL)
        );
        let options =
            Options::try_parse_from(["sshd", "--principals", PRINCIPAL]).expect("parse CLI");
        let principals = Arc::from(options.principals.expect("CLI principals"));
        let (client, _, server) = test_connection_as(
            PathBuf::from("."),
            None,
            None,
            crate::registry::StoreMode::Auto,
            PRINCIPAL,
            principals,
        )
        .await;

        close_test_connection(client, server).await;
    }

    async fn close_test_connection(
        client: russh::client::Handle<TestClient>,
        server: tokio::task::JoinHandle<()>,
    ) {
        let _ = client
            .disconnect(russh::Disconnect::ByApplication, "test complete", "en")
            .await;
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("SSH server did not stop")
            .expect("SSH server task failed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pty_shell_welcomes_once_per_connection() {
        async fn run_shell(client: &russh::client::Handle<TestClient>) -> Vec<u8> {
            let mut channel = client
                .channel_open_session()
                .await
                .expect("open PTY session channel");
            channel
                .request_pty(true, "xterm-axe", 91, 37, 0, 0, &[])
                .await
                .expect("request PTY");
            channel
                .request_shell(true)
                .await
                .expect("request interactive shell");
            channel
                .window_change(121, 44, 0, 0)
                .await
                .expect("resize PTY");
            channel.data(&b"x\n"[..]).await.expect("write PTY input");

            let mut stdout = Vec::new();
            let exit_status = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match channel.wait().await {
                        Some(russh::ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
                        Some(russh::ChannelMsg::ExitStatus { exit_status }) => break exit_status,
                        Some(_) => {}
                        None => panic!("PTY channel closed without exit status"),
                    }
                }
            })
            .await
            .expect("PTY shell did not exit");
            assert_eq!(exit_status, 0);
            stdout
        }

        let directory = std::env::temp_dir().join(format!("axe-sshd-pty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).expect("create PTY test directory");
        let source = directory.join("pty-fixture.c");
        let executable = directory.join("pty-fixture");
        std::fs::write(
            &source,
            b"#include <stdio.h>\n#include <stdlib.h>\n#include <sys/ioctl.h>\n#include <unistd.h>\nint main(void) {\n if (!isatty(0) || !isatty(1)) return 2;\n if (getchar() == EOF) return 3;\n struct winsize size;\n if (ioctl(0, TIOCGWINSZ, &size) < 0) return 4;\n const char *term = getenv(\"TERM\");\n printf(\"PTY %u %u %s\\n\", size.ws_col, size.ws_row, term ? term : \"\");\n return 0;\n}\n",
        )
        .expect("write PTY fixture source");
        let compiled = std::process::Command::new("cc")
            .arg("-Os")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .expect("compile PTY fixture");
        assert!(compiled.success(), "compile PTY fixture");

        let (client, _, server) = test_connection(directory.clone(), None, Some(executable)).await;
        let first = run_shell(&client).await;
        let second = run_shell(&client).await;

        let mut expected_first = SHELL_WELCOME.to_vec();
        expected_first.extend_from_slice(b"x\r\nPTY 121 44 xterm-axe\r\n");
        assert_eq!(first, expected_first);
        assert_eq!(second, b"x\r\nPTY 121 44 xterm-axe\r\n");

        close_test_connection(client, server).await;
        std::fs::remove_dir_all(directory).expect("remove PTY test directory");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires a Docker-compatible container runtime"]
    async fn vanilla_openssh_client_observes_pinned_storage_environment() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};
        use testcontainers::{
            GenericImage, ImageExt,
            core::{ExecCommand, Host, Mount, WaitFor},
            runners::AsyncRunner,
        };

        let directory =
            std::env::temp_dir().join(format!("axe-openssh-client-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let keys = directory.join("keys");
        let workdir = directory.join("work");
        let store_dir = directory.join("store");
        std::fs::create_dir_all(&keys).expect("create OpenSSH key directory");
        std::fs::create_dir(&workdir).expect("create SSH workdir");
        std::fs::create_dir(&store_dir).expect("create SSH AXE Store");
        let directory = directory
            .canonicalize()
            .expect("canonicalize OpenSSH test directory");
        let keys = keys
            .canonicalize()
            .expect("canonicalize OpenSSH key directory");
        let workdir = workdir.canonicalize().expect("canonicalize SSH workdir");
        let store_dir = store_dir
            .canonicalize()
            .expect("canonicalize SSH AXE Store");

        let ca_key = PrivateKey::from_openssh(HOST_KEY).expect("parse test CA key");
        let user_key = PrivateKey::from_openssh(HOST_KEY).expect("parse test user key");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_secs();
        let mut builder = ssh_key::certificate::Builder::new(
            vec![11; 32],
            user_key.public_key(),
            now.saturating_sub(60),
            now + 3600,
        )
        .expect("create OpenSSH test certificate");
        builder
            .cert_type(CertType::User)
            .expect("set certificate type");
        builder
            .valid_principal("axe-user")
            .expect("set certificate principal");
        let certificate = builder
            .sign(&ca_key)
            .expect("sign OpenSSH test certificate");
        let private_key_path = keys.join("id_ed25519");
        user_key
            .write_openssh_file(&private_key_path, ssh_key::LineEnding::LF)
            .expect("write OpenSSH private key");
        certificate
            .write_file(&keys.join("id_ed25519-cert.pub"))
            .expect("write OpenSSH certificate");
        std::fs::set_permissions(&private_key_path, std::fs::Permissions::from_mode(0o600))
            .expect("secure OpenSSH private key");

        let source = directory.join("session-fixture.c");
        let executable = directory.join("session-fixture");
        std::fs::write(
            &source,
            b"#include <stdio.h>\n#include <stdlib.h>\n#include <unistd.h>\nint main(void) {\n char cwd[4096];\n if (!getcwd(cwd, sizeof(cwd))) return 2;\n const char *axe = getenv(\"AXE\");\n const char *work = getenv(\"AXE_WORK_DIR\");\n const char *store = getenv(\"AXE_STORE_DIR\");\n const char *mode = getenv(\"AXE_STORE_MODE\");\n if (!axe || !work || !store || !mode) return 3;\n printf(\"%s\\n%s\\n%s\\n%s\\n%s\\n\", cwd, axe, work, store, mode);\n return 0;\n}\n",
        )
        .expect("write SSH session fixture source");
        let compiled = std::process::Command::new("cc")
            .arg("-Os")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .expect("compile SSH session fixture");
        assert!(compiled.success(), "compile SSH session fixture");

        let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
            .await
            .expect("bind test SSH server");
        let address = listener.local_addr().expect("read SSH server address");
        let config = Arc::new(server::Config {
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            keys: vec![PrivateKey::from_openssh(HOST_KEY).expect("parse test host key")],
            ..Default::default()
        });
        let handler = SshServer {
            ca_fingerprints: Arc::new(vec![ca_key.public_key().fingerprint(HashAlg::Sha256)]),
            principals: Arc::from([String::from("axe-user")]),
            workdir: Arc::new(workdir.clone()),
            store_dir: Arc::new(Some(store_dir.clone())),
            store_mode: crate::registry::StoreMode::CacheOnly,
            #[cfg(feature = "on-demand")]
            index_activity: None,
            inputs: Arc::new(Mutex::new(HashMap::new())),
            session_channels: Arc::new(Mutex::new(HashMap::new())),
            sftp_tasks: Arc::new(Mutex::new(HashMap::new())),
            ptys: Arc::new(Mutex::new(HashMap::new())),
            tcp_forwards: Arc::new(Mutex::new(HashMap::new())),
            unix_forwards: Arc::new(Mutex::new(HashMap::new())),
            executable: Arc::new(
                crate::executable::Executable::from_filesystem_path(&executable)
                    .expect("open SSH session fixture"),
            ),
            active_connections: Arc::new(AtomicUsize::new(0)),
            welcome_sent: false,
            connection: None,
        };
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept OpenSSH client");
            let session = server::run_stream(config, stream, handler)
                .await
                .expect("start OpenSSH session");
            let _ = session.await;
        });

        let container = GenericImage::new("alpine", "3.22.1")
            .with_wait_for(WaitFor::message_on_stdout("OPENSSH_READY"))
            .with_cmd([
                "sh",
                "-c",
                "apk add --no-cache openssh-client >/dev/null && echo OPENSSH_READY && sleep 600",
            ])
            .with_mount(Mount::bind_mount(
                keys.to_string_lossy().into_owned(),
                "/keys",
            ))
            .with_host("host.testcontainers.internal", Host::HostGateway)
            .start()
            .await
            .expect("start vanilla OpenSSH client container");
        let mut result = container
            .exec(ExecCommand::new([
                "ssh",
                "-F",
                "/dev/null",
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-i",
                "/keys/id_ed25519",
                "-p",
                &address.port().to_string(),
                "axe-user@host.testcontainers.internal",
                "true",
            ]))
            .await
            .expect("execute vanilla OpenSSH client");
        let stdout = result.stdout_to_vec().await.expect("read OpenSSH stdout");
        let stderr = result.stderr_to_vec().await.expect("read OpenSSH stderr");
        assert_eq!(
            result.exit_code().await.expect("read OpenSSH exit code"),
            Some(0),
            "OpenSSH stderr: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&stdout),
            format!(
                "{}\ntrue\n{}\n{}\ncache-only\n",
                workdir.display(),
                workdir.display(),
                store_dir.display()
            )
        );

        drop(container);
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("SSH server did not stop")
            .expect("SSH server task failed");
        std::fs::remove_dir_all(&directory).expect("remove OpenSSH test directory");
    }

    #[tokio::test]
    async fn direct_tcpip_forwards_streams_for_local_and_socks_clients() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind TCP echo server");
        let target = listener.local_addr().expect("read TCP echo address");

        let echo = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept TCP forward");
            let (mut read, mut write) = tokio::io::split(stream);
            tokio::io::copy(&mut read, &mut write)
                .await
                .expect("echo TCP stream");
        });

        let (client, _, server) = test_connection(PathBuf::from("."), None, None).await;

        let channel = client
            .channel_open_direct_tcpip(
                target.ip().to_string(),
                u32::from(target.port()),
                "127.0.0.1",
                12345,
            )
            .await
            .expect("open direct-tcpip channel");

        let mut stream = channel.into_stream();
        stream
            .write_all(b"through direct-tcpip")
            .await
            .expect("send");
        let mut response = [0; 20];
        stream.read_exact(&mut response).await.expect("receive");
        assert_eq!(&response, b"through direct-tcpip");

        stream.shutdown().await.expect("close direct channel");
        echo.await.expect("TCP echo task failed");

        close_test_connection(client, server).await;
    }

    #[tokio::test]
    async fn reverse_tcpip_forward_accepts_connections() {
        let (client, mut forwarded, server) = test_connection(PathBuf::from("."), None, None).await;
        let port = client
            .tcpip_forward("127.0.0.1", 0)
            .await
            .expect("request reverse TCP forward");
        assert_ne!(port, 0);

        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port as u16))
            .await
            .expect("connect reverse TCP listener");
        let ForwardedChannel::Tcp(channel) =
            tokio::time::timeout(Duration::from_secs(5), forwarded.recv())
                .await
                .expect("forwarded TCP channel timed out")
                .expect("forwarded TCP channel missing")
        else {
            panic!("received Unix channel for TCP forward");
        };

        let mut channel = channel.into_stream();
        socket.write_all(b"request").await.expect("send request");
        let mut request = [0; 7];
        channel
            .read_exact(&mut request)
            .await
            .expect("read request");
        assert_eq!(&request, b"request");

        channel.write_all(b"response").await.expect("send response");
        let mut response = [0; 8];
        socket
            .read_exact(&mut response)
            .await
            .expect("read response");
        assert_eq!(&response, b"response");

        client
            .cancel_tcpip_forward("127.0.0.1", port)
            .await
            .expect("cancel reverse TCP forward");

        close_test_connection(client, server).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_forwarding_works_in_both_directions() {
        let directory =
            std::env::temp_dir().join(format!("axe-ssh-forward-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).expect("create Unix socket directory");

        let direct_path = directory.join("direct.sock");
        let reverse_path = directory.join("reverse.sock");
        let listener = tokio::net::UnixListener::bind(&direct_path).expect("bind Unix echo server");
        let echo = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept Unix forward");
            let (mut read, mut write) = tokio::io::split(stream);
            tokio::io::copy(&mut read, &mut write)
                .await
                .expect("echo Unix stream");
        });

        let (client, mut forwarded, server) = test_connection(PathBuf::from("."), None, None).await;

        let channel = client
            .channel_open_direct_streamlocal(direct_path.to_string_lossy())
            .await
            .expect("open direct-streamlocal channel");
        let mut direct = channel.into_stream();
        direct.write_all(b"direct").await.expect("send direct");
        let mut response = [0; 6];
        direct.read_exact(&mut response).await.expect("read direct");
        assert_eq!(&response, b"direct");
        direct.shutdown().await.expect("close direct Unix channel");
        echo.await.expect("Unix echo task failed");

        client
            .streamlocal_forward(reverse_path.to_string_lossy())
            .await
            .expect("request reverse Unix forward");

        let mut socket = tokio::net::UnixStream::connect(&reverse_path)
            .await
            .expect("connect reverse Unix listener");
        let ForwardedChannel::Unix(channel) =
            tokio::time::timeout(Duration::from_secs(5), forwarded.recv())
                .await
                .expect("forwarded Unix channel timed out")
                .expect("forwarded Unix channel missing")
        else {
            panic!("received TCP channel for Unix forward");
        };

        let mut channel = channel.into_stream();
        socket.write_all(b"unix").await.expect("send Unix request");
        let mut request = [0; 4];
        channel
            .read_exact(&mut request)
            .await
            .expect("read Unix request");
        assert_eq!(&request, b"unix");

        channel.write_all(b"ok").await.expect("send Unix response");
        let mut response = [0; 2];
        socket
            .read_exact(&mut response)
            .await
            .expect("read Unix response");
        assert_eq!(&response, b"ok");

        client
            .cancel_streamlocal_forward(reverse_path.to_string_lossy())
            .await
            .expect("cancel reverse Unix forward");

        close_test_connection(client, server).await;
        let _ = std::fs::remove_file(direct_path);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn sftp_subsystem_transfers_files() {
        let directory = std::env::temp_dir().join(format!("axe-ssh-sftp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).expect("create SFTP directory");

        let (client, _, server) = test_connection(directory.clone(), None, None).await;

        let channel = client
            .channel_open_session()
            .await
            .expect("open SFTP session channel");
        channel
            .request_subsystem(true, "sftp")
            .await
            .expect("request SFTP subsystem");
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .expect("start SFTP subsystem");

        let mut file = sftp.create("artifact").await.expect("create SFTP file");
        file.write_all(b"scp-compatible payload")
            .await
            .expect("write SFTP file");
        file.close().await.expect("close SFTP file");

        assert_eq!(
            sftp.read("artifact").await.expect("read SFTP file"),
            b"scp-compatible payload"
        );

        drop(sftp);
        close_test_connection(client, server).await;

        assert_eq!(
            std::fs::read(directory.join("artifact")).expect("read transferred artifact"),
            b"scp-compatible payload"
        );
        std::fs::remove_dir_all(directory).expect("remove SFTP directory");
    }
}
