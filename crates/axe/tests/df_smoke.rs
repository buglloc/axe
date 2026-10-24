#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-df-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create df scratch directory");
        Self(fs::canonicalize(path).expect("resolve df scratch directory"))
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

fn df(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", "df", "--"])
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("DF_BLOCK_SIZE", "7")
        .env("AXE_STORE_DIR", root.join("store"))
        .env("AXE_STORE_URL", "http://127.0.0.1:9")
        .env("AXE_STORE_ADDRESSES", "127.0.0.1");
    command
}

fn stdout(command: &mut Command) -> String {
    let output = command.output().expect("run df applet");
    assert!(
        output.status.success(),
        "status: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");
    String::from_utf8(output.stdout).expect("df emits UTF-8")
}

#[test]
fn megabyte_options_preserve_units_precedence_and_locale_headers() {
    let scratch = Scratch::new("megabytes");
    let root = scratch.path();
    let megabytes = stdout(df(root).args(["-B", "1M", "--output=size"]).arg(root));
    assert_eq!(megabytes.lines().next().unwrap().trim(), "1M-blocks");

    for args in [
        &["-m", "--output=size"][..],
        &["--output=size", "-m"][..],
        &["-k", "-m", "--output=size"][..],
        &["-B", "2K", "-m", "--output=size"][..],
    ] {
        assert_eq!(
            stdout(df(root).args(args).arg(root)),
            megabytes,
            "df {}",
            args.join(" ")
        );
    }

    for (args, block_size) in [
        (&["-m", "-k", "--output=size"][..], "1K"),
        (&["-m", "-B", "2K", "--output=size"][..], "2K"),
    ] {
        let expected = stdout(df(root).args(["-B", block_size, "--output=size"]).arg(root));
        assert_eq!(stdout(df(root).args(args).arg(root)), expected);
    }

    let default_columns = stdout(df(root).arg("-m").arg(root));
    let headers: Vec<_> = default_columns
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    let expected = [
        "Filesystem",
        "1M-blocks",
        "Used",
        "Available",
        #[cfg(target_vendor = "apple")]
        "Capacity",
        "Use%",
        "Mounted",
        "on",
    ];
    assert_eq!(headers, expected);
    let help = stdout(df(root).arg("--help"));
    assert!(help.contains("-m"), "help: {help}");
    assert!(help.contains("like --block-size=1M"), "help: {help}");
}

#[cfg(target_os = "linux")]
#[test]
fn bind_mounts_keep_only_the_best_device_row() {
    const CHILD_ROOT: &str = "AXE_TEST_DF_BIND_ROOT";
    const TEST_NAME: &str = "bind_mounts_keep_only_the_best_device_row";

    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        use std::ffi::{CStr, CString};
        use std::os::unix::ffi::OsStrExt;

        fn mount(source: &CStr, target: &Path, filesystem: Option<&CStr>, flags: libc::c_ulong) {
            let target =
                CString::new(target.as_os_str().as_bytes()).expect("mount path has no NUL");
            // SAFETY: the C strings remain valid for the call. This test child
            // owns its user and mount namespaces; no host mount is changed.
            let result = unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    filesystem.map_or(std::ptr::null(), CStr::as_ptr),
                    flags,
                    std::ptr::null(),
                )
            };
            assert_eq!(
                result,
                0,
                "mount {}: {}",
                target.to_string_lossy(),
                std::io::Error::last_os_error()
            );
        }

        let root = PathBuf::from(root);
        let original = root.join("original-long-mount");
        let better = root.join("better");
        let best = root.join("b");
        let worse = root.join("worse-mounted-after-best");
        for directory in [&original, &better, &best, &worse] {
            fs::create_dir(directory).expect("create mount point");
        }
        mount(c"axe-df-bind", &original, Some(c"tmpfs"), 0);
        let source = CString::new(original.as_os_str().as_bytes()).unwrap();
        for target in [&better, &best, &worse] {
            mount(&source, target, None, libc::MS_BIND);
        }

        let table = stdout(df(&root).args(["--type=tmpfs", "--output=source,target"]));
        let rows: Vec<_> = table
            .lines()
            .filter(|line| line.split_whitespace().next() == Some("axe-df-bind"))
            .collect();
        assert_eq!(rows.len(), 1, "df must select one row per device:\n{table}");
        assert_eq!(
            rows[0]
                .trim_start()
                .strip_prefix("axe-df-bind")
                .unwrap()
                .trim(),
            best.to_str().unwrap(),
            "df must replace earlier, less representative mounts:\n{table}"
        );
        return;
    }

    let scratch = Scratch::new("binds");
    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "--fork", "--"])
        .arg(std::env::current_exe().expect("locate df test executable"))
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_ROOT, scratch.path())
        .output()
        .expect("start isolated df test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success()
        && stderr.contains("unshare: unshare failed:")
        && (stderr.contains("Operation not permitted") || stderr.contains("Permission denied"))
    {
        eprintln!("df bind proof skipped: {}", stderr.trim());
        return;
    }
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
}
