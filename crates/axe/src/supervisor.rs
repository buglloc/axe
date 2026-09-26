use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;

const SUPERVISOR_FLAG: &str = "--axe-supervisor";
const WORKER_FLAG: &str = "--axe-worker";
const READY_FD_ENV: &str = "AXE_INTERNAL_READY_FD";
const READY_BYTE: u8 = 1;
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct ReadyNotifier(File);

impl ReadyNotifier {
    pub(crate) fn notify(mut self) -> io::Result<()> {
        self.0.write_all(&[READY_BYTE])?;
        self.0.flush()
    }
}

pub(crate) struct ShutdownSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
}

impl ShutdownSignals {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            quit: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::quit())?,
        })
    }

    pub(crate) async fn recv(&mut self) -> io::Result<()> {
        tokio::select! {
            signal = self.interrupt.recv() => signal
                .map(|_| ())
                .ok_or_else(|| io::Error::other("SIGINT handler closed")),
            signal = self.terminate.recv() => signal
                .map(|_| ())
                .ok_or_else(|| io::Error::other("SIGTERM handler closed")),
            signal = self.quit.recv() => signal
                .map(|_| ())
                .ok_or_else(|| io::Error::other("SIGQUIT handler closed")),
        }
    }
}

pub fn is_supervisor(args: &[OsString]) -> bool {
    args.iter().any(|arg| arg == OsStr::new(SUPERVISOR_FLAG))
}

pub fn strip_internal(args: &[OsString]) -> Vec<OsString> {
    args.iter()
        .filter(|arg| {
            *arg != OsStr::new("--daemon")
                && *arg != OsStr::new(SUPERVISOR_FLAG)
                && *arg != OsStr::new(WORKER_FLAG)
        })
        .cloned()
        .collect()
}

pub(crate) fn take_ready_notifier() -> io::Result<Option<ReadyNotifier>> {
    let value = std::env::var_os(READY_FD_ENV);
    // SAFETY: workers consume the inherited readiness descriptor before they
    // create a runtime or any other thread.
    unsafe { std::env::remove_var(READY_FD_ENV) };
    let Some(value) = value else {
        return Ok(None);
    };
    let descriptor = value
        .to_str()
        .and_then(|value| value.parse::<RawFd>().ok())
        .filter(|descriptor| *descriptor > libc::STDERR_FILENO)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid readiness descriptor")
        })?;

    // SAFETY: F_GETFD validates the inherited descriptor without taking ownership.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the descriptor is live. CLOEXEC prevents it from leaking to
    // commands subsequently launched by the daemon worker.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: the validated descriptor was deliberately transferred to this
    // worker and has no other owner in this process.
    Ok(Some(ReadyNotifier(unsafe {
        File::from_raw_fd(descriptor)
    })))
}

pub fn daemonize(applet: &str, args: &[OsString], log_path: Option<&OsStr>) -> io::Result<u32> {
    let (mut readiness, notifier) = UnixStream::pair()?;
    readiness.set_read_timeout(Some(READY_TIMEOUT))?;

    let mut command = crate::executable::current().command()?;
    let output = log_path
        .filter(|path| !path.is_empty())
        .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());
    let stdout = output
        .as_ref()
        .and_then(|file| file.try_clone().ok())
        .map_or_else(Stdio::null, Stdio::from);
    let stderr = output.map_or_else(Stdio::null, Stdio::from);
    let ready_fd = notifier.as_raw_fd();

    command
        .arg(brush_shell::bundled::DISPATCH_FLAG)
        .arg(applet)
        .args(strip_internal(args))
        .arg(SUPERVISOR_FLAG)
        .env(READY_FD_ENV, ready_fd.to_string())
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    detach_command(&mut command, ready_fd);

    let mut child = command.spawn()?;
    drop(notifier);

    let mut byte = [0];
    match readiness.read_exact(&mut byte) {
        Ok(()) if byte[0] == READY_BYTE => Ok(child.id()),
        Ok(()) => {
            terminate_process_group(&mut child);
            Err(io::Error::other(
                "daemon worker sent an invalid readiness response",
            ))
        }
        Err(error) => {
            terminate_process_group(&mut child);
            Err(io::Error::new(
                error.kind(),
                format!("daemon worker did not become ready: {error}"),
            ))
        }
    }
}

pub fn supervise_applet(applet: &str, args: &[OsString]) -> i32 {
    let executable = crate::executable::current();
    let worker_args = strip_internal(args);
    let mut signals = match Signals::new([SIGTERM, SIGINT, SIGQUIT, SIGHUP]) {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("{applet}: install supervisor signal handlers: {error}");
            return 1;
        }
    };
    let mut readiness = inherited_ready_fd();
    let mut failures = 0_u32;

    loop {
        let mut command = match executable.command() {
            Ok(command) => command,
            Err(error) => {
                eprintln!("{applet}: cannot preserve executable: {error}");
                close_readiness(&mut readiness);
                return 1;
            }
        };
        command
            .arg(brush_shell::bundled::DISPATCH_FLAG)
            .arg(applet)
            .args(&worker_args)
            .arg(WORKER_FLAG);
        install_parent_death_signal(&mut command);

        let started = Instant::now();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if readiness.is_some() => {
                eprintln!("{applet}: initial worker launch failed ({error})");
                close_readiness(&mut readiness);
                return 1;
            }
            Err(error) => {
                eprintln!("{applet}: worker launch failed ({error}); retrying");
                failures = failures.saturating_add(1).min(6);
                thread::sleep(retry_delay(failures));
                continue;
            }
        };
        close_readiness(&mut readiness);

        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {}
                Err(error) => break Err(error),
            }

            let signal = signals.pending().next();
            if let Some(signal) = signal {
                if let Err(error) = signal_process(child.id(), signal) {
                    eprintln!("{applet}: forward signal {signal} to worker: {error}");
                }
                if matches!(signal, SIGTERM | SIGINT | SIGQUIT) {
                    wait_for_shutdown(&mut child);
                    return 0;
                }
            }
            thread::sleep(Duration::from_millis(50));
        };

        if started.elapsed() >= Duration::from_secs(30) {
            failures = 0;
        } else {
            failures = failures.saturating_add(1).min(6);
        }

        match status {
            Ok(status) => eprintln!("{applet}: worker exited ({status}); restarting"),
            Err(error) => eprintln!("{applet}: wait for worker failed ({error}); restarting"),
        }
        thread::sleep(retry_delay(failures));
    }
}

fn inherited_ready_fd() -> Option<RawFd> {
    std::env::var_os(READY_FD_ENV)?
        .to_str()?
        .parse::<RawFd>()
        .ok()
        .filter(|descriptor| *descriptor > libc::STDERR_FILENO)
}

fn close_readiness(descriptor: &mut Option<RawFd>) {
    let Some(descriptor) = descriptor.take() else {
        return;
    };
    // SAFETY: the supervisor owns the inherited readiness descriptor and closes
    // its copy immediately after transferring it to the first worker.
    unsafe { libc::close(descriptor) };
    // SAFETY: the supervisor is single-threaded.
    unsafe { std::env::remove_var(READY_FD_ENV) };
}

fn signal_process(pid: u32, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: kill accepts any numeric PID; the live child PID comes from Child.
    if unsafe { libc::kill(pid as libc::pid_t, signal) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wait_for_shutdown(child: &mut Child) -> Option<ExitStatus> {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(None) | Err(_) => break,
        }
    }

    let _ = signal_process(child.id(), libc::SIGKILL);
    child.wait().ok()
}

fn terminate_process_group(child: &mut Child) {
    let process_group = -(child.id() as libc::pid_t);
    // SAFETY: the supervisor called setsid before exec, so its PID is also its
    // process-group ID. A negative PID signals the complete daemon group.
    unsafe { libc::kill(process_group, libc::SIGTERM) };
    if wait_for_shutdown(child).is_none() {
        // SAFETY: same process-group invariant as above.
        unsafe { libc::kill(process_group, libc::SIGKILL) };
        let _ = child.wait();
    }
}

fn retry_delay(failures: u32) -> Duration {
    let ceiling_ms = (1_u64 << failures.min(5)) * 1_000;
    let half = ceiling_ms / 2;
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| u64::from(time.subsec_nanos()) % (half + 1));
    Duration::from_millis((half + jitter).min(30_000))
}

fn detach_command(command: &mut crate::executable::ExecutableCommand, ready_fd: RawFd) {
    // SAFETY: the callback uses only async-signal-safe libc calls between fork
    // and exec. `ready_fd` remains live until spawn returns in the parent.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            let flags = libc::fcntl(ready_fd, libc::F_GETFD);
            if flags == -1 || libc::fcntl(ready_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
fn install_parent_death_signal(command: &mut crate::executable::ExecutableCommand) {
    // SAFETY: prctl/getppid are async-signal-safe syscalls. The post-prctl
    // parent check closes the race where the supervisor dies before PR_SET_PDEATHSIG.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() == 1 {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "supervisor exited before worker exec",
                ));
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn install_parent_death_signal(_: &mut crate::executable::ExecutableCommand) {}
