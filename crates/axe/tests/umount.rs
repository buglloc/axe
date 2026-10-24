#![cfg(target_os = "linux")]

use std::ffi::{CString, OsStr};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

fn axe(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(args)
        .output()
        .expect("run axe")
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn option_errors_are_umount_errors() {
    for options in [
        vec!["umount", "-Z"],
        vec!["umount", "--unknown-option"],
        vec!["umount", "--types"],
        vec!["umount", "-N"],
        vec!["umount", "--lazy=yes"],
    ] {
        let output = axe(&options.iter().map(OsStr::new).collect::<Vec<_>>());
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.starts_with("umount: "));
        assert!(stderr.ends_with("Try 'umount --help' for more information.\n"));
        assert!(!stderr.contains("Try 'mount --help'"));
    }
}

#[test]
fn end_of_options_and_quiet_missing_targets_are_safe_without_privileges() {
    let output = axe(&[
        OsStr::new("umount"),
        OsStr::new("--fake"),
        OsStr::new("-cq"),
        OsStr::new("--"),
        OsStr::new("--axe-umount-not-a-mounted-target"),
    ]);
    assert_eq!(output.status.code(), Some(32));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

struct Fixture {
    root: PathBuf,
    mounted: Vec<PathBuf>,
}

impl Fixture {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        loop {
            let root = std::env::temp_dir().join(format!(
                "axe-umount-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    return Self {
                        root,
                        mounted: Vec::new(),
                    };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create fixture: {error}"),
            }
        }
    }

    fn directory(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(&path).expect("create mountpoint");
        path
    }

    fn tmpfs(&mut self, target: &Path, source: &str, options: &str) {
        success(axe(&[
            OsStr::new("mount"),
            OsStr::new("-it"),
            OsStr::new("tmpfs"),
            OsStr::new("-o"),
            OsStr::new(options),
            OsStr::new(source),
            target.as_os_str(),
        ]));
        self.mounted.push(target.to_owned());
    }

    fn bind(&mut self, source: &Path, target: &Path) {
        success(axe(&[
            OsStr::new("mount"),
            OsStr::new("-iB"),
            source.as_os_str(),
            target.as_os_str(),
        ]));
        self.mounted.push(target.to_owned());
    }

    fn count(&self, target: &Path) -> usize {
        let mut escaped = Vec::with_capacity(target.as_os_str().len());
        for &byte in target.as_os_str().as_bytes() {
            match byte {
                b' ' | b'\t' | b'\n' | b'\\' => escaped.extend_from_slice(&[
                    b'\\',
                    b'0' + (byte >> 6),
                    b'0' + ((byte >> 3) & 7),
                    b'0' + (byte & 7),
                ]),
                byte => escaped.push(byte),
            }
        }
        fs::read("/proc/self/mountinfo")
            .unwrap()
            .split(|byte| *byte == b'\n')
            .filter(|line| line.split(|byte| *byte == b' ').nth(4) == Some(escaped.as_slice()))
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let mut clean = true;
        for target in self.mounted.iter().rev() {
            let path = CString::new(target.as_os_str().as_bytes()).expect("fixture path has no NUL");
            // SAFETY: only this fixture's scratch mountpoints are detached;
            // the NUL-terminated path outlives the syscall.
            if unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) } != 0
                && !matches!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EINVAL | libc::ENOENT)
                )
            {
                clean = false;
            }
        }
        if clean {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

struct WorkingDirectory(PathBuf);

impl WorkingDirectory {
    fn enter(path: &Path) -> Self {
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).expect("hold busy mount as working directory");
        Self(original)
    }
}

impl Drop for WorkingDirectory {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore working directory");
    }
}

/// Runs the destructive cases only in a new user and mount namespace.
#[test]
#[ignore = "requires unshare -rm and unprivileged user namespaces"]
fn namespace_real_smoke() {
    const PARENT_NAMESPACE: &str = "AXE_UMOUNT_PARENT_NAMESPACE";
    if let Some(parent_namespace) = std::env::var_os(PARENT_NAMESPACE) {
        let namespace = fs::read_link("/proc/self/ns/mnt").expect("read child mount namespace");
        assert_ne!(namespace.as_os_str(), parent_namespace);
        namespace_fixture();
        return;
    }
    let namespace = fs::read_link("/proc/self/ns/mnt").expect("read parent mount namespace");
    let output = Command::new("unshare")
        .args(["-rm", "--"])
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "namespace_real_smoke", "--nocapture"])
        .env(PARENT_NAMESPACE, namespace)
        .output()
        .expect("run isolated unmount smoke");
    success(output);
}

fn namespace_fixture() {
    let mut fixture = Fixture::new();
    let tree = fixture.directory("tree");
    fixture.tmpfs(&tree, "lower", "size=1m");
    let hidden = fixture.directory("tree/hidden");
    fixture.tmpfs(&hidden, "hidden", "size=1m");
    fixture.tmpfs(&tree, "upper", "size=1m");
    let visible = fixture.directory("tree/visible");
    fixture.tmpfs(&visible, "visible", "size=1m");

    let fake = success(axe(&[
        OsStr::new("umount"),
        OsStr::new("--fake"),
        OsStr::new("-Riv"),
        tree.as_os_str(),
    ]));
    assert!(fake.stdout.is_empty());
    assert_eq!(fixture.count(&tree), 2);
    assert_eq!(fixture.count(&visible), 1);
    assert_eq!(String::from_utf8(fake.stderr).unwrap().lines().count(), 4);

    let alias = fixture.root.join("tree-link");
    std::os::unix::fs::symlink(&tree, &alias).unwrap();
    let fake = success(axe(&[
        OsStr::new("umount"),
        OsStr::new("--fake"),
        OsStr::new("-vitext4"),
        OsStr::new("--target"),
        alias.as_os_str(),
    ]));
    assert!(fake.stdout.is_empty());
    assert!(String::from_utf8(fake.stderr).unwrap().contains("(upper) unmounted"));
    assert_eq!(fixture.count(&tree), 2);

    let removed = success(axe(&[
        OsStr::new("umount"),
        OsStr::new("-Riv"),
        tree.as_os_str(),
    ]));
    let stderr = String::from_utf8(removed.stderr).unwrap();
    let actions = stderr
        .lines()
        .map(|line| line.rsplit_once(" (").unwrap().1)
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        [
            "visible) unmounted",
            "upper) unmounted",
            "hidden) unmounted",
            "lower) unmounted"
        ]
    );
    assert_eq!(fixture.count(&tree), 0);

    let shared = fixture.directory("shared");
    let alias = fixture.directory("shared-alias");
    fixture.tmpfs(&shared, "shared", "size=1m");
    fixture.bind(&shared, &alias);
    fixture.bind(&shared, &alias);
    let child = fixture.directory("shared/child");
    fixture.tmpfs(&child, "different-filesystem", "size=1m");
    let partial = axe(&[
        OsStr::new("umount"),
        OsStr::new("-Aiv"),
        shared.as_os_str(),
    ]);
    assert_eq!(partial.status.code(), Some(64));
    assert_eq!(fixture.count(&alias), 0);
    assert_eq!(fixture.count(&shared), 1);
    assert_eq!(fixture.count(&child), 1);
    success(axe(&[
        OsStr::new("umount"),
        OsStr::new("-i"),
        child.as_os_str(),
    ]));
    success(axe(&[
        OsStr::new("umount"),
        OsStr::new("-Ai"),
        shared.as_os_str(),
    ]));
    assert_eq!(fixture.count(&shared), 0);

    let all = fixture.directory("all-stack");
    for source in ["all-lower", "all-upper"] {
        fixture.tmpfs(&all, source, "size=12344k,nr_inodes=173");
    }
    success(axe(&[
        OsStr::new("umount"),
        OsStr::new("-aic"),
        OsStr::new("--types=tmpfs"),
        OsStr::new("-Osize=12344k,nr_inodes=173"),
    ]));
    assert_eq!(fixture.count(&all), 0);

    let chain = fixture.directory("chain");
    fixture.tmpfs(&chain, "chain", "size=1m");
    let early = fixture.directory("chain/early");
    fixture.tmpfs(&early, "early", "size=1m");
    let busy = fixture.directory("chain/busy");
    fixture.tmpfs(&busy, "busy", "size=1m");
    let independent = fixture.directory("independent");
    fixture.tmpfs(&independent, "independent", "size=1m");
    let missing = fixture.root.join("missing");
    {
        let _working_directory = WorkingDirectory::enter(&busy);
        let partial = axe(&[
            OsStr::new("umount"),
            OsStr::new("-Ric"),
            chain.as_os_str(),
            missing.as_os_str(),
            independent.as_os_str(),
        ]);
        assert_eq!(partial.status.code(), Some(64));
        let stderr = String::from_utf8(partial.stderr).unwrap();
        assert!(stderr.contains("target is busy."));
        assert!(stderr.contains("no mount point specified."));
    }
    assert_eq!(fixture.count(&chain), 1);
    assert_eq!(fixture.count(&early), 1);
    assert_eq!(fixture.count(&busy), 1);
    assert_eq!(fixture.count(&independent), 0);
    success(axe(&[
        OsStr::new("umount"),
        OsStr::new("-Ric"),
        chain.as_os_str(),
    ]));

    let source = fixture.root.join("source");
    fs::write(&source, []).unwrap();
    let source_alias = fixture.root.join("source-link");
    std::os::unix::fs::symlink(&source, &source_alias).unwrap();
    let target = fixture.directory("source-target");
    fixture.tmpfs(&target, source.to_str().unwrap(), "size=1m");
    let namespace = std::process::id().to_string();
    success(axe(&[
        OsStr::new("umount"),
        OsStr::new("-iN"),
        OsStr::new(&namespace),
        OsStr::new("--source"),
        source_alias.as_os_str(),
    ]));
    assert_eq!(fixture.count(&target), 0);
}
