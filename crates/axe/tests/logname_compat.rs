#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const EXECUTABLE: &str = "AXE_TEST_BINARY";
const ENVIRONMENTS: [(Option<&str>, Option<&str>); 5] = [
    (Some("axe_fake_login"), None),
    (None, Some("axe_fake_user")),
    (Some("axe_other_login"), Some("axe_other_user")),
    (Some("effective-user"), Some("effective-user")),
    (Some(""), Some("")),
];

fn executable() -> PathBuf {
    std::env::var_os(EXECUTABLE)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_axe")))
}

fn logname(login: Option<&str>, user: Option<&str>) -> Output {
    let mut command = Command::new(executable());
    command
        .args(["--applet", "logname", "--"])
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("HOME", "/nonexistent")
        .env("LC_ALL", "C")
        .env("AXE_STORE_MODE", "off")
        .stdin(Stdio::null());
    if let Some(login) = login {
        command.env("LOGNAME", login);
    }
    if let Some(user) = user {
        command.env("USER", user);
    }
    command.output().expect("run logname")
}

#[test]
fn login_identity_is_independent_of_logname_and_user_environment() {
    let baseline = logname(None, None);
    assert!(
        matches!(baseline.status.code(), Some(0 | 1)),
        "{baseline:?}"
    );
    if baseline.status.success() {
        assert!(baseline.stdout.ends_with(b"\n"), "{baseline:?}");
    } else {
        assert!(baseline.stdout.is_empty(), "{baseline:?}");
    }
    for (login, user) in ENVIRONMENTS {
        let output = logname(login, user);
        assert_eq!(output.status.code(), baseline.status.code(), "{output:?}");
        assert_eq!(output.stdout, baseline.stdout, "{output:?}");
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-logname-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn file(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Mounts(Vec<PathBuf>);

impl Mounts {
    fn bind(&mut self, source: &Path, target: &Path) {
        let source = CString::new(source.as_os_str().as_bytes()).unwrap();
        let target_c = CString::new(target.as_os_str().as_bytes()).unwrap();
        // SAFETY: both C strings are live, and the caller has verified a new,
        // private mount namespace before the first mount mutation.
        let result = unsafe {
            libc::mount(
                source.as_ptr(),
                target_c.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        self.0.push(target.to_owned());
    }
}

impl Drop for Mounts {
    fn drop(&mut self) {
        for target in self.0.iter().rev() {
            let target = CString::new(target.as_os_str().as_bytes()).unwrap();
            // SAFETY: only this test's successfully recorded private mounts
            // are detached, before its scratch directory is removed.
            unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
        }
    }
}

fn unshare_program() -> PathBuf {
    std::env::var_os("AXE_REFERENCE_PATH")
        .into_iter()
        .chain(std::env::var_os("PATH"))
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join("unshare"))
        .find(|path| path.is_file())
        .expect("unshare must be available in AXE_REFERENCE_PATH or PATH")
}

fn assert_login(expected: &[u8]) {
    for (login, user) in ENVIRONMENTS {
        let output = logname(login, user);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, expected, "{output:?}");
    }
}

fn assert_no_login() {
    for (login, user) in ENVIRONMENTS {
        let output = logname(login, user);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
}

#[test]
#[ignore = "requires a Linux musl AXE_TEST_BINARY, unshare and private user/mount namespaces"]
fn private_musl_loginuid_preserves_identity_bytes_and_rejects_missing_logins() {
    const CHILD: &str = "AXE_LOGNAME_NAMESPACE_CHILD";
    const TEST: &str = "private_musl_loginuid_preserves_identity_bytes_and_rejects_missing_logins";
    let namespaces = ["user", "mnt"];
    if std::env::var_os(CHILD).is_none() {
        let mut command = Command::new(unshare_program());
        command
            .args(["--user", "--map-root-user", "--mount", "--fork", "--"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                TEST,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env(EXECUTABLE, executable().canonicalize().unwrap());
        for namespace in namespaces {
            command.env(
                format!("AXE_LOGNAME_PARENT_{namespace}"),
                fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
            );
        }
        let output = command.output().expect("run private logname fixture");
        assert!(output.status.success(), "{output:?}");
        return;
    }
    for namespace in namespaces {
        let parent =
            PathBuf::from(std::env::var_os(format!("AXE_LOGNAME_PARENT_{namespace}")).unwrap());
        assert_ne!(
            fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
            parent,
            "a new user and mount namespace is required before mounting"
        );
    }
    // SAFETY: geteuid has no arguments and only reads this process's identity.
    assert_eq!(unsafe { libc::geteuid() }, 0);
    // SAFETY: both namespace identities differ from the parent's. Making the
    // whole mount tree private prevents any later bind from propagating out.
    let result = unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());

    let scratch = Scratch::new();
    let mut mounts = Mounts(Vec::new());
    let executable = executable().canonicalize().unwrap();
    let mut passwd =
        b"effective-user:x:0:0:Effective user:/:/bin/false\nax_login_\xff:x:42424:0:".to_vec();
    // Exercise reentrant passwd lookups whose complete record needs more than
    // the usual initial buffer, without requiring a long login name.
    passwd.extend(std::iter::repeat_n(b'x', 32768));
    passwd.extend_from_slice(b":/:/bin/false\n");
    let passwd_path = scratch.file("etc/passwd", passwd);
    scratch.file("etc/nsswitch.conf", b"passwd: files\ngroup: files\n");
    let loginuid = scratch.file("proc/self/loginuid", b"42424");
    symlink(&executable, scratch.0.join("proc/self/exe")).unwrap();
    fs::create_dir(scratch.0.join("run")).unwrap();
    let mut run_targets = Vec::new();
    for target in ["/run", "/var/run"] {
        if Path::new(target).is_dir() {
            let target = fs::canonicalize(target).unwrap();
            if !run_targets.contains(&target) {
                run_targets.push(target);
            }
        }
    }
    mounts.bind(&scratch.0.join("etc"), Path::new("/etc"));
    for target in run_targets {
        mounts.bind(&scratch.0.join("run"), &target);
    }
    mounts.bind(&scratch.0.join("proc"), Path::new("/proc"));

    assert_login(b"ax_login_\xff\n");
    fs::write(&loginuid, b"0").unwrap();
    assert_login(b"effective-user\n");
    for uid in ["4294967295", "42425", "not-a-uid", "-1", "4294967296", ""] {
        fs::write(&loginuid, uid).unwrap();
        assert_no_login();
    }
    fs::remove_file(&loginuid).unwrap();
    assert_no_login();
    fs::write(&loginuid, b"42424").unwrap();
    fs::write(
        &passwd_path,
        b"effective-user:x:0:0:Effective user:/:/bin/false\n",
    )
    .unwrap();
    assert_no_login();
    fs::write(&passwd_path, b":x:42424:0:Empty name:/:/bin/false\n").unwrap();
    assert_no_login();
}
