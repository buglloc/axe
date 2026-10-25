#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-admin-terminal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("create private scratch directory");
        Self(fs::canonicalize(path).unwrap())
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn applet(root: &Path, name: &str) -> Command {
    applet_from(root, name, OsStr::new(env!("CARGO_BIN_EXE_axe")))
}

fn applet_from(root: &Path, name: &str, program: &OsStr) -> Command {
    let mut command = Command::new(program);
    command
        .args(["--applet", name, "--"])
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_MODE", "off")
        .stdin(Stdio::null());
    command
}

fn umask(command: &mut Command, mask: libc::mode_t) -> &mut Command {
    // SAFETY: pre_exec only invokes the async-signal-safe umask syscall in the
    // fresh child; the multithreaded test process's umask is not changed.
    unsafe {
        command.pre_exec(move || {
            libc::umask(mask);
            Ok(())
        })
    }
}

fn input(command: &mut Command, bytes: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    child.wait_with_output().unwrap()
}

fn success(output: &Output) -> &[u8] {
    assert!(
        output.status.success(),
        "status {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");
    &output.stdout
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().mode() & 0o7777
}

fn filesystem_identity() -> (libc::uid_t, libc::gid_t) {
    // SAFETY: identity getters take no pointers and do not mutate process state.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    if uid == 0 { (65534, 65534) } else { (uid, gid) }
}

fn permission_test_binary(root: &Path) -> PathBuf {
    // A root-only checkout path must not prevent the dropped-UID child from
    // starting before the directory-permission behavior can be exercised.
    // SAFETY: geteuid only reads the current identity.
    if unsafe { libc::geteuid() } != 0 {
        return PathBuf::from(env!("CARGO_BIN_EXE_axe"));
    }
    assert_eq!(fs::metadata(root).unwrap().uid(), 0);
    let binary = root.join("axe-permission-test");
    fs::copy(env!("CARGO_BIN_EXE_axe"), &binary).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    binary
}

fn own_fixture(path: &Path, identity: (libc::uid_t, libc::gid_t)) {
    // SAFETY: geteuid has no pointer arguments or side effects.
    let uid = unsafe { libc::geteuid() };
    assert_eq!(fs::symlink_metadata(path).unwrap().uid(), uid);
    if uid == 0 {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: this is a NUL-terminated private fixture path whose ownership
        // was checked above, not a shared system object.
        assert_eq!(
            unsafe { libc::chown(path.as_ptr(), identity.0, identity.1) },
            0
        );
    }
}

fn unprivileged(command: &mut Command, identity: (libc::uid_t, libc::gid_t)) -> &mut Command {
    // SAFETY: geteuid only reads the calling process's effective identity.
    if unsafe { libc::geteuid() } == 0 {
        // SAFETY: only credential syscalls run after fork, in the command child.
        // The parent retains its identity; no host namespace or policy is changed.
        unsafe {
            command.pre_exec(move || {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(identity.1) != 0
                    || libc::setuid(identity.0) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
}

struct RestoreAccess(Vec<PathBuf>);
impl Drop for RestoreAccess {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        }
    }
}

#[test]
fn mkdir_persists_requested_set_id_modes_without_changing_parent_policy() {
    let scratch = Scratch::new();
    let root = scratch.path();
    for (name, requested, expected) in [
        ("setgid", "2750", 0o2750),
        ("setuid", "4750", 0o4750),
        ("symbolic", "u=rwx,g=rx,g+s", 0o2757),
        ("sticky", "1750", 0o1750),
        ("ordinary", "750", 0o750),
    ] {
        let mut command = applet(root, "mkdir");
        command.args(["-m", requested, name]);
        assert_eq!(success(&umask(&mut command, 0o077).output().unwrap()), b"");
        assert_eq!(mode(&root.join(name)), expected, "mode {requested}");
    }
    let mut command = applet(root, "mkdir");
    command.args(["-p", "-m", "2770", "deep/a"]);
    success(&umask(&mut command, 0o077).output().unwrap());
    assert_eq!(mode(&root.join("deep")), 0o700);
    assert_eq!(mode(&root.join("deep/a")), 0o2770);
    let mut command = applet(root, "mkdir");
    command.arg("default");
    success(&umask(&mut command, 0o077).output().unwrap());
    assert_eq!(mode(&root.join("default")), 0o700);
}

#[test]
fn install_ancestors_are_0755_and_leaf_modes_are_independent() {
    let scratch = Scratch::new();
    let root = scratch.path();
    fs::write(root.join("source"), b"source\0bytes\n").unwrap();
    for mask in [0o077, 0o022, 0o000] {
        let base = format!("parents-{mask:o}");
        let destination = format!("{base}/a/file");
        let mut command = applet(root, "install");
        command.args(["-D", "-m", "640", "source", &destination]);
        success(&umask(&mut command, mask).output().unwrap());
        assert_eq!(mode(&root.join(&base)), 0o755);
        assert_eq!(mode(&root.join(&base).join("a")), 0o755);
        assert_eq!(mode(&root.join(&destination)), 0o640);
        assert_eq!(
            fs::read(root.join(&destination)).unwrap(),
            b"source\0bytes\n"
        );
    }
    let mut command = applet(root, "install");
    command.args(["-d", "-m", "700", "dirs/a", "dirs/b"]);
    success(&umask(&mut command, 0o077).output().unwrap());
    assert_eq!(mode(&root.join("dirs")), 0o755);
    for name in ["dirs/a", "dirs/b"] {
        assert_eq!(mode(&root.join(name)), 0o700);
    }
    fs::create_dir(root.join("existing")).unwrap();
    fs::set_permissions(root.join("existing"), fs::Permissions::from_mode(0o711)).unwrap();
    success(
        &applet(root, "install")
            .args(["-D", "source", "existing/child/file"])
            .output()
            .unwrap(),
    );
    assert_eq!(mode(&root.join("existing")), 0o711);
    assert_eq!(mode(&root.join("existing/child")), 0o755);
}

#[test]
fn mkdir_set_id_restoration_needs_no_permissions_on_the_created_directory() {
    let scratch = Scratch::new();
    let root = scratch.path();
    let binary = permission_test_binary(root);
    let identity = filesystem_identity();
    own_fixture(root, identity);
    let _access = RestoreAccess(vec![root.join("minimal"), root.join("deep/leaf")]);
    for (args, path, expected) in [
        (&["-m", "6000", "minimal"][..], "minimal", 0o6000),
        (&["-p", "-m", "6000", "deep/leaf"][..], "deep/leaf", 0o6000),
    ] {
        let mut command = applet_from(root, "mkdir", binary.as_os_str());
        command.args(args);
        umask(&mut command, 0o077);
        success(&unprivileged(&mut command, identity).output().unwrap());
        assert_eq!(mode(&root.join(path)), expected, "{args:?}");
        assert_eq!(fs::metadata(root.join(path)).unwrap().uid(), identity.0);
    }
    assert_eq!(mode(&root.join("deep")), 0o700);
}

#[test]
fn install_preserves_parent_components_in_directory_and_leading_paths() {
    let scratch = Scratch::new();
    let root = scratch.path();
    let binary = permission_test_binary(root);
    let identity = filesystem_identity();
    fs::write(root.join("source"), b"content").unwrap();
    fs::set_permissions(root.join("source"), fs::Permissions::from_mode(0o644)).unwrap();
    own_fixture(root, identity);
    let mut command = applet_from(root, "install", binary.as_os_str());
    command.args(["-d", "-m", "700", "a/../b", "c/d/../../e"]);
    umask(&mut command, 0o077);
    success(&unprivileged(&mut command, identity).output().unwrap());
    for name in ["a", "c", "c/d"] {
        assert_eq!(mode(&root.join(name)), 0o755);
    }
    for name in ["b", "e"] {
        assert_eq!(mode(&root.join(name)), 0o700);
    }
    assert!(!root.join("a/b").exists());
    let mut command = applet_from(root, "install", binary.as_os_str());
    command.args(["-D", "-m", "640", "source", "leading/../destination/file"]);
    umask(&mut command, 0o077);
    success(&unprivileged(&mut command, identity).output().unwrap());
    assert_eq!(mode(&root.join("leading")), 0o755);
    assert_eq!(mode(&root.join("destination")), 0o755);
    assert_eq!(mode(&root.join("destination/file")), 0o640);
    assert_eq!(fs::read(root.join("destination/file")).unwrap(), b"content");
    assert!(!root.join("leading/destination").exists());
}

#[test]
fn install_accepts_owned_write_search_only_ancestors_after_privilege_drop() {
    let scratch = Scratch::new();
    let root = scratch.path();
    let binary = permission_test_binary(root);
    let identity = filesystem_identity();
    fs::create_dir(root.join("parent")).unwrap();
    fs::write(root.join("source"), b"content").unwrap();
    fs::set_permissions(root.join("source"), fs::Permissions::from_mode(0o644)).unwrap();
    own_fixture(&root.join("parent"), identity);
    own_fixture(root, identity);
    fs::set_permissions(root.join("parent"), fs::Permissions::from_mode(0o300)).unwrap();
    let _access = RestoreAccess(vec![root.join("parent")]);
    let mut command = applet_from(root, "install", binary.as_os_str());
    command.args(["-d", "-m", "700", "parent/child"]);
    umask(&mut command, 0o077);
    success(&unprivileged(&mut command, identity).output().unwrap());
    assert_eq!(mode(&root.join("parent")), 0o300);
    assert_eq!(mode(&root.join("parent/child")), 0o700);
    let mut command = applet_from(root, "install", binary.as_os_str());
    command.args(["-D", "-m", "640", "source", "parent/leading/file"]);
    umask(&mut command, 0o077);
    success(&unprivileged(&mut command, identity).output().unwrap());
    assert_eq!(mode(&root.join("parent")), 0o300);
    assert_eq!(mode(&root.join("parent/leading")), 0o755);
    assert_eq!(mode(&root.join("parent/leading/file")), 0o640);
    assert_eq!(
        fs::read(root.join("parent/leading/file")).unwrap(),
        b"content"
    );
}

#[test]
fn shred_remove_truncates_the_open_inode_seen_by_surviving_hardlinks() {
    let scratch = Scratch::new();
    let root = scratch.path();
    for flags in [
        &["-n", "0", "-u"][..],
        &["-n", "0", "-z", "-x", "-u"],
        &["-n", "0", "--remove=unlink"],
    ] {
        fs::write(root.join("file"), b"alpha\0beta\n").unwrap();
        fs::hard_link(root.join("file"), root.join("hard")).unwrap();
        let inode = fs::metadata(root.join("hard")).unwrap().ino();
        success(
            &applet(root, "shred")
                .args(flags)
                .arg("file")
                .output()
                .unwrap(),
        );
        assert!(!root.join("file").exists());
        assert_eq!(fs::metadata(root.join("hard")).unwrap().ino(), inode);
        assert_eq!(fs::read(root.join("hard")).unwrap(), b"");
        fs::remove_file(root.join("hard")).unwrap();
    }
}

#[test]
fn byte_paths_reach_fifo_creation_and_stat_output_without_lossy_conversion() {
    let scratch = Scratch::new();
    let root = scratch.path();
    for (name, app) in [
        (b"fifo-\xff".as_slice(), "mkfifo"),
        (b"node-\xff".as_slice(), "mknod"),
    ] {
        let name = OsStr::from_bytes(name);
        let mut command = applet(root, app);
        command.args(["-m", "600"]).arg(name);
        if app == "mknod" {
            command.arg("p");
        }
        success(&command.output().unwrap());
        let metadata = fs::symlink_metadata(root.join(name)).unwrap();
        assert!(metadata.file_type().is_fifo());
        assert_eq!(metadata.mode() & 0o777, 0o600);
    }
    let name = OsStr::from_bytes(b"raw-\xff");
    fs::write(root.join(name), b"data").unwrap();
    assert_eq!(
        success(
            &applet(root, "stat")
                .args(["-c", "%n|%s"])
                .arg(name)
                .output()
                .unwrap()
        ),
        b"raw-\xff|4\n"
    );
    assert_eq!(
        success(
            &applet(root, "stat")
                .args(["-c", "%10.5n"])
                .arg(name)
                .output()
                .unwrap()
        ),
        b"     raw-\xff\n"
    );
    assert_eq!(
        success(
            &applet(root, "stat")
                .args(["-c", "%N"])
                .arg(name)
                .output()
                .unwrap()
        ),
        b"'raw-'$'\\377'\n"
    );
    std::os::unix::fs::symlink("target", root.join("link")).unwrap();
    assert_eq!(
        success(
            &applet(root, "stat")
                .args(["-c", "%N", "link"])
                .output()
                .unwrap()
        ),
        b"link -> target\n"
    );
    assert_eq!(
        success(
            &applet(root, "stat")
                .env("QUOTING_STYLE", "shell-escape-always")
                .args(["-c", "%N", "link"])
                .output()
                .unwrap()
        ),
        b"'link' -> 'target'\n"
    );
}

#[test]
fn listing_control_defaults_preserve_invalid_bytes_but_explicit_q_hides_them() {
    let scratch = Scratch::new();
    let root = scratch.path();
    fs::create_dir(root.join("names")).unwrap();
    for name in [
        b"line\nbreak".as_slice(),
        b"mixed-\xff\nbreak".as_slice(),
        b"raw-\xff".as_slice(),
    ] {
        fs::write(root.join("names").join(OsStr::from_bytes(name)), b"").unwrap();
    }
    let default = b"line?break\nmixed-\xff?break\nraw-\xff\n";
    let hidden = b"line?break\nmixed-??break\nraw-?\n";
    let raw = b"line\nbreak\nmixed-\xff\nbreak\nraw-\xff\n";
    let nul = b"line\nbreak\0mixed-\xff\nbreak\0raw-\xff\0";
    let hidden_nul = b"line?break\0mixed-??break\0raw-?\0";
    for app in ["ls", "dir", "vdir"] {
        for (flags, expected) in [
            (&[][..], default.as_slice()),
            (&["-q"][..], hidden.as_slice()),
            (&["--show-control-chars"][..], raw.as_slice()),
            (&["-q", "--show-control-chars"][..], raw.as_slice()),
            (&["--show-control-chars", "-q"][..], hidden.as_slice()),
            (&["--zero"][..], nul.as_slice()),
            (&["-q", "--zero"][..], nul.as_slice()),
            (&["--zero", "-q"][..], hidden_nul.as_slice()),
            (&["--zero", "--quoting-style=literal"][..], nul.as_slice()),
        ] {
            let output = applet(root, app)
                .args(["-1", "--quoting-style=literal", "names"])
                .args(flags)
                .output()
                .unwrap();
            assert_eq!(success(&output), expected, "{app} {flags:?}");
        }
        for (operand, default, hidden, raw, nul) in [
            (
                b"names/raw-\xff".as_slice(),
                b"names/raw-\xff\n".as_slice(),
                b"names/raw-?\n".as_slice(),
                b"names/raw-\xff\n".as_slice(),
                b"names/raw-\xff\0".as_slice(),
            ),
            (
                b"names/mixed-\xff\nbreak".as_slice(),
                b"names/mixed-\xff?break\n".as_slice(),
                b"names/mixed-??break\n".as_slice(),
                b"names/mixed-\xff\nbreak\n".as_slice(),
                b"names/mixed-\xff\nbreak\0".as_slice(),
            ),
        ] {
            for (flags, expected) in [
                (&[][..], default),
                (&["-q"][..], hidden),
                (&["--show-control-chars"][..], raw),
                (&["--zero"][..], nul),
            ] {
                let output = applet(root, app)
                    .args(["-1", "--quoting-style=literal"])
                    .args(flags)
                    .arg(OsStr::from_bytes(operand))
                    .output()
                    .unwrap();
                assert_eq!(success(&output), expected, "{app} {flags:?} {operand:?}");
            }
        }
    }
}

#[test]
fn more_without_a_controlling_terminal_is_byte_preserving_and_has_no_pager_escapes() {
    let scratch = Scratch::new();
    let root = scratch.path();
    let bytes = b"a\n\n\nb\0\xff\n";
    for flags in [&[][..], &["-s"], &["-n", "2"]] {
        let mut command = applet(root, "more");
        command.args(flags);
        // SAFETY: setsid has no borrowed state and runs only in the new child.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        assert_eq!(success(&input(&mut command, bytes)), bytes);
    }
    fs::write(root.join("input"), bytes).unwrap();
    let output = applet(root, "more")
        .args(["-n", "2", "input"])
        .output()
        .unwrap();
    let expected = [b"::::::::::::::\ninput\n::::::::::::::\n".as_slice(), bytes].concat();
    assert_eq!(success(&output), expected);
    let mut shell = Command::new(env!("CARGO_BIN_EXE_axe"));
    shell
        .current_dir(root)
        .env("AXE_STORE_MODE", "off")
        .env("LC_ALL", "C")
        .args(["-c", "printf 'a\\nb\\n' | more"]);
    assert_eq!(success(&shell.output().unwrap()), b"a\nb\n");
}

#[test]
fn printenv_accepts_byte_environment_names_and_distinguishes_missing_names() {
    let scratch = Scratch::new();
    let root = scratch.path();
    let name = OsString::from_vec(b"KEY\xff".to_vec());
    let value = OsString::from_vec(b"value\xff".to_vec());
    assert_eq!(
        success(
            &applet(root, "printenv")
                .env(&name, &value)
                .arg(&name)
                .output()
                .unwrap()
        ),
        b"value\xff\n"
    );
    assert_eq!(
        success(
            &applet(root, "printenv")
                .env(&name, &value)
                .arg("-0")
                .arg(&name)
                .output()
                .unwrap()
        ),
        b"value\xff\0"
    );
    let output = applet(root, "printenv").arg("MISSING").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"");
}

#[cfg(target_os = "linux")]
#[test]
fn stdout_io_errors_have_distinct_utility_exit_classes() {
    let scratch = Scratch::new();
    let root = scratch.path();
    for (app, args, bytes, code) in [
        ("sort", &[][..], b"b\na\n".as_slice(), 2),
        ("sort", &["-m"][..], b"a\nb\n".as_slice(), 2),
        ("sort", &["-m", "-u", "-z"][..], b"a\0a\0b\0".as_slice(), 2),
        (
            "sort",
            &["-m", "-o", "/dev/full"][..],
            b"a\nb\n".as_slice(),
            2,
        ),
        ("printenv", &["HOME"][..], b"".as_slice(), 2),
        ("printenv", &[][..], b"".as_slice(), 2),
        ("env", &[][..], b"".as_slice(), 125),
    ] {
        let mut command = applet(root, app);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(File::options().write(true).open("/dev/full").unwrap())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{app} {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stderr.is_empty());
    }
    assert_eq!(
        input(applet(root, "sort").arg("-c"), b"b\na\n")
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        applet(root, "sort")
            .args(["-c", "missing"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        applet(root, "sort")
            .arg("-c")
            .arg(root)
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        applet(root, "env")
            .args([env!("CARGO_BIN_EXE_axe"), "--applet", "false", "--"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        applet(root, "env")
            .arg("/nonexistent/command")
            .output()
            .unwrap()
            .status
            .code(),
        Some(127)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn sort_merge_flushes_final_output_after_intermediate_batches() {
    let scratch = Scratch::new();
    let root = scratch.path();
    for (name, bytes) in [("a", b"a\n"), ("b", b"b\n"), ("c", b"c\n")] {
        fs::write(root.join(name), bytes).unwrap();
    }
    let output = applet(root, "sort")
        .args([
            "-m",
            "--batch-size=2",
            "-T",
            ".",
            "-o",
            "/dev/full",
            "a",
            "b",
            "c",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stderr.is_empty());
    for (name, expected) in [("a", b"a\n"), ("b", b"b\n"), ("c", b"c\n")] {
        assert_eq!(fs::read(root.join(name)).unwrap(), expected);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn sort_temporary_write_failures_return_fatal_status_instead_of_truncated_success() {
    let scratch = Scratch::new();
    let root = scratch.path();
    fs::create_dir(root.join("temporary")).unwrap();
    let bytes = b"b\na\n".repeat(4096);
    fs::write(root.join("input"), &bytes).unwrap();
    let mut command = applet(root, "sort");
    command
        .args(["-S", "1K", "-T", "temporary", "input"])
        .stdout(Stdio::null());
    // SAFETY: these changes apply only to the spawned child. Ignoring SIGXFSZ
    // turns its private temporary-file limit into a real EFBIG write error;
    // neither the test runner's limits nor any host filesystem is changed.
    unsafe {
        command.pre_exec(|| {
            if libc::signal(libc::SIGXFSZ, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: 512,
                rlim_max: 512,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stderr.is_empty());
    assert_eq!(fs::read(root.join("input")).unwrap(), bytes);
    assert_eq!(fs::read_dir(root.join("temporary")).unwrap().count(), 0);
}

#[test]
fn printf_uses_binary_long_double_conversion_and_complete_float_formatting() {
    let scratch = Scratch::new();
    let root = scratch.path();
    for (format, value, expected) in [
        ("%.2f", "1.255", "1.25"),
        ("%.2f", "2.675", "2.67"),
        ("%.0f", "2.5", "2"),
        ("%.0f", "3.5", "4"),
        ("%+.3e", "-0", "-0.000e+00"),
        ("%.4g", "12345.125", "1.235e+04"),
        ("%#.4G", "0.00125", "0.001250"),
        ("%010.2f", "-1.25", "-000001.25"),
        ("%#.0f", "2", "2."),
        ("%E", "infinity", "INF"),
        ("%g", "-nan", "-nan"),
        ("%a", "0", "0x0p+0"),
        ("%f", "'A", "65.000000"),
    ] {
        assert_eq!(
            success(
                &applet(root, "printf")
                    .args([format, value])
                    .output()
                    .unwrap()
            ),
            expected.as_bytes(),
            "{format} {value}"
        );
    }
    for modifier in ["", "l", "L", "hh", "ll", "j", "z", "t"] {
        let format = format!("%.2{modifier}f");
        assert_eq!(
            success(
                &applet(root, "printf")
                    .args([format.as_str(), "1.255"])
                    .output()
                    .unwrap()
            ),
            b"1.25"
        );
    }
    assert_eq!(
        success(
            &applet(root, "printf")
                .args(["%s|%*.*f|%d", "left", "-8", "2", "1.255", "7"])
                .output()
                .unwrap()
        ),
        b"left|1.25    |7"
    );
    assert_eq!(
        success(
            &applet(root, "printf")
                .args(["%3$*1$.*2$f", "8", "2", "1.255"])
                .output()
                .unwrap()
        ),
        b"    1.25"
    );
    assert_eq!(
        success(
            &applet(root, "printf")
                .args([
                    "%2$f/%1$s/%3$*4$.*5$g",
                    "tag",
                    "1.255",
                    "12345.125",
                    "12",
                    "4"
                ])
                .output()
                .unwrap()
        ),
        b"1.255000/tag/   1.235e+04"
    );
    assert_eq!(
        success(
            &applet(root, "printf")
                .args(["%%\\x25\\045%f", "1.5"])
                .output()
                .unwrap()
        ),
        b"%%%1.500000"
    );
    assert_eq!(
        success(
            &applet(root, "printf")
                .args(["%f\\c%f", "1.5", "2.5"])
                .output()
                .unwrap()
        ),
        b"1.500000"
    );
    assert_eq!(
        success(&applet(root, "printf").args(["%3$f", "1"]).output().unwrap()),
        b"0.000000"
    );
    assert_eq!(
        success(
            &applet(root, "printf")
                .args(["%.2f,", "1.255", "2.675"])
                .output()
                .unwrap()
        ),
        b"1.25,2.67,"
    );
    assert_eq!(
        success(&applet(root, "printf").arg("%f").output().unwrap()),
        b"0.000000"
    );
    let empty = applet(root, "printf").args(["%f", ""]).output().unwrap();
    assert_eq!(empty.status.code(), Some(1));
    assert_eq!(empty.stdout, b"0.000000");
    let partial = applet(root, "printf")
        .args(["%.2f", "1.25tail"])
        .output()
        .unwrap();
    assert_eq!(partial.status.code(), Some(1));
    assert_eq!(partial.stdout, b"1.25");
    for value in ["1e99999", "1e-99999"] {
        assert_eq!(
            applet(root, "printf")
                .args(["%g", value])
                .output()
                .unwrap()
                .status
                .code(),
            Some(1)
        );
    }
    let large = format!("1.5{}", "0".repeat(299));
    assert_eq!(
        success(
            &applet(root, "printf")
                .args(["%.300f", "1.5"])
                .output()
                .unwrap()
        ),
        large.as_bytes()
    );
}

#[test]
fn printf_overflowing_positions_never_panic_or_disrupt_other_operands() {
    let scratch = Scratch::new();
    let root = scratch.path();
    let position = "9".repeat(100);
    for (conversion, expected) in [
        ("s", "".as_bytes()),
        ("d", b"0".as_slice()),
        ("f", b"0.000000".as_slice()),
    ] {
        let format = format!("%{position}${conversion}");
        assert_eq!(
            success(
                &applet(root, "printf")
                    .args([format.as_str(), "7"])
                    .output()
                    .unwrap()
            ),
            expected
        );
    }
    let format = format!("[%{position}$s][%d]");
    assert_eq!(
        success(
            &applet(root, "printf")
                .args([format.as_str(), "7"])
                .output()
                .unwrap()
        ),
        b"[][7]"
    );
    let output = applet(root, "printf")
        .args([format!("%*{position}$f"), "1.5".to_owned()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty());
}

#[test]
fn printf_shift_jis_character_constants_keep_the_single_byte_case() {
    let Some(locale) = std::env::var_os("AXE_TEST_SHIFT_JIS_LOCALE") else {
        eprintln!("SKIP: set AXE_TEST_SHIFT_JIS_LOCALE to an installed Shift-JIS locale");
        return;
    };
    let scratch = Scratch::new();
    for (operand, expected) in [
        (b"'\xa6".as_slice(), b"166".as_slice()),
        (b"'\xa6Z".as_slice(), b"65382".as_slice()),
    ] {
        let output = applet(scratch.path(), "printf")
            .env("LC_ALL", &locale)
            .envs(
                ["LOCPATH", "LOCALE_ARCHIVE"]
                    .into_iter()
                    .filter_map(|name| std::env::var_os(name).map(|value| (name, value))),
            )
            .env("POSIXLY_CORRECT", "1")
            .arg("%.0f")
            .arg(OsStr::from_bytes(operand))
            .output()
            .unwrap();
        assert_eq!(success(&output), expected);
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn printf_x87_boundaries_and_gnu_hex_normalization_do_not_depend_on_libc() {
    let scratch = Scratch::new();
    let root = scratch.path();
    for (format, value, expected) in [
        ("%a", "1.5", "0xcp-3"),
        ("%A", "1.5", "0XCP-3"),
        ("%.0a", "0x1.fp+3", "0x1p+4"),
        ("%a", "0x1p-16382", "0x8p-16385"),
        ("%a", "0x1p-16445", "0x0.000000000000001p-16385"),
        ("%.0f", "9007199254740993", "9007199254740993"),
        ("%.20g", "1.0000000000000000001", "1.0000000000000000001"),
    ] {
        assert_eq!(
            success(
                &applet(root, "printf")
                    .args([format, value])
                    .output()
                    .unwrap()
            ),
            expected.as_bytes(),
            "{format} {value}"
        );
    }
}

#[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
#[test]
fn printf_darwin_long_double_uses_the_arm64_c_abi() {
    let scratch = Scratch::new();
    let root = scratch.path();
    assert_eq!(
        success(&applet(root, "printf").args(["%a", "1.5"]).output().unwrap()),
        b"0x1.8p+0"
    );
    assert_eq!(
        success(
            &applet(root, "printf")
                .args(["%.0Lf", "9007199254740993"])
                .output()
                .unwrap()
        ),
        b"9007199254740992"
    );
}

struct Pty {
    master: File,
    slave: File,
}
impl Pty {
    fn new() -> Self {
        let mut master = -1;
        let mut slave = -1;
        let size = libc::winsize {
            ws_row: 10,
            ws_col: 60,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initializes two fresh descriptors; all pointers are
        // valid and the optional name/termios outputs are null.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            },
            0
        );
        // SAFETY: each successful openpty descriptor is transferred exactly once.
        Self {
            master: unsafe { File::from_raw_fd(master) },
            slave: unsafe { File::from_raw_fd(slave) },
        }
    }
}

fn stty_command(root: &Path, pty: &Pty) -> Command {
    let program = std::env::var_os("AXE_TEST_BINARY")
        .unwrap_or_else(|| OsString::from(env!("CARGO_BIN_EXE_axe")));
    let mut command = applet_from(root, "stty", &program);
    command.stdin(pty.slave.try_clone().unwrap());
    command
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "powerpc", target_arch = "powerpc64"))
))]
mod linux_stty_tests {
    use super::*;

    fn reference_stty(root: &Path, pty: &Pty) -> Command {
        let paths = std::env::var_os("AXE_REFERENCE_PATH")
            .expect("AXE_REFERENCE_PATH is set in nix develop .#default");
        let program = std::env::split_paths(&paths)
            .map(|directory| directory.join("stty"))
            .find(|candidate| candidate.is_file())
            .expect("GNU stty is present in AXE_REFERENCE_PATH");
        let mut command = Command::new(program);
        command
            .current_dir(root)
            .env_clear()
            .env("HOME", root)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .env("PATH", "/nonexistent")
            .stdin(pty.slave.try_clone().unwrap());
        command
    }

    fn terminal_state(pty: &Pty) -> libc::termios2 {
        // SAFETY: termios2 consists solely of integers and integer arrays.
        let mut state = unsafe { std::mem::zeroed::<libc::termios2>() };
        // SAFETY: the live PTY descriptor and writable storage match TCGETS2's ABI.
        assert_eq!(
            unsafe { libc::ioctl(pty.slave.as_raw_fd(), libc::TCGETS2 as _, &mut state) },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        state
    }

    fn set_terminal_state(pty: &Pty, state: &libc::termios2) {
        // SAFETY: this changes only the private PTY, using initialized TCSETS2 storage.
        assert_eq!(
            unsafe { libc::ioctl(pty.slave.as_raw_fd(), libc::TCSETS2 as _, state) },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
    }

    fn assert_terminal_state(actual: &libc::termios2, expected: &libc::termios2) {
        assert_eq!(
            (
                actual.c_iflag,
                actual.c_oflag,
                actual.c_cflag,
                actual.c_lflag,
                actual.c_line,
                actual.c_cc,
                actual.c_ispeed,
                actual.c_ospeed
            ),
            (
                expected.c_iflag,
                expected.c_oflag,
                expected.c_cflag,
                expected.c_lflag,
                expected.c_line,
                expected.c_cc,
                expected.c_ispeed,
                expected.c_ospeed
            )
        );
    }

    fn configured_pty(
        input_code: libc::tcflag_t,
        output_code: libc::tcflag_t,
        input_speed: u32,
        output_speed: u32,
    ) -> (Pty, libc::termios2) {
        let pty = Pty::new();
        let mut state = terminal_state(&pty);
        state.c_iflag ^= libc::IXON | libc::IXOFF | libc::ICRNL | libc::ISTRIP;
        state.c_oflag ^= libc::OPOST | libc::OCRNL | libc::ONLCR;
        state.c_lflag ^= libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN;
        state.c_cflag ^= libc::CLOCAL | libc::HUPCL | libc::CSTOPB;
        state.c_cflag &= !(libc::CBAUD | libc::CIBAUD);
        state.c_cflag |= output_code;
        if input_speed != output_speed {
            state.c_cflag |= input_code << libc::IBSHIFT;
        }
        for (index, value) in [
            (libc::VINTR, 0x1d),
            (libc::VERASE, 0x08),
            (libc::VMIN, 7),
            (libc::VTIME, 3),
            (libc::VEOL, 0x8f),
            (libc::VEOL2, 0xff),
            (libc::VLNEXT, 0xfe),
        ] {
            state.c_cc[index] = value;
        }
        state.c_ispeed = input_speed;
        state.c_ospeed = output_speed;
        set_terminal_state(&pty, &state);
        let actual = terminal_state(&pty);
        assert_terminal_state(&actual, &state);
        (pty, actual)
    }

    fn saved_state(command: &mut Command) -> String {
        let output = command.arg("-g").output().unwrap();
        std::str::from_utf8(success(&output))
            .unwrap()
            .trim_end_matches('\n')
            .to_owned()
    }

    #[test]
    fn stty_saved_state_is_stable_and_matches_gnu_on_a_fresh_pty() {
        let scratch = Scratch::new();
        let pty = Pty::new();
        let before = terminal_state(&pty);
        let expected = saved_state(&mut reference_stty(scratch.path(), &pty));
        for _ in 0..12 {
            assert_eq!(
                saved_state(&mut stty_command(scratch.path(), &pty)),
                expected
            );
            assert_terminal_state(&terminal_state(&pty), &before);
        }
    }

    #[test]
    fn stty_saved_state_restores_flags_control_chars_and_independent_speeds() {
        let scratch = Scratch::new();
        for (input_code, output_code, input_speed, output_speed) in [
            (libc::B38400, libc::B38400, 38400, 38400),
            (libc::B9600, libc::B19200, 9600, 19200),
            (libc::B115200, libc::B57600, 115200, 57600),
            (libc::BOTHER, libc::BOTHER, 123457, 234569),
            (libc::BOTHER, libc::B9600, 123457, 9600),
            (libc::B19200, libc::BOTHER, 19200, 234569),
        ] {
            let (pty, before) = configured_pty(input_code, output_code, input_speed, output_speed);
            let actual = saved_state(&mut stty_command(scratch.path(), &pty));
            let arbitrary = input_code == libc::BOTHER || output_code == libc::BOTHER;
            // Some GNU builds omit numeric BOTHER rates; use the kernel values.
            if !arbitrary {
                assert_eq!(
                    actual,
                    saved_state(&mut reference_stty(scratch.path(), &pty))
                );
            }
            let mut expected_fields = vec![
                before.c_iflag,
                before.c_oflag,
                (before.c_cflag & !(libc::CBAUD | libc::CIBAUD))
                    | output_code
                    | (input_code << libc::IBSHIFT),
                before.c_lflag,
            ];
            expected_fields.extend(before.c_cc.into_iter().map(u32::from));
            expected_fields.resize(4 + libc::NCCS, 0);
            if arbitrary {
                expected_fields.extend([input_speed, output_speed]);
            }
            let fields: Vec<u32> = actual
                .split(':')
                .map(|field| u32::from_str_radix(field, 16).unwrap())
                .collect();
            assert_eq!(fields, expected_fields);
            assert_terminal_state(&terminal_state(&pty), &before);

            let changes = [
                "raw", "4800", "intr", "^A", "erase", "^?", "min", "1", "time", "0",
            ];
            success(
                &stty_command(scratch.path(), &pty)
                    .args(changes)
                    .output()
                    .unwrap(),
            );
            assert_eq!(
                (terminal_state(&pty).c_ispeed, terminal_state(&pty).c_ospeed),
                (4800, 4800)
            );
            success(
                &stty_command(scratch.path(), &pty)
                    .arg(&actual)
                    .output()
                    .unwrap(),
            );
            assert_terminal_state(&terminal_state(&pty), &before);
            let expected_speed = if input_speed == output_speed {
                format!("{output_speed}\n")
            } else {
                format!("{input_speed} {output_speed}\n")
            };
            assert_eq!(
                success(
                    &stty_command(scratch.path(), &pty)
                        .arg("speed")
                        .output()
                        .unwrap()
                ),
                expected_speed.as_bytes()
            );
        }
    }

    #[test]
    fn stty_speed_setters_preserve_the_other_direction_and_unrelated_settings() {
        let scratch = Scratch::new();
        let (pty, mut expected) = configured_pty(libc::BOTHER, libc::BOTHER, 123457, 234569);
        assert_eq!(
            success(
                &stty_command(scratch.path(), &pty)
                    .args(["ospeed", "9600", "speed"])
                    .output()
                    .unwrap()
            ),
            b"123457 9600\n"
        );
        expected.c_cflag = (expected.c_cflag & !libc::CBAUD) | libc::B9600;
        expected.c_ospeed = 9600;
        assert_terminal_state(&terminal_state(&pty), &expected);

        assert_eq!(
            success(
                &stty_command(scratch.path(), &pty)
                    .args(["ispeed", "19200", "speed"])
                    .output()
                    .unwrap()
            ),
            b"19200 9600\n"
        );
        expected.c_cflag = (expected.c_cflag & !libc::CIBAUD) | (libc::B19200 << libc::IBSHIFT);
        expected.c_ispeed = 19200;
        assert_terminal_state(&terminal_state(&pty), &expected);

        assert_eq!(
            success(
                &stty_command(scratch.path(), &pty)
                    .args(["ispeed", "0", "speed"])
                    .output()
                    .unwrap()
            ),
            b"9600\n"
        );
        expected.c_cflag &= !libc::CIBAUD;
        expected.c_ispeed = 9600;
        assert_terminal_state(&terminal_state(&pty), &expected);
    }
}

#[test]
fn stty_speed_reads_a_private_pty_without_changing_its_settings() {
    let scratch = Scratch::new();
    let pty = Pty::new();
    // SAFETY: termios contains only C integer/array fields and tcgetattr fills it.
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: pty.slave is a live terminal and settings is writable termios storage.
    assert_eq!(
        unsafe { libc::tcgetattr(pty.slave.as_raw_fd(), &mut settings) },
        0
    );
    // SAFETY: these calls change only the local termios value and the newly allocated PTY.
    unsafe {
        assert_eq!(libc::cfsetispeed(&mut settings, libc::B38400), 0);
        assert_eq!(libc::cfsetospeed(&mut settings, libc::B38400), 0);
        assert_eq!(
            libc::tcsetattr(pty.slave.as_raw_fd(), libc::TCSANOW, &settings),
            0
        );
    }
    let output = stty_command(scratch.path(), &pty)
        .arg("speed")
        .output()
        .unwrap();
    assert_eq!(success(&output), b"38400\n");
    // SAFETY: tcgetattr fills another integer-only termios value for this private PTY.
    let mut after: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::tcgetattr(pty.slave.as_raw_fd(), &mut after) },
        0
    );
    assert_eq!(
        (
            after.c_iflag,
            after.c_oflag,
            after.c_cflag,
            after.c_lflag,
            after.c_cc
        ),
        (
            settings.c_iflag,
            settings.c_oflag,
            settings.c_cflag,
            settings.c_lflag,
            settings.c_cc
        )
    );
}

#[test]
fn stty_speed_observes_preceding_staged_baud_setters() {
    let scratch = Scratch::new();
    let pty = Pty::new();
    let output = stty_command(scratch.path(), &pty)
        .args(["38400", "speed", "9600", "speed", "19200", "speed"])
        .output()
        .unwrap();
    assert_eq!(success(&output), b"38400\n9600\n19200\n");
    let output = stty_command(scratch.path(), &pty)
        .arg("speed")
        .output()
        .unwrap();
    assert_eq!(success(&output), b"19200\n");
}

struct Running(Option<Child>);
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn more_keeps_the_real_tty_pager_and_accepts_quit() {
    let scratch = Scratch::new();
    let root = scratch.path();
    let contents: String = (0..100).map(|i| format!("row{i:03}\n")).collect();
    fs::write(root.join("pages"), contents).unwrap();
    let mut pty = Pty::new();
    let mut command = applet(root, "more");
    command
        .arg("pages")
        .env("TERM", "xterm")
        .stdin(pty.slave.try_clone().unwrap())
        .stdout(pty.slave.try_clone().unwrap())
        .stderr(Stdio::piped());
    // SAFETY: only async-signal-safe session/ioctl operations run in the child,
    // after stdin has been replaced by the test's freshly allocated PTY slave.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut running = Running(Some(command.spawn().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = Vec::new();
    let mut quit = false;
    loop {
        let mut poll = libc::pollfd {
            fd: pty.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll references exactly one initialized pollfd and waits at most 50ms.
        let ready = unsafe { libc::poll(&mut poll, 1, 50) };
        assert!(ready >= 0);
        if ready > 0 && poll.revents & libc::POLLIN != 0 {
            let mut bytes = [0; 4096];
            let read = pty.master.read(&mut bytes).unwrap();
            received.extend_from_slice(&bytes[..read]);
            if !quit && received.windows(6).any(|w| w == b"row008") {
                pty.master.write_all(b"q").unwrap();
                quit = true;
            }
        }
        if running.0.as_mut().unwrap().try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pager did not display/quit: {:?}",
            String::from_utf8_lossy(&received)
        );
    }
    let output = running.0.take().unwrap().wait_with_output().unwrap();
    success(&output);
    assert!(quit);
    assert!(received.windows(6).any(|w| w == b"row000"));
    assert!(!received.windows(6).any(|w| w == b"row099"));
}
