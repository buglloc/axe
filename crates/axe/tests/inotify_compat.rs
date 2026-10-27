#![cfg(target_os = "linux")]

use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-inotify-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Watching {
    child: Option<Child>,
    setup: Vec<u8>,
}

impl Watching {
    fn start(applet: &str, args: &[&str], operands: &[OsString]) -> Self {
        Self::start_with_binary(
            Path::new(env!("CARGO_BIN_EXE_axe")),
            applet,
            args,
            operands,
            false,
        )
    }

    fn start_with_binary(
        binary: &Path,
        applet: &str,
        args: &[&str],
        operands: &[OsString],
        drop_dac: bool,
    ) -> Self {
        let mut command = Command::new(binary);
        command
            .args(["--applet", applet, "--"])
            .args(args)
            .args(operands)
            .env("PATH", "/nonexistent")
            .env("AXE_STORE_MODE", "off")
            .env("LC_ALL", "C")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if drop_dac {
            // SAFETY: the closure only changes the child process credentials before exec.
            unsafe {
                command.pre_exec(|| {
                    let mut header = [0u32; 2];
                    header[0] = 0x2008_0522;
                    let mut data = [0u32; 6];
                    if libc::syscall(libc::SYS_capset, header.as_mut_ptr(), data.as_mut_ptr()) == -1
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = command.spawn().expect("start inotify applet");
        let mut watcher = Self {
            child: Some(child),
            setup: Vec::new(),
        };
        let ready: &[u8] = if applet == "inotifywait" {
            b"Watches established.\n"
        } else {
            b"Finished establishing watches, now collecting statistics.\n"
        };
        loop {
            let mut byte = [0];
            let count = watcher
                .child
                .as_mut()
                .expect("owned child")
                .stderr
                .as_mut()
                .expect("piped stderr")
                .read(&mut byte)
                .expect("read setup diagnostics");
            assert_ne!(
                count,
                0,
                "watch setup failed: {:?}",
                String::from_utf8_lossy(&watcher.setup)
            );
            watcher.setup.push(byte[0]);
            if watcher.setup.ends_with(ready) {
                break;
            }
        }
        watcher
    }

    fn watched_inodes(&self) -> Vec<u64> {
        let child = self.child.as_ref().expect("owned child");
        fs::read_dir(format!("/proc/{}/fdinfo", child.id()))
            .expect("read owned child descriptors")
            .flatten()
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .flat_map(|info| {
                info.lines()
                    .filter(|line| line.starts_with("inotify wd:"))
                    .filter_map(|line| {
                        line.split_ascii_whitespace()
                            .find_map(|field| field.strip_prefix("ino:"))
                            .and_then(|value| u64::from_str_radix(value, 16).ok())
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn wait_until_watched(&mut self, path: &Path) {
        let inode = fs::metadata(path)
            .expect("watched directory metadata")
            .ino();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.watched_inodes().contains(&inode) {
            assert!(
                Instant::now() < deadline,
                "new directory never received a watch: {path:?}"
            );
            assert!(
                self.child
                    .as_mut()
                    .expect("owned child")
                    .try_wait()
                    .expect("child status")
                    .is_none()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn signal(&mut self, signal: i32) {
        let child = self.child.as_mut().expect("owned child");
        assert!(
            child
                .try_wait()
                .expect("child status before signal")
                .is_none()
        );
        let pid = i32::try_from(child.id()).expect("child PID fits pid_t");
        // SAFETY: the unreaped child handle owns this positive PID.
        assert_eq!(
            unsafe { libc::kill(pid, signal) },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
    }

    fn stop(&mut self) {
        self.signal(libc::SIGSTOP);
        let pid = self.child.as_ref().expect("owned child").id();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status =
                fs::read_to_string(format!("/proc/{pid}/status")).expect("owned child status");
            if status.lines().any(|line| {
                line.starts_with("State:") && line.split_ascii_whitespace().nth(1) == Some("T")
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "owned watcher did not stop");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn finish(mut self) -> Output {
        let mut output = self
            .child
            .take()
            .expect("owned child")
            .wait_with_output()
            .expect("collect watcher");
        self.setup.extend(output.stderr);
        output.stderr = std::mem::take(&mut self.setup);
        output
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn exclusion(path: &Path) -> OsString {
    OsString::from_vec([b"@", path.as_os_str().as_bytes()].concat())
}

#[test]
fn nonrecursive_wait_and_watch_never_install_child_directory_watches() {
    let scratch = Scratch::new("nonrecursive");
    let nested = scratch.0.join("nested");
    fs::create_dir(&nested).expect("create nested directory");
    for applet in ["inotifywait", "inotifywatch"] {
        let watcher = Watching::start(
            applet,
            &["-e", "close_write", "-t", "1"],
            &[scratch.0.clone().into_os_string()],
        );
        let watches = watcher.watched_inodes();
        assert_eq!(
            watches,
            vec![fs::metadata(&scratch.0).expect("root metadata").ino()]
        );
        fs::write(nested.join("inside"), b"not in scope").expect("write nested file");
        let output = watcher.finish();
        assert_eq!(
            output.status.code(),
            Some(if applet == "inotifywait" { 2 } else { 0 })
        );
        assert!(output.stdout.is_empty(), "{applet}: {:?}", output.stdout);
        if applet == "inotifywatch" {
            assert!(!output.stderr.is_empty(), "{:?}", output.stderr);
        }
    }
}

#[test]
fn recursive_wait_skips_inaccessible_subtrees() {
    let scratch = Scratch::new("inaccessible");
    fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o755))
        .expect("make scratch searchable");
    let inaccessible = scratch.0.join("inaccessible");
    let accessible = scratch.0.join("accessible");
    fs::create_dir_all(inaccessible.join("nested")).expect("create inaccessible subtree");
    fs::create_dir(&accessible).expect("create accessible subtree");
    fs::set_permissions(&inaccessible, fs::Permissions::from_mode(0o000))
        .expect("make subtree inaccessible");
    let binary = Path::new(env!("CARGO_BIN_EXE_axe"));
    let drop_dac = unsafe { libc::geteuid() } == 0;
    let watcher = Watching::start_with_binary(
        binary,
        "inotifywait",
        &["-r", "-e", "close_write", "-t", "2", "--format", "%f"],
        &[scratch.0.clone().into_os_string()],
        drop_dac,
    );
    let watches = watcher.watched_inodes();
    assert!(
        watches.contains(
            &fs::metadata(&accessible)
                .expect("accessible metadata")
                .ino()
        )
    );
    assert!(
        !watches.contains(
            &fs::metadata(&inaccessible)
                .expect("inaccessible metadata")
                .ino()
        )
    );

    fs::write(accessible.join("accepted"), b"accepted").expect("write accepted event");
    let output = watcher.finish();
    fs::set_permissions(&inaccessible, fs::Permissions::from_mode(0o755))
        .expect("restore subtree permissions");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"accepted\n");
}

#[test]
fn recursive_exclusions_prune_initial_subtrees_and_do_not_match_sibling_prefixes() {
    let scratch = Scratch::new("initial-exclusions");
    let excluded = scratch.0.join("excluded");
    let included = scratch.0.join("excluded-other");
    fs::create_dir_all(excluded.join("deep")).expect("create excluded subtree");
    fs::create_dir(&included).expect("create similarly named included subtree");
    for applet in ["inotifywait", "inotifywatch"] {
        let args = if applet == "inotifywait" {
            vec!["-r", "-e", "close_write", "-t", "1", "--format", "%f"]
        } else {
            vec!["-r", "-e", "close_write", "-t", "1"]
        };
        let watcher = Watching::start(
            applet,
            &args,
            &[scratch.0.clone().into_os_string(), exclusion(&excluded)],
        );
        let watches = watcher.watched_inodes();
        assert_eq!(watches.len(), 2);
        assert!(!watches.contains(&fs::metadata(&excluded).expect("excluded metadata").ino()));
        fs::write(excluded.join("deep/ignored"), b"ignored").expect("write excluded file");
        fs::write(included.join("accepted"), b"accepted").expect("write included file");
        let output = watcher.finish();
        assert!(output.status.success(), "{:?}", output.stderr);
        if applet == "inotifywait" {
            assert_eq!(output.stdout, b"accepted\n");
        } else {
            assert_eq!(
                output.stdout,
                format!(
                    "total  close_write  filename\n1      1            {}/\n",
                    included.display()
                )
                .as_bytes()
            );
        }
    }
}

#[test]
fn recursive_exclusions_apply_before_new_subtree_watches_and_internal_events_do_not_leak() {
    let scratch = Scratch::new("new-exclusions");
    for (index, applet) in ["inotifywait", "inotifywatch"].into_iter().enumerate() {
        let root = scratch.0.join(index.to_string());
        fs::create_dir(&root).expect("create isolated watched root");
        let excluded = root.join("later-excluded");
        let included = root.join("later-included");
        let args = if applet == "inotifywait" {
            vec!["-r", "-e", "close_write", "-t", "3", "--format", "%f"]
        } else {
            vec!["-r", "-e", "close_write", "-t", "3"]
        };
        let mut watcher = Watching::start(
            applet,
            &args,
            &[root.clone().into_os_string(), exclusion(&excluded)],
        );
        fs::create_dir_all(excluded.join("deep")).expect("create future excluded subtree");
        fs::create_dir(&included).expect("create future included directory");
        watcher.wait_until_watched(&included);
        let watches = watcher.watched_inodes();
        assert_eq!(watches.len(), 2);
        assert!(!watches.contains(&fs::metadata(&excluded).expect("excluded metadata").ino()));
        fs::write(excluded.join("deep/ignored"), b"ignored").expect("write future excluded file");
        fs::write(included.join("accepted"), b"accepted").expect("write future included file");
        let output = watcher.finish();
        assert!(output.status.success(), "{:?}", output.stderr);
        if applet == "inotifywait" {
            assert_eq!(output.stdout, b"accepted\n");
        } else {
            assert_eq!(
                output.stdout,
                format!(
                    "total  close_write  filename\n1      1            {}/\n",
                    included.display()
                )
                .as_bytes()
            );
        }
    }
}

#[test]
fn zero_mode_without_events_has_no_table_or_header() {
    let scratch = Scratch::new("empty-zero");
    let watcher = Watching::start(
        "inotifywatch",
        &["-v", "-z", "-e", "create", "-t", "1"],
        &[scratch.0.clone().into_os_string()],
    );
    let output = watcher.finish();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn raw_watch_and_exclusion_paths_and_format_names_remain_byte_exact() {
    let scratch = Scratch::new("raw-paths");
    let root = scratch.0.join(OsString::from_vec(b"root-\xff".to_vec()));
    let excluded = root.join(OsString::from_vec(b"excluded-\xfe".to_vec()));
    fs::create_dir_all(&excluded).expect("create raw watch directories");
    let watcher = Watching::start(
        "inotifywait",
        &[
            "-r",
            "-e",
            "close_write",
            "-t",
            "1",
            "--format",
            "%w%f%0",
            "--no-newline",
            "--",
        ],
        &[root.clone().into_os_string(), exclusion(&excluded)],
    );
    fs::write(excluded.join("ignored"), b"ignored").expect("write excluded file");
    let filename = OsStr::from_bytes(b"accepted-\xfd");
    fs::write(root.join(filename), b"accepted").expect("write raw file");
    let output = watcher.finish();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(
        output.stdout,
        [
            root.as_os_str().as_bytes(),
            b"/",
            filename.as_bytes(),
            b"\0"
        ]
        .concat()
    );
}

#[test]
fn zero_mode_keeps_zero_rows_only_when_a_selected_event_occurred_and_paths_stay_raw() {
    let scratch = Scratch::new("zero-with-event");
    let root = scratch.0.join(OsString::from_vec(b"root-\xff".to_vec()));
    let child = root.join(OsString::from_vec(b"child-\xfe".to_vec()));
    fs::create_dir_all(&child).expect("create raw watched subtree");
    let watcher = Watching::start(
        "inotifywatch",
        &["-r", "-z", "-e", "close_write", "-t", "1"],
        &[root.clone().into_os_string()],
    );
    fs::write(root.join("accepted"), b"accepted").expect("write eventful file");
    let output = watcher.finish();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(
        output.stdout,
        [
            b"total  close_write  filename\n1      1            ",
            root.as_os_str().as_bytes(),
            b"/\n0      0            ",
            child.as_os_str().as_bytes(),
            b"/\n",
        ]
        .concat()
    );
}

#[test]
fn vanished_recursive_discovery_directories_do_not_terminate_either_watcher() {
    let scratch = Scratch::new("vanished-discovery");
    for applet in ["inotifywait", "inotifywatch"] {
        let root = scratch.0.join(applet);
        fs::create_dir(&root).expect("create isolated root");
        let args = if applet == "inotifywait" {
            vec!["-m", "-r", "-e", "close_write", "-t", "3", "--format", "%f"]
        } else {
            vec!["-r", "-e", "close_write", "-t", "3"]
        };
        let mut watcher = Watching::start(applet, &args, &[root.clone().into_os_string()]);
        watcher.stop();
        let transient = root.join("already-gone");
        fs::create_dir(&transient).expect("queue internal directory create");
        fs::remove_dir(&transient).expect("remove before watcher can discover directory");
        watcher.signal(libc::SIGCONT);
        fs::write(root.join("accepted"), b"accepted").expect("write selected event after resume");
        let output = watcher.finish();
        if applet == "inotifywait" {
            assert_eq!(output.status.code(), Some(2), "{:?}", output.stderr);
            assert_eq!(output.stdout, b"accepted\n");
        } else {
            assert!(output.status.success(), "{:?}", output.stderr);
            assert_eq!(
                output.stdout,
                format!(
                    "total  close_write  filename\n1      1            {}/\n",
                    root.display()
                )
                .as_bytes()
            );
        }
    }
}

#[test]
fn missing_explicit_operands_still_fail_instead_of_being_treated_as_discovery_races() {
    let scratch = Scratch::new("missing-explicit");
    let missing = scratch.0.join("missing");
    for applet in ["inotifywait", "inotifywatch"] {
        let output = Command::new(env!("CARGO_BIN_EXE_axe"))
            .args([
                "--applet",
                applet,
                "--",
                "-r",
                "-e",
                "close_write",
                "-t",
                "1",
            ])
            .arg(&missing)
            .output()
            .expect("run missing explicit operand");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            output
                .stderr
                .windows(missing.as_os_str().as_bytes().len())
                .any(|text| text == missing.as_os_str().as_bytes())
        );
    }
}

#[test]
fn byte_regex_include_and_exclude_patterns_cover_non_utf8_filenames() {
    let scratch = Scratch::new("byte-regex");
    for applet in ["inotifywait", "inotifywatch"] {
        for (index, filter) in [vec!["--include", "^.*$"], vec!["--exclude", "^.*\\.tmp$"]]
            .into_iter()
            .enumerate()
        {
            let root = scratch.0.join(format!("{applet}-{index}"));
            fs::create_dir(&root).expect("create regex root");
            let mut args = vec!["-e", "close_write", "-t", "1"];
            if applet == "inotifywait" {
                args.extend(["--format", "%f"]);
            }
            args.extend(filter);
            let watcher = Watching::start(applet, &args, &[root.clone().into_os_string()]);
            if index == 1 {
                fs::write(
                    root.join(OsStr::from_bytes(b"ignored-\xff.tmp")),
                    b"ignored",
                )
                .expect("write excluded raw name");
            }
            fs::write(
                root.join(OsStr::from_bytes(b"accepted-\xff.txt")),
                b"accepted",
            )
            .expect("write included raw name");
            let output = watcher.finish();
            assert!(
                output.status.success(),
                "{applet}, {index}: {:?}",
                output.stderr
            );
            if applet == "inotifywait" {
                assert_eq!(output.stdout, b"accepted-\xff.txt\n");
            } else {
                assert_eq!(
                    output.stdout,
                    format!(
                        "total  close_write  filename\n1      1            {}/\n",
                        root.display()
                    )
                    .as_bytes()
                );
            }
        }
    }
    for (index, filter) in [["--includei", "^.*\\.TXT$"], ["--excludei", "^.*\\.TMP$"]]
        .into_iter()
        .enumerate()
    {
        let root = scratch.0.join(format!("case-insensitive-{index}"));
        fs::create_dir(&root).expect("create case-insensitive regex root");
        let watcher = Watching::start(
            "inotifywait",
            &[
                "-e",
                "close_write",
                "-t",
                "1",
                "--format",
                "%f",
                filter[0],
                filter[1],
            ],
            &[root.clone().into_os_string()],
        );
        if index == 1 {
            fs::write(
                root.join(OsStr::from_bytes(b"ignored-\xff.tmp")),
                b"ignored",
            )
            .expect("write excluded raw name");
        }
        fs::write(
            root.join(OsStr::from_bytes(b"accepted-\xff.txt")),
            b"accepted",
        )
        .expect("write included raw name");
        let output = watcher.finish();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(output.stdout, b"accepted-\xff.txt\n");
    }
}

struct PrivateTmpfs {
    path: CString,
    mounted: bool,
}

impl PrivateTmpfs {
    fn new(path: &Path) -> Self {
        let mut mount = Self {
            path: CString::new(path.as_os_str().as_bytes()).expect("mount path has no NUL"),
            mounted: false,
        };
        let source = CString::new("axe-inotify-fixture").unwrap();
        let filesystem = CString::new("tmpfs").unwrap();
        // SAFETY: the test verifies mount-namespace ownership before constructing any mount.
        let result = unsafe {
            libc::mount(
                source.as_ptr(),
                mount.path.as_ptr(),
                filesystem.as_ptr(),
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        mount.mounted = true;
        mount
    }

    fn unmount(&mut self) {
        // SAFETY: this path records a mount created by this fixture in its verified private namespace.
        let result = unsafe { libc::umount2(self.path.as_ptr(), 0) };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        self.mounted = false;
    }
}

impl Drop for PrivateTmpfs {
    fn drop(&mut self) {
        if self.mounted {
            // SAFETY: only this fixture's recorded private-namespace mount is detached.
            let result = unsafe { libc::umount2(self.path.as_ptr(), libc::MNT_DETACH) };
            if result != 0 && !std::thread::panicking() {
                panic!("detach private tmpfs: {}", std::io::Error::last_os_error());
            }
        }
    }
}

#[test]
#[ignore = "requires unshare and permitted unprivileged user/mount namespaces"]
fn private_namespace_unmount_events_are_selected_without_other_kernel_events_leaking() {
    const CHILD: &str = "AXE_INOTIFY_UNMOUNT_CHILD";
    let namespaces = ["mnt", "user"];
    if std::env::var_os(CHILD).is_none() {
        let unshare = std::env::var_os("AXE_REFERENCE_PATH")
            .into_iter()
            .chain(std::env::var_os("PATH"))
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join("unshare"))
            .find(|path| path.is_file())
            .expect("unshare must be available in AXE_REFERENCE_PATH or PATH");
        let mut command = Command::new(unshare);
        command
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "--propagation",
                "unchanged",
                "--fork",
                "--",
            ])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_namespace_unmount_events_are_selected_without_other_kernel_events_leaking",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1");
        for namespace in namespaces {
            command.env(
                format!("AXE_INOTIFY_PARENT_{namespace}"),
                fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
            );
        }
        let output = command.output().expect("run isolated unmount event test");
        assert!(output.status.success(), "{output:?}");
        return;
    }
    for namespace in namespaces {
        let parent = PathBuf::from(
            std::env::var_os(format!("AXE_INOTIFY_PARENT_{namespace}"))
                .expect("parent namespace identity"),
        );
        assert_ne!(
            fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
            parent,
            "namespace must differ before mount mutations"
        );
    }
    // SAFETY: geteuid has no arguments or side effects.
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let mount_namespace = File::open("/proc/self/ns/mnt").expect("open private mount namespace");
    // SAFETY: NS_GET_USERNS takes no pointer and returns a newly owned descriptor on success.
    let owner_fd = unsafe { libc::ioctl(mount_namespace.as_raw_fd(), libc::NS_GET_USERNS) };
    assert!(owner_fd >= 0, "{}", std::io::Error::last_os_error());
    // SAFETY: the successful ioctl returned this owned, nonnegative descriptor.
    let owner = unsafe { File::from_raw_fd(owner_fd) };
    let owner_identity = owner.metadata().expect("mount namespace owner identity");
    let user_identity =
        fs::metadata("/proc/self/ns/user").expect("current user namespace identity");
    assert_eq!(
        (owner_identity.dev(), owner_identity.ino()),
        (user_identity.dev(), user_identity.ino()),
        "new mount namespace must be owned by the new user namespace"
    );
    let root = CString::new("/").unwrap();
    // SAFETY: identity and ownership checks above precede the first mount mutation.
    let result = unsafe {
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    let scratch = Scratch::new("unmount-events");
    for applet in ["inotifywait", "inotifywatch"] {
        let target = scratch.0.join(applet);
        fs::create_dir(&target).expect("create private mount target");
        let mut mount = PrivateTmpfs::new(&target);
        let args = if applet == "inotifywait" {
            vec!["-e", "unmount", "-t", "2", "--format", "%e"]
        } else {
            vec!["-e", "unmount", "-t", "1"]
        };
        let watcher = Watching::start(applet, &args, &[target.clone().into_os_string()]);
        fs::write(target.join("not-selected"), b"ignored ordinary events")
            .expect("write nonselected close event");
        mount.unmount();
        let output = watcher.finish();
        assert!(output.status.success(), "{:?}", output.stderr);
        if applet == "inotifywait" {
            assert_eq!(output.stdout, b"UNMOUNT\n");
        } else {
            assert_eq!(
                output.stdout,
                format!(
                    "total  unmount  filename\n1      1        {}/\n",
                    target.display()
                )
                .as_bytes()
            );
        }
    }
}
