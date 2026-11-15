#![cfg(unix)]

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(150);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "axe-tail-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private tail fixture: {error}"),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct OwnedChild {
    child: Child,
    separate_group: bool,
    status: Option<ExitStatus>,
}

impl OwnedChild {
    fn spawn(command: &mut Command, separate_group: bool) -> io::Result<Self> {
        if separate_group {
            command.process_group(0);
        }
        Ok(Self {
            child: command.spawn()?,
            separate_group,
            status: None,
        })
    }

    fn status(&mut self) -> Option<ExitStatus> {
        if self.status.is_none() {
            self.status = self.child.try_wait().expect("query owned child status");
        }
        self.status
    }

    fn terminate(&mut self) {
        self.child.kill().expect("terminate owned live child");
        self.status = Some(self.child.wait().expect("reap terminated child"));
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.status.is_none() {
            if self.separate_group {
                let pid = i32::try_from(self.child.id()).expect("child PID fits pid_t");
                // SAFETY: this unreaped child owns the process group created at spawn.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Running {
    owned: OwnedChild,
    stdout: ChildStdout,
    stderr: ChildStderr,
    out: Vec<u8>,
    err: Vec<u8>,
    out_eof: bool,
    err_eof: bool,
}

fn nonblocking(fd: RawFd) {
    // SAFETY: fd is a live pipe owned by Running; fcntl does not transfer ownership.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert_ne!(flags, -1, "{}", io::Error::last_os_error());
    // SAFETY: preserve the pipe's existing flags and only change blocking behavior.
    assert_ne!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        -1,
        "{}",
        io::Error::last_os_error(),
    );
}

fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>, eof: &mut bool) {
    if *eof {
        return;
    }
    let mut buffer = [0; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *eof = true;
                return;
            }
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(error) => panic!("read applet output: {error}"),
        }
        assert!(
            bytes.len() <= 1024 * 1024,
            "unexpectedly unbounded applet output"
        );
    }
}

impl Running {
    fn spawn(command: &mut Command, separate_group: bool) -> io::Result<Self> {
        let mut owned = OwnedChild::spawn(command, separate_group)?;
        let stdout = owned.child.stdout.take().expect("piped stdout");
        let stderr = owned.child.stderr.take().expect("piped stderr");
        nonblocking(stdout.as_raw_fd());
        nonblocking(stderr.as_raw_fd());
        Ok(Self {
            owned,
            stdout,
            stderr,
            out: Vec::new(),
            err: Vec::new(),
            out_eof: false,
            err_eof: false,
        })
    }

    fn pump(&mut self) {
        drain(&mut self.stdout, &mut self.out, &mut self.out_eof);
        drain(&mut self.stderr, &mut self.err, &mut self.err_eof);
    }

    fn poll(&self, deadline: Instant) {
        let mut fds = [
            libc::pollfd {
                fd: if self.out_eof {
                    -1
                } else {
                    self.stdout.as_raw_fd()
                },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if self.err_eof {
                    -1
                } else {
                    self.stderr.as_raw_fd()
                },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = remaining.as_millis().clamp(1, 20) as i32;
        // SAFETY: fds is writable for both pollfd entries and all nonnegative FDs are owned.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
        assert!(
            result >= 0 || io::Error::last_os_error().kind() == io::ErrorKind::Interrupted,
            "poll applet output: {}",
            io::Error::last_os_error(),
        );
    }

    fn expect_stdout(&mut self, expected: &[u8]) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump();
            assert!(
                expected.starts_with(&self.out),
                "unexpected bytes: {:?}; expected {expected:?}; stderr {:?}",
                self.out,
                String::from_utf8_lossy(&self.err),
            );
            if self.out == expected {
                self.assert_running();
                return;
            }
            assert!(
                self.owned.status().is_none() && Instant::now() < deadline,
                "tail did not produce {expected:?}: status {:?}, stdout {:?}, stderr {:?}",
                self.owned.status,
                self.out,
                String::from_utf8_lossy(&self.err),
            );
            self.poll(deadline);
        }
    }

    fn assert_running(&mut self) {
        assert!(
            self.owned.status().is_none(),
            "tail exited early: {:?}; stdout {:?}; stderr {:?}",
            self.owned.status,
            self.out,
            String::from_utf8_lossy(&self.err),
        );
    }

    fn expect_stderr_since(&mut self, start: usize, expected: &[u8]) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump();
            if self.err[start..]
                .windows(expected.len())
                .any(|part| part == expected)
            {
                self.assert_running();
                return;
            }
            self.assert_running();
            assert!(
                Instant::now() < deadline,
                "missing diagnostic {expected:?}: {:?}",
                String::from_utf8_lossy(&self.err),
            );
            self.poll(deadline);
        }
    }

    fn expect_quiet(&mut self, expected: &[u8]) {
        let deadline = Instant::now() + QUIET;
        loop {
            self.pump();
            assert_eq!(
                self.out,
                expected,
                "stderr {:?}",
                String::from_utf8_lossy(&self.err)
            );
            self.assert_running();
            if Instant::now() >= deadline {
                return;
            }
            self.poll(deadline);
        }
    }

    fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if let Some(status) = self.owned.status() {
                self.pump();
                if self.out_eof && self.err_eof {
                    return status;
                }
            }
            assert!(
                Instant::now() < deadline,
                "child did not finish: stdout {:?}, stderr {:?}",
                self.out,
                String::from_utf8_lossy(&self.err),
            );
            self.poll(deadline);
        }
    }
}

fn applet(root: &Path, name: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", name, "--"])
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_MODE", "off")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn tail(root: &Path, polling: bool, follow: &str, path: Option<&Path>) -> Command {
    let mut command = applet(root, "tail");
    command.args([follow, "-n", "+1", "-s", "0.02", "--max-unchanged-stats=1"]);
    if polling {
        command.arg("---disable-inotify");
    }
    if let Some(path) = path {
        command.arg(path);
    }
    command
}

fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn replace_symlink(root: &Path, link: &Path, target: &Path) {
    let temporary = root.join("next-link");
    symlink(target, &temporary).unwrap();
    fs::rename(temporary, link).unwrap();
}

#[test]
fn regular_stdin_follows_appends_and_truncation_from_its_initial_offset() {
    for polling in [false, true] {
        for explicit_stdin in [false, true] {
            for nonzero_offset in [false, true] {
                let scratch = Scratch::new("stdin");
                let path = scratch.0.join("input");
                let mut contents = if nonzero_offset {
                    b"skipped\n".repeat(8192)
                } else {
                    Vec::new()
                };
                let offset = contents.len() as u64;
                contents.extend_from_slice(b"ready\n");
                fs::write(&path, contents).unwrap();
                let mut input = File::open(&path).unwrap();
                input.seek(SeekFrom::Start(offset)).unwrap();
                let mut command = applet(&scratch.0, "tail");
                command.args(["-f", "-n", "10", "-s", "0.02"]);
                if polling {
                    command.arg("---disable-inotify");
                }
                if explicit_stdin {
                    command.arg("-");
                }
                command.stdin(input);
                let mut running = Running::spawn(&mut command, true).unwrap();
                let mut expected = b"ready\n".to_vec();
                running.expect_stdout(&expected);
                append(&path, b"appended\n");
                expected.extend_from_slice(b"appended\n");
                running.expect_stdout(&expected);
                fs::write(&path, b"t\n").unwrap();
                expected.extend_from_slice(b"t\n");
                running.expect_stdout(&expected);
                running.expect_quiet(&expected);
            }
        }
    }
}

#[test]
fn stdin_pipe_follow_exits_successfully_at_eof() {
    for polling in [false, true] {
        for explicit_stdin in [false, true] {
            let scratch = Scratch::new("pipe");
            let mut command = tail(
                &scratch.0,
                polling,
                "-f",
                explicit_stdin.then_some(Path::new("-")),
            );
            command.stdin(Stdio::piped());
            let mut running = Running::spawn(&mut command, true).unwrap();
            running
                .owned
                .child
                .stdin
                .take()
                .unwrap()
                .write_all(b"pipe\nbytes\0\xff\n")
                .unwrap();
            assert_eq!(running.wait(WAIT).code(), Some(0));
            assert_eq!(running.out, b"pipe\nbytes\0\xff\n");
        }
    }
}

#[test]
fn descriptor_follow_tracks_symlink_and_cross_directory_hardlink_targets() {
    for polling in [false, true] {
        for topology in [
            "same-directory-symlink",
            "other-directory-symlink",
            "hardlink",
        ] {
            let scratch = Scratch::new(topology);
            let other = scratch.0.join("other");
            fs::create_dir(&other).unwrap();
            let target_root = if topology == "same-directory-symlink" {
                &scratch.0
            } else {
                &other
            };
            let target = target_root.join(OsStr::from_bytes(b"cqudp-server.log.2026-11-15-\xff"));
            let watched = scratch.0.join("cqudp-server.log");
            fs::write(&target, b"ready\n").unwrap();
            if topology == "hardlink" {
                fs::hard_link(&target, &watched).unwrap();
            } else {
                let relative = target.strip_prefix(&scratch.0).unwrap();
                symlink(relative, &watched).unwrap();
            }
            let mut writer = OpenOptions::new().append(true).open(&target).unwrap();
            let mut running =
                Running::spawn(&mut tail(&scratch.0, polling, "-f", Some(&watched)), true).unwrap();
            running.expect_stdout(b"ready\n");
            writer.write_all(b"target\0\xff\n").unwrap();
            let mut expected = b"ready\ntarget\0\xff\n".to_vec();
            running.expect_stdout(&expected);
            if topology != "hardlink" {
                let next = target_root.join("cqudp-server.log.2026-11-16");
                fs::write(&next, b"WRONG NEW SYMLINK TARGET\n").unwrap();
                replace_symlink(&scratch.0, &watched, next.strip_prefix(&scratch.0).unwrap());
                writer.write_all(b"original target\n").unwrap();
                expected.extend_from_slice(b"original target\n");
                running.expect_stdout(&expected);
                writer.set_len(0).unwrap();
                writer.write_all(b"t\n").unwrap();
                expected.extend_from_slice(b"t\n");
                running.expect_stdout(&expected);
            }
            running.expect_quiet(&expected);
        }
    }
}

#[test]
fn descriptor_follow_survives_cross_directory_rename_and_unlink() {
    for polling in [false, true] {
        for unlink in [false, true] {
            let scratch = Scratch::new("detached");
            let path = scratch.0.join(OsStr::from_bytes(b"log-\xff"));
            let elsewhere = scratch.0.join("elsewhere");
            fs::create_dir(&elsewhere).unwrap();
            fs::write(&path, b"ready\n").unwrap();
            let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
            let mut running =
                Running::spawn(&mut tail(&scratch.0, polling, "-f", Some(&path)), true).unwrap();
            running.expect_stdout(b"ready\n");
            let moved = elsewhere.join("moved");
            if unlink {
                fs::remove_file(&path).unwrap();
                writer.write_all(b"detached\n").unwrap();
            } else {
                fs::rename(&path, &moved).unwrap();
                append(&moved, b"detached\n");
            }
            running.expect_stdout(b"ready\ndetached\n");
            writer.set_len(0).unwrap();
            writer.write_all(b"t\n").unwrap();
            running.expect_stdout(b"ready\ndetached\nt\n");
            running.expect_quiet(b"ready\ndetached\nt\n");
        }
    }
}

#[test]
fn descriptor_follow_never_reopens_a_replaced_or_truncated_pathname() {
    for polling in [false, true] {
        let scratch = Scratch::new("rotation");
        let path = scratch.0.join("log");
        fs::write(&path, b"ready\n").unwrap();
        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        let mut running =
            Running::spawn(&mut tail(&scratch.0, polling, "-f", Some(&path)), true).unwrap();
        running.expect_stdout(b"ready\n");
        fs::rename(&path, scratch.0.join("rotated")).unwrap();
        fs::write(&path, b"WRONG REPLACEMENT INODE\n").unwrap();
        writer.write_all(b"held\n").unwrap();
        running.expect_stdout(b"ready\nheld\n");
        fs::write(&path, b"x\n").unwrap();
        writer.write_all(b"still held\n").unwrap();
        running.expect_stdout(b"ready\nheld\nstill held\n");
        let replacement = scratch.0.join("replacement");
        fs::write(&replacement, b"WRONG SYMLINK REPLACEMENT\n").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&replacement, &path).unwrap();
        writer.write_all(b"same inode\n").unwrap();
        running.expect_stdout(b"ready\nheld\nstill held\nsame inode\n");
        writer.set_len(0).unwrap();
        writer.write_all(b"t\n").unwrap();
        running.expect_stdout(b"ready\nheld\nstill held\nsame inode\nt\n");
        running.expect_quiet(b"ready\nheld\nstill held\nsame inode\nt\n");
    }
}

#[test]
fn name_follow_tracks_symlink_retargets_and_retry_recovers_dangling_targets() {
    for polling in [false, true] {
        for follow in ["-F", "--follow=name"] {
            let scratch = Scratch::new("name-symlink");
            let first = scratch.0.join("log.2026-11-15");
            let next = scratch.0.join(OsStr::from_bytes(b"log.2026-11-16-\xff"));
            let link = scratch.0.join("log");
            fs::write(&first, b"ready\n").unwrap();
            fs::write(&next, b"next\n").unwrap();
            symlink(first.file_name().unwrap(), &link).unwrap();
            let mut running =
                Running::spawn(&mut tail(&scratch.0, polling, follow, Some(&link)), true).unwrap();
            running.expect_stdout(b"ready\n");
            replace_symlink(&scratch.0, &link, Path::new(next.file_name().unwrap()));
            running.expect_stdout(b"ready\nnext\n");
            append(&first, b"WRONG OLD TARGET\n");
            append(&next, b"new append\n");
            running.expect_stdout(b"ready\nnext\nnew append\n");
            if follow == "--follow=name" {
                fs::remove_file(&link).unwrap();
                assert_eq!(running.wait(WAIT).code(), Some(1));
                assert_eq!(running.out, b"ready\nnext\nnew append\n");
                assert!(!running.err.is_empty());
                continue;
            }
            let missing = scratch.0.join("log.2026-11-17");
            let diagnostic_start = running.err.len();
            replace_symlink(&scratch.0, &link, Path::new(missing.file_name().unwrap()));
            running.expect_stderr_since(diagnostic_start, b"No such file or directory");
            fs::write(&missing, b"appeared\n").unwrap();
            running.expect_stdout(b"ready\nnext\nnew append\nappeared\n");
            append(&missing, b"late\n");
            running.expect_stdout(b"ready\nnext\nnew append\nappeared\nlate\n");
            let diagnostic_start = running.err.len();
            fs::remove_file(&missing).unwrap();
            running.expect_stderr_since(diagnostic_start, b"No such file or directory");
            fs::write(&missing, b"reappeared\n").unwrap();
            running.expect_stdout(b"ready\nnext\nnew append\nappeared\nlate\nreappeared\n");
            running.expect_quiet(b"ready\nnext\nnew append\nappeared\nlate\nreappeared\n");
        }
    }
}

#[test]
fn retry_name_follow_waits_for_an_initially_dangling_symlink() {
    for polling in [false, true] {
        let scratch = Scratch::new("initial-dangling");
        let target = scratch.0.join("dated-log");
        let link = scratch.0.join("log");
        symlink(target.file_name().unwrap(), &link).unwrap();
        let mut running =
            Running::spawn(&mut tail(&scratch.0, polling, "-F", Some(&link)), true).unwrap();
        running.expect_stderr_since(0, b"No such file or directory");
        assert!(running.out.is_empty());
        fs::write(&target, b"appeared\n").unwrap();
        running.expect_stdout(b"appeared\n");
        append(&target, b"append\n");
        running.expect_stdout(b"appeared\nappend\n");
        running.expect_quiet(b"appeared\nappend\n");
    }
}

#[test]
fn name_follow_without_retry_reports_an_initially_missing_path() {
    for polling in [false, true] {
        let scratch = Scratch::new("missing");
        let path = scratch.0.join("missing");
        let mut running = Running::spawn(
            &mut tail(&scratch.0, polling, "--follow=name", Some(&path)),
            true,
        )
        .unwrap();
        assert_eq!(running.wait(WAIT).code(), Some(1));
        assert!(running.out.is_empty());
        assert!(
            running
                .err
                .windows(b"No such file or directory".len())
                .any(|part| part == b"No such file or directory")
        );
    }
}

#[test]
fn follow_exits_successfully_when_the_requested_pid_exits() {
    for polling in [false, true] {
        let scratch = Scratch::new("pid");
        let path = scratch.0.join("log");
        fs::write(&path, b"ready\n").unwrap();
        let mut producer = OwnedChild::spawn(applet(&scratch.0, "sleep").arg("30"), true).unwrap();
        let mut command = tail(&scratch.0, polling, "-f", Some(&path));
        command.arg(format!("--pid={}", producer.child.id()));
        let mut running = Running::spawn(&mut command, true).unwrap();
        running.expect_stdout(b"ready\n");
        append(&path, b"append\n");
        running.expect_stdout(b"ready\nappend\n");
        producer.terminate();
        assert_eq!(running.wait(WAIT).code(), Some(0));
        assert_eq!(running.out, b"ready\nappend\n");
    }
}

#[test]
fn named_fifo_follow_streams_bytes_and_preserves_pid_shutdown() {
    for polling in [false, true] {
        let scratch = Scratch::new("fifo");
        let path = scratch.0.join("fifo");
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: c_path is a live NUL-terminated path within the private fixture.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let mut writer = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let mut producer = OwnedChild::spawn(applet(&scratch.0, "sleep").arg("30"), true).unwrap();
        let mut command = tail(&scratch.0, polling, "-f", Some(&path));
        command.arg(format!("--pid={}", producer.child.id()));
        let mut running = Running::spawn(&mut command, true).unwrap();
        writer.write_all(b"ready\n").unwrap();
        running.expect_stdout(b"ready\n");
        writer.write_all(b"fifo append\n").unwrap();
        running.expect_stdout(b"ready\nfifo append\n");
        drop(writer);
        producer.terminate();
        assert_eq!(running.wait(WAIT).code(), Some(0));
        assert_eq!(running.out, b"ready\nfifo append\n");
    }
}

#[cfg(target_os = "linux")]
mod bind_mounts {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::os::unix::fs::MetadataExt;

    fn namespace_inode(path: &CStr) -> io::Result<u64> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: path is NUL-terminated and stat has storage for the whole result.
        if unsafe { libc::stat(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful stat initialized st_ino.
        Ok(unsafe { stat.assume_init().st_ino })
    }

    fn kernel_write(path: &CStr, bytes: &[u8]) -> io::Result<()> {
        // SAFETY: path is NUL-terminated; this call does not create files.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut offset = 0;
        let result = loop {
            if offset == bytes.len() {
                break Ok(());
            }
            // SAFETY: the remaining bytes are live and fd is exclusively owned here.
            let count =
                unsafe { libc::write(fd, bytes[offset..].as_ptr().cast(), bytes.len() - offset) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break Err(error);
            }
            if count == 0 {
                break Err(io::Error::from_raw_os_error(libc::EIO));
            }
            offset += count as usize;
        };
        // SAFETY: fd was opened here and is closed exactly once, including error paths.
        let closed = unsafe { libc::close(fd) };
        result?;
        if closed < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    struct Binding(CString);

    impl Binding {
        fn new(source: &Path, target: &Path) -> Self {
            let source = CString::new(source.as_os_str().as_bytes()).unwrap();
            let target = CString::new(target.as_os_str().as_bytes()).unwrap();
            // SAFETY: only the isolated helper calls this after namespace and propagation checks.
            assert_eq!(
                unsafe {
                    libc::mount(
                        source.as_ptr(),
                        target.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND,
                        std::ptr::null(),
                    )
                },
                0,
                "bind mount: {}",
                io::Error::last_os_error(),
            );
            Self(target)
        }
    }

    impl Drop for Binding {
        fn drop(&mut self) {
            // SAFETY: this mount exists only in the helper's verified private mount namespace.
            unsafe { libc::umount2(self.0.as_ptr(), libc::MNT_DETACH) };
        }
    }

    #[test]
    fn file_and_directory_bind_mounts_preserve_follow_contracts() {
        let scratch = Scratch::new("namespace");
        let old_user = fs::metadata("/proc/self/ns/user").unwrap().ino();
        let old_mount = fs::metadata("/proc/self/ns/mnt").unwrap().ino();
        // SAFETY: these syscalls only read the current process's credentials.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let uid_map = format!("0 {uid} 1\n").into_bytes();
        let gid_map = format!("0 {gid} 1\n").into_bytes();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "bind_mounts::namespace_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("AXE_TAIL_BIND_NAMESPACE", "1")
            .env("AXE_TAIL_PARENT_USER_NS", old_user.to_string())
            .env("AXE_TAIL_PARENT_MOUNT_NS", old_mount.to_string())
            .env("PATH", "/nonexistent")
            .env("HOME", &scratch.0)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: the fork-child hook uses only libc calls, preallocated buffers, and stack data.
        unsafe {
            command.pre_exec(move || {
                // SAFETY: namespace changes affect only the fork child; no mount is changed yet.
                if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) != 0 {
                    let error = io::Error::last_os_error();
                    return Err(match error.raw_os_error() {
                        Some(libc::EPERM | libc::EACCES | libc::EINVAL | libc::ENOSYS) => {
                            io::Error::from_raw_os_error(libc::ENOTSUP)
                        }
                        _ => error,
                    });
                }
                if namespace_inode(c"/proc/self/ns/user")? == old_user
                    || namespace_inode(c"/proc/self/ns/mnt")? == old_mount
                {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                match kernel_write(c"/proc/self/setgroups", b"deny\n") {
                    Ok(()) => {}
                    Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
                    Err(error) => return Err(error),
                }
                kernel_write(c"/proc/self/uid_map", &uid_map)?;
                kernel_write(c"/proc/self/gid_map", &gid_map)?;
                // SAFETY: both namespace identities differ; privatize propagation before any bind.
                if libc::mount(
                    std::ptr::null(),
                    c"/".as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut helper = match Running::spawn(&mut command, true) {
            Ok(helper) => helper,
            Err(error) if error.raw_os_error() == Some(libc::ENOTSUP) => {
                eprintln!(
                    "SKIP tail bind mounts: unprivileged user/mount namespaces are unavailable"
                );
                return;
            }
            Err(error) => panic!("start isolated tail bind-mount helper: {error}"),
        };
        assert_eq!(
            helper.wait(Duration::from_secs(30)).code(),
            Some(0),
            "stdout {:?}; stderr {:?}",
            String::from_utf8_lossy(&helper.out),
            String::from_utf8_lossy(&helper.err)
        );
    }

    #[test]
    #[ignore = "run by file_and_directory_bind_mounts_preserve_follow_contracts in an isolated namespace"]
    fn namespace_child() {
        assert_eq!(
            std::env::var_os("AXE_TAIL_BIND_NAMESPACE").as_deref(),
            Some(OsStr::new("1"))
        );
        let parent_user: u64 = std::env::var("AXE_TAIL_PARENT_USER_NS")
            .unwrap()
            .parse()
            .unwrap();
        let parent_mount: u64 = std::env::var("AXE_TAIL_PARENT_MOUNT_NS")
            .unwrap()
            .parse()
            .unwrap();
        assert_ne!(
            fs::metadata("/proc/self/ns/user").unwrap().ino(),
            parent_user
        );
        assert_ne!(
            fs::metadata("/proc/self/ns/mnt").unwrap().ino(),
            parent_mount
        );
        for polling in [false, true] {
            for directory in [false, true] {
                let scratch = Scratch::new("bind");
                let source_dir = scratch.0.join("source");
                let target_dir = scratch.0.join("target");
                fs::create_dir(&source_dir).unwrap();
                fs::create_dir(&target_dir).unwrap();
                let source = source_dir.join("log");
                let target = target_dir.join("log");
                fs::write(&source, b"ready\n").unwrap();
                fs::write(&target, b"").unwrap();
                let _binding = if directory {
                    Binding::new(&source_dir, &target_dir)
                } else {
                    Binding::new(&source, &target)
                };
                let mut running =
                    Running::spawn(&mut tail(&scratch.0, polling, "-f", Some(&target)), false)
                        .unwrap();
                running.expect_stdout(b"ready\n");
                append(&source, b"source append\n");
                running.expect_stdout(b"ready\nsource append\n");
                append(&target, b"endpoint append\n");
                running.expect_stdout(b"ready\nsource append\nendpoint append\n");
                running.expect_quiet(b"ready\nsource append\nendpoint append\n");
            }
        }
        let scratch = Scratch::new("overmount");
        let source = scratch.0.join("source");
        let watched = scratch.0.join("watched");
        fs::write(&source, b"mounted\n").unwrap();
        fs::write(&watched, b"ready\n").unwrap();
        let mut running =
            Running::spawn(&mut tail(&scratch.0, true, "-F", Some(&watched)), false).unwrap();
        running.expect_stdout(b"ready\n");
        let _binding = Binding::new(&source, &watched);
        running.expect_stdout(b"ready\nmounted\n");
        append(&source, b"source append\n");
        running.expect_stdout(b"ready\nmounted\nsource append\n");
        append(&watched, b"endpoint append\n");
        running.expect_stdout(b"ready\nmounted\nsource append\nendpoint append\n");
        running.expect_quiet(b"ready\nmounted\nsource append\nendpoint append\n");
    }
}
