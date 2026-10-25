#![cfg(target_os = "linux")]

use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::pty::{Winsize, openpty};
use nix::sys::termios::{Termios, tcgetattr};

const AXE: &str = env!("CARGO_BIN_EXE_axe");
const WAIT: Duration = Duration::from_secs(5);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-procutils-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct OwnedChild(Option<Child>);

impl OwnedChild {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }

    fn wait_bounded(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child().try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "child did not terminate");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn output(mut self) -> Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn applet(name: &str) -> Command {
    let mut command = Command::new(AXE);
    command
        .args(["--applet", name, "--"])
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("HOME", "/nonexistent")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run(name: &str, args: &[&str]) -> Output {
    OwnedChild::new(applet(name).args(args).spawn().unwrap()).output()
}

fn text(output: &Output) -> &str {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    std::str::from_utf8(&output.stdout).unwrap()
}

#[test]
fn pgrep_count_reports_zero_and_keeps_no_match_status() {
    let output = run("pgrep", &["-c", "-P", "2147483647", "-x", "axabsent"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output.stdout, b"0\n");
    assert!(output.stderr.is_empty(), "{output:?}");
}

struct ProcessFixture {
    child: OwnedChild,
    scratch: Scratch,
    executable: PathBuf,
}

impl ProcessFixture {
    fn new() -> Self {
        let scratch = Scratch::new("child");
        let executable = scratch.0.join("ax fixture");
        symlink(std::env::current_exe().unwrap(), &executable).unwrap();
        fs::write(scratch.0.join("mapping"), vec![0x42; 65536]).unwrap();
        let child = Command::new(&executable)
            .args(["--exact", "process_fixture_child", "--nocapture"])
            .env("AXE_PROCUTILS_CHILD_ROOT", &scratch.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut fixture = Self {
            child: OwnedChild::new(child),
            scratch,
            executable,
        };
        let deadline = Instant::now() + WAIT;
        while !fixture.scratch.0.join("ready").exists() {
            assert!(
                fixture.child.child().try_wait().unwrap().is_none(),
                "fixture exited early"
            );
            assert!(Instant::now() < deadline, "fixture was not ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture
    }

    fn pid(&mut self) -> String {
        self.child.child().id().to_string()
    }
}

#[test]
fn process_fixture_child() {
    let Some(root) = std::env::var_os("AXE_PROCUTILS_CHILD_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let file = File::open(root.join("mapping")).unwrap();
    // SAFETY: the file contains 65536 readable bytes; the mapping is kept
    // alive until the stdin rendezvous completes and then unmapped once.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            65536,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    // SAFETY: mmap succeeded and the first byte lies in its readable range.
    assert_eq!(
        unsafe { std::ptr::read_volatile(mapping.cast::<u8>()) },
        0x42
    );
    fs::write(root.join("ready"), b"ready").unwrap();
    let _ = io::stdin().read(&mut [0]);
    // SAFETY: this is the original address and length of our live mapping.
    assert_eq!(unsafe { libc::munmap(mapping, 65536) }, 0);
}

#[test]
fn pidof_matches_the_full_argv_path_without_matching_an_unrelated_same_basename() {
    let mut fixture = ProcessFixture::new();
    let pid = fixture.pid();
    let output = applet("pidof").arg(&fixture.executable).output().unwrap();
    assert_eq!(text(&output), format!("{pid}\n"));
    let output = run("pidof", &["ax fixture"]);
    assert!(text(&output).split_whitespace().any(|value| value == pid));
    let wrong = fixture
        .scratch
        .0
        .join("not-the-executable")
        .join("ax fixture");
    let output = applet("pidof").arg(wrong).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn skill_signal_zero_probes_an_owned_process_without_terminating_it() {
    let mut fixture = ProcessFixture::new();
    let pid = fixture.pid();
    for args in [
        vec!["-0", "-p", pid.as_str()],
        vec!["--signal", "0", "-p", pid.as_str()],
    ] {
        let output = run("skill", &args);
        assert_eq!(text(&output), "");
        assert!(fixture.child.child().try_wait().unwrap().is_none());
    }
    let output = run("skill", &["-0", "-p", "2147483647"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

#[test]
fn pmap_preserves_missing_pid_status_and_procps_headings_and_hex_devices() {
    let output = run("pmap", &["2147483647"]);
    assert_eq!(output.status.code(), Some(42), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");

    let mut fixture = ProcessFixture::new();
    let pid = fixture.pid();
    let maps = fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    let held = maps
        .lines()
        .find(|line| line.ends_with("/mapping"))
        .unwrap();
    let fields: Vec<_> = held.split_whitespace().collect();
    let address = format!(
        "{:0width$x}",
        u64::from_str_radix(fields[0].split('-').next().unwrap(), 16).unwrap(),
        width = std::mem::size_of::<usize>() * 2,
    );
    let (major, minor) = fields[3].split_once(':').unwrap();
    let expected_device = format!(
        "{:03x}:{:05x}",
        u64::from_str_radix(major, 16).unwrap(),
        u64::from_str_radix(minor, 16).unwrap(),
    );
    let output = run("pmap", &["-d", "-p", &pid]);
    let output = text(&output);
    assert_eq!(
        output
            .lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["Address", "Kbytes", "Mode", "Offset", "Device", "Mapping"]
    );
    let row = output
        .lines()
        .find(|line| line.split_whitespace().next() == Some(address.as_str()))
        .unwrap();
    assert_eq!(row.split_whitespace().nth(4).unwrap(), expected_device);
    let output = run("pmap", &[&pid]);
    assert!(!text(&output).lines().any(|line| line.contains("Address")));
    let output = run("pmap", &["-x", &pid]);
    assert_eq!(
        text(&output)
            .lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["Address", "Kbytes", "RSS", "Dirty", "Mode", "Mapping"]
    );
    let output = run("pmap", &["-xq", &pid]);
    assert!(!text(&output).contains("Address"));
    assert!(!text(&output).contains("total kB"));
}

fn shared_kib(pid: &str) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("RssFile:") || line.starts_with("RssShmem:"))
        .map(|line| {
            line.split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u64>()
                .unwrap()
        })
        .sum()
}

fn displayed_kib(value: &str) -> (f64, f64) {
    for (suffix, scale) in [('m', 1024.0), ('g', 1048576.0), ('t', 1073741824.0)] {
        if let Some(number) = value.strip_suffix(suffix) {
            return (number.parse::<f64>().unwrap() * scale, scale * 0.051);
        }
    }
    (value.parse().unwrap(), 0.0)
}

#[test]
fn top_shared_memory_uses_status_kib_without_a_second_division() {
    let mut fixture = ProcessFixture::new();
    let pid = fixture.pid();
    let before = shared_kib(&pid);
    let output = run("top", &["-b", "-n", "1", "-p", &pid]);
    let after = shared_kib(&pid);
    assert!(before > 1024 && after > 1024);
    let row = text(&output)
        .lines()
        .find(|line| line.split_whitespace().next() == Some(pid.as_str()))
        .unwrap();
    let (actual, tolerance) = displayed_kib(row.split_whitespace().nth(6).unwrap());
    assert!(
        actual + tolerance >= before.min(after) as f64,
        "{row}; {before}..{after} KiB"
    );
    assert!(
        actual - tolerance <= before.max(after) as f64,
        "{row}; {before}..{after} KiB"
    );
}

const MEMINFO: &str = "MemTotal: 65536 kB\nMemFree: 2048 kB\nMemAvailable: 49152 kB\nBuffers: 1000 kB\nCached: 4096 kB\nSwapCached: 0 kB\nActive: 8192 kB\nInactive: 4096 kB\nSwapTotal: 8192 kB\nSwapFree: 7168 kB\nDirty: 0 kB\nWriteback: 0 kB\nMapped: 1024 kB\nShmem: 1024 kB\nSlab: 3072 kB\nSReclaimable: 2048 kB\nCommitted_AS: 16384 kB\nVmallocTotal: 65536 kB\nVmallocUsed: 0 kB\nVmallocChunk: 0 kB\n";

fn memory_fixture() -> Scratch {
    let scratch = Scratch::new("proc");
    fs::write(scratch.0.join("meminfo"), MEMINFO).unwrap();
    write_vmstat(&scratch.0, 10240, 20480);
    fs::write(scratch.0.join("uptime"), b"20.00 10.00\n").unwrap();
    fs::create_dir(scratch.0.join("empty-run")).unwrap();
    scratch
}

fn write_vmstat(root: &Path, input: u64, output: u64) {
    fs::write(
        root.join("vmstat"),
        format!("pswpin {input}\npswpout {output}\npgpgin 3000\npgpgout 4000\n"),
    )
    .unwrap();
}

fn namespace_command(root: &Path, mode: &str, name: &str, args: &[&str]) -> Command {
    let old_user = fs::metadata("/proc/self/ns/user").unwrap().ino();
    let old_mount = fs::metadata("/proc/self/ns/mnt").unwrap().ino();
    // SAFETY: these no-argument syscalls only read this process's IDs.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let uid_map = format!("0 {uid} 1\n").into_bytes();
    let gid_map = format!("0 {gid} 1\n").into_bytes();
    let mut bindings = Vec::new();
    let mut bind = |source: PathBuf, target: &str| {
        bindings.push((
            CString::new(source.as_os_str().as_bytes()).unwrap(),
            CString::new(target).unwrap(),
        ));
    };
    if mode == "memory" {
        for (file, target) in [
            ("meminfo", "/proc/meminfo"),
            ("vmstat", "/proc/vmstat"),
            ("uptime", "/proc/uptime"),
        ] {
            bind(root.join(file), target);
        }
        for (file, target) in [("kernel-stat", "/proc/stat"), ("passwd", "/etc/passwd")] {
            if root.join(file).is_file() {
                bind(root.join(file), target);
            }
        }
        if root.join("process-pid").is_file() {
            let pid: u32 = fs::read_to_string(root.join("process-pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(pid > 0);
            for file in ["status", "stat", "maps", "smaps"] {
                if root.join(file).is_file() {
                    bind(root.join(file), &format!("/proc/{pid}/{file}"));
                }
            }
        }
    } else if Path::new("/var/run").is_dir() {
        bind(root.join("empty-run"), "/var/run");
    }
    let setup = move || {
        // SAFETY: pre_exec runs in a single-threaded fork child. No mount
        // or host file is changed before both new namespaces are verified.
        if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if namespace_inode(c"/proc/self/ns/user")? == old_user
            || namespace_inode(c"/proc/self/ns/mnt")? == old_mount
        {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        match write_kernel_file(c"/proc/self/setgroups", b"deny\n") {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(error) => return Err(error),
        }
        write_kernel_file(c"/proc/self/uid_map", &uid_map)?;
        write_kernel_file(c"/proc/self/gid_map", &gid_map)?;
        // SAFETY: unshare created a mount namespace owned by our new user
        // namespace; its inode differs from the parent's. Propagation is
        // made private before any bind, so parent mounts cannot change.
        if unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        for (source, target) in &bindings {
            // SAFETY: both preallocated C strings remain live. All mount
            // writes are confined to the verified private namespace.
            if unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    };
    let mut command = applet(name);
    command.args(args);
    // SAFETY: setup uses only async-signal-safe libc calls, stack locals,
    // and preallocated buffers. It neither allocates nor takes Rust locks.
    unsafe { command.pre_exec(setup) };
    command
}

fn namespace_inode(path: &CStr) -> io::Result<u64> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: path is NUL-terminated; stat points to writable storage for
    // the complete C struct. stat follows the namespace symlink.
    if unsafe { libc::stat(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: stat succeeded and initialized st_ino.
    Ok(unsafe { stat.assume_init().st_ino })
}

fn write_kernel_file(path: &CStr, data: &[u8]) -> io::Result<()> {
    // SAFETY: path is NUL-terminated; open takes no mode argument without
    // O_CREAT. The returned descriptor is used and closed only here.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut offset = 0;
    let mut error = None;
    while offset < data.len() {
        // SAFETY: offset is within data; a successful write returns at
        // most the supplied length. No other thread uses this descriptor.
        let written =
            unsafe { libc::write(fd, data[offset..].as_ptr().cast(), data.len() - offset) };
        if written < 0 {
            let current = io::Error::last_os_error();
            if current.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            error = Some(current);
            break;
        }
        if written == 0 {
            error = Some(io::Error::from_raw_os_error(libc::EIO));
            break;
        }
        offset += written as usize;
    }
    // SAFETY: fd is our still-open descriptor. On Linux it must not be
    // retried after EINTR; a prior write error retains precedence.
    let closed = unsafe { libc::close(fd) };
    if let Some(error) = error {
        Err(error)
    } else if closed < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn namespace_child(root: &Path, mode: &str, name: &str, args: &[&str]) -> Option<OwnedChild> {
    match namespace_command(root, mode, name, args).spawn() {
        Ok(child) => Some(OwnedChild::new(child)),
        Err(error)
            if error.raw_os_error().is_some_and(|code| {
                [
                    libc::EPERM,
                    libc::EACCES,
                    libc::ENOSYS,
                    libc::ENOSPC,
                    libc::EUSERS,
                    libc::EINVAL,
                ]
                .contains(&code)
            }) =>
        {
            eprintln!("private namespace/bind mounts unavailable: {error}");
            None
        }
        Err(error) => panic!("spawn isolated {name}: {error}"),
    }
}

fn isolated(root: &Path, mode: &str, name: &str, args: &[&str]) -> Option<Output> {
    let output = namespace_child(root, mode, name, args)?.output();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    Some(output)
}

fn labelled<'a>(output: &'a str, label: &str) -> Vec<&'a str> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .unwrap_or_else(|| panic!("missing {label:?}: {output}"))
        .split_whitespace()
        .collect()
}

fn vm_rows(output: &str) -> Vec<Vec<u64>> {
    output
        .lines()
        .filter_map(|line| {
            let values: Result<Vec<_>, _> =
                line.split_whitespace().map(str::parse::<u64>).collect();
            values.ok().filter(|values| values.len() == 18)
        })
        .collect()
}

#[test]
fn free_uses_available_memory_totals_and_human_rounding_with_a_fixed_proc_snapshot() {
    let fixture = memory_fixture();
    let Some(output) = isolated(&fixture.0, "memory", "free", &["-t"]) else {
        return;
    };
    let output = text(&output);
    assert_eq!(
        labelled(output, "Mem:"),
        ["65536", "16384", "2048", "1024", "7144", "49152"]
    );
    assert_eq!(labelled(output, "Total:"), ["73728", "17408", "9216"]);
    let Some(output) = isolated(&fixture.0, "memory", "free", &["-h", "-w"]) else {
        return;
    };
    assert_eq!(
        labelled(text(&output), "Mem:"),
        ["64Mi", "16Mi", "2.0Mi", "1.0Mi", "1.0Mi", "6.0Mi", "48Mi"]
    );
}

#[test]
fn top_used_memory_is_total_minus_available_in_the_same_fixed_snapshot() {
    let fixture = memory_fixture();
    let pid = std::process::id().to_string();
    fs::write(fixture.0.join("process-pid"), &pid).unwrap();
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let status: String = status
        .lines()
        .map(|line| {
            if line.starts_with("RssFile:") {
                "RssFile:\t8872 kB\n".to_owned()
            } else if line.starts_with("RssShmem:") {
                "RssShmem:\t128 kB\n".to_owned()
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    fs::write(fixture.0.join("status"), status).unwrap();
    let Some(output) = isolated(&fixture.0, "memory", "top", &["-b", "-n", "1", "-p", &pid]) else {
        return;
    };
    let line = text(&output)
        .lines()
        .find(|line| line.starts_with("MiB Mem :"))
        .unwrap();
    assert_eq!(
        line.split_whitespace()
            .filter_map(|word| word.parse::<f64>().ok())
            .collect::<Vec<_>>(),
        [64.0, 2.0, 16.0, 7.0]
    );
    let row = text(&output)
        .lines()
        .find(|line| line.split_whitespace().next() == Some(pid.as_str()))
        .unwrap();
    assert_eq!(row.split_whitespace().nth(6), Some("9000"));
}

#[test]
fn pmap_device_columns_are_hexadecimal_even_when_the_host_device_numbers_are_small() {
    let fixture = memory_fixture();
    let pid = std::process::id().to_string();
    fs::write(fixture.0.join("process-pid"), &pid).unwrap();
    fs::write(
        fixture.0.join("maps"),
        "00001000-00002000 r--p 00000000 ab:21 42 /fixture/map\n",
    )
    .unwrap();
    let Some(output) = isolated(&fixture.0, "memory", "pmap", &["-d", "-p", &pid]) else {
        return;
    };
    let row = text(&output)
        .lines()
        .find(|line| line.contains("/fixture/map"))
        .unwrap();
    assert_eq!(row.split_whitespace().nth(4), Some("0ab:00021"));
}

#[test]
fn vmstat_includes_reclaimable_cache_scales_swap_rates_and_emits_headings_once() {
    let fixture = memory_fixture();
    // SAFETY: sysconf only reads a process-independent system parameter.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    assert!(page_size > 0);
    for (unit, divisor) in [("K", 1024_u64), ("M", 1048576), ("k", 1000), ("m", 1000000)] {
        let Some(output) = isolated(&fixture.0, "memory", "vmstat", &["-S", unit]) else {
            return;
        };
        let output = text(&output);
        let rows = vm_rows(output);
        assert_eq!(rows.len(), 1, "{output}");
        assert_eq!(rows[0][5], 6144 * 1024 / divisor, "{output}");
        assert_eq!(
            &rows[0][6..8],
            &[512 * page_size / divisor, 1024 * page_size / divisor],
            "{output}"
        );
        assert!(
            output.contains("procs -----------memory----------"),
            "{output}"
        );
        assert!(
            output
                .lines()
                .any(|line| line.split_whitespace().collect::<Vec<_>>()
                    == [
                        "r", "b", "swpd", "free", "buff", "cache", "si", "so", "bi", "bo", "in",
                        "cs", "us", "sy", "id", "wa", "st", "gu"
                    ])
        );
    }
    let Some(output) = isolated(&fixture.0, "memory", "vmstat", &["-s"]) else {
        return;
    };
    assert!(text(&output).lines().any(
        |line| line.split_whitespace().collect::<Vec<_>>() == ["16384", "K", "used", "memory"]
    ));
    assert!(
        text(&output)
            .lines()
            .any(|line| line.split_whitespace().collect::<Vec<_>>()
                == ["6144", "K", "swap", "cache"])
    );
    let Some(output) = isolated(&fixture.0, "memory", "vmstat", &["1", "2"]) else {
        return;
    };
    assert_eq!(
        text(&output)
            .matches("procs -----------memory----------")
            .count(),
        1
    );
    assert_eq!(vm_rows(text(&output)).len(), 2);
}

#[test]
fn w_missing_utmp_is_an_empty_successful_session_table() {
    let fixture = memory_fixture();
    let Some(output) = isolated(&fixture.0, "utmp", "w", &["-h"]) else {
        return;
    };
    assert_eq!(text(&output), "");
    for args in [&[][..], &["-s", "-f"][..], &["ax_absent_user"][..]] {
        let Some(output) = isolated(&fixture.0, "utmp", "w", args) else {
            return;
        };
        assert!(text(&output).contains("0 users"), "{output:?}");
        assert_eq!(
            text(&output)
                .lines()
                .filter(|line| line.starts_with("USER"))
                .count(),
            1
        );
    }
}

#[test]
fn w_preserves_non_missing_utmp_read_errors() {
    if !Path::new("/var/run").is_dir() {
        return;
    }
    let fixture = memory_fixture();
    fs::create_dir(fixture.0.join("empty-run/utmp")).unwrap();
    let Some(child) = namespace_child(&fixture.0, "utmp", "w", &["-h"]) else {
        return;
    };
    let output = child.output();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("w: cannot read /var/run/utmp"));
}

fn nonblocking(fd: &impl AsRawFd) {
    // SAFETY: fcntl reads and updates flags of this live, owned descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: O_NONBLOCK does not change descriptor ownership or memory.
    assert_eq!(
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
}

fn drain(reader: &mut impl Read, output: &mut Vec<u8>) {
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => output.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("read child output: {error}"),
        }
    }
}

fn feed_vmstat_fifo(root: &Path, child: &mut OwnedChild, input: u64, output: u64) {
    let deadline = Instant::now() + WAIT;
    let mut writer = loop {
        match File::options()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(root.join("vmstat"))
        {
            Ok(file) => break file,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                assert!(
                    child.child().try_wait().unwrap().is_none(),
                    "vmstat exited before reading its snapshot"
                );
                assert!(
                    Instant::now() < deadline,
                    "vmstat did not read its snapshot"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("open vmstat fixture writer: {error}"),
        }
    };
    writer
        .write_all(
            format!("pswpin {input}\npswpout {output}\npgpgin 3000\npgpgout 4000\n").as_bytes(),
        )
        .unwrap();
}

#[test]
fn vmstat_interval_swap_rates_use_the_selected_unit_before_integer_truncation() {
    let fixture = memory_fixture();
    fs::remove_file(fixture.0.join("vmstat")).unwrap();
    let fifo = CString::new(fixture.0.join("vmstat").as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is inside this test's private scratch directory;
    // it is absent, NUL-terminated, and the mode grants only owner access.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let Some(mut child) = namespace_child(&fixture.0, "memory", "vmstat", &["-S", "M", "1", "2"])
    else {
        return;
    };
    feed_vmstat_fifo(&fixture.0, &mut child, 10240, 20480);
    let mut stdout = child.child().stdout.take().unwrap();
    nonblocking(&stdout);
    let mut bytes = Vec::new();
    let deadline = Instant::now() + WAIT;
    loop {
        drain(&mut stdout, &mut bytes);
        if !vm_rows(&String::from_utf8_lossy(&bytes)).is_empty() {
            break;
        }
        if child.child().try_wait().unwrap().is_some() {
            let output = child.output();
            panic!("vmstat exited before its first sample: {output:?}");
        }
        assert!(
            Instant::now() < deadline,
            "vmstat did not print its first sample"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: sysconf only reads a system parameter.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let pages = 4 * 1048576 / page_size;
    feed_vmstat_fifo(&fixture.0, &mut child, 10240 + pages, 20480 + 2 * pages);
    assert!(child.wait_bounded().success());
    drain(&mut stdout, &mut bytes);
    let output = String::from_utf8(bytes).unwrap();
    let rows = vm_rows(&output);
    assert_eq!(rows.len(), 2, "{output}");
    assert_eq!(&rows[1][6..8], &[4, 8], "{output}");
}

struct PtySession {
    child: OwnedChild,
    master: File,
    slave: File,
    original: Termios,
    bytes: Vec<u8>,
}

impl PtySession {
    fn watch(args: &[&OsStr]) -> Self {
        Self::watch_env(args, None)
    }

    fn watch_env(args: &[&OsStr], extra: Option<(&str, &Path)>) -> Self {
        let pair = openpty(
            Some(&Winsize {
                ws_row: 24,
                ws_col: 160,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let master = File::from(pair.master);
        let slave = File::from(pair.slave);
        let original = tcgetattr(&slave).unwrap();
        nonblocking(&master);
        let mut command = applet("watch");
        command
            .args(args)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        if let Some((name, path)) = extra {
            command.env(name, path);
        }
        // SAFETY: after fork, the closure uses only async-signal-safe libc
        // session/ioctl calls and constructs errno values before exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            child: OwnedChild::new(command.spawn().unwrap()),
            master,
            slave,
            original,
            bytes: Vec::new(),
        }
    }

    fn wait_for(&mut self, needle: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            drain(&mut self.master, &mut self.bytes);
            if String::from_utf8_lossy(&self.bytes).contains(needle) {
                return;
            }
            assert!(
                self.child.child().try_wait().unwrap().is_none(),
                "watch exited early: {}",
                String::from_utf8_lossy(&self.bytes)
            );
            assert!(
                Instant::now() < deadline,
                "watch did not show {needle:?}: {}",
                String::from_utf8_lossy(&self.bytes)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn signal(&mut self, signal: i32) {
        assert!(self.child.child().try_wait().unwrap().is_none());
        // SAFETY: this unreaped Child is owned by the test, so even if it
        // exits before kill, its PID cannot have been reused.
        assert_eq!(
            unsafe { libc::kill(self.child.child().id() as i32, signal) },
            0
        );
    }

    fn finish(&mut self, code: i32) {
        let status = self.child.wait_bounded();
        drain(&mut self.master, &mut self.bytes);
        assert_eq!(
            status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&self.bytes)
        );
        assert_eq!(
            tcgetattr(&self.slave).unwrap(),
            self.original,
            "terminal settings leaked"
        );
        assert!(
            self.bytes.windows(5).any(|value| value == b"\x1b[?7h"),
            "wrapping was not restored"
        );
    }
}

#[test]
fn watch_q_exits_during_a_long_refresh_interval_without_a_newline() {
    let args: Vec<_> = [
        "-t", "-w", "-n", "60", "-x", AXE, "--applet", "echo", "--", "AXWATCH",
    ]
    .iter()
    .map(OsStr::new)
    .collect();
    let mut terminal = PtySession::watch(&args);
    terminal.wait_for("AXWATCH");
    terminal.master.write_all(b"q").unwrap();
    terminal.finish(0);
}

#[test]
fn watch_errexit_waits_for_any_single_key_and_returns_one() {
    for key in [b"q", b"z", b"\n"] {
        let args: Vec<_> = ["-t", "-w", "-e", "-x", AXE, "--applet", "false", "--"]
            .iter()
            .map(OsStr::new)
            .collect();
        let mut terminal = PtySession::watch(&args);
        terminal.wait_for("press a key to exit");
        assert!(terminal.child.child().try_wait().unwrap().is_none());
        terminal.master.write_all(key).unwrap();
        terminal.finish(1);
    }
}

#[test]
fn watch_termination_signals_restore_the_terminal_during_refresh_and_errexit_waits() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let args: Vec<_> = [
            "-t", "-w", "-n", "60", "-x", AXE, "--applet", "echo", "--", "AXWATCH",
        ]
        .iter()
        .map(OsStr::new)
        .collect();
        let mut terminal = PtySession::watch(&args);
        terminal.wait_for("AXWATCH");
        terminal.signal(signal);
        terminal.finish(0);

        let args: Vec<_> = ["-t", "-w", "-e", "-x", AXE, "--applet", "false", "--"]
            .iter()
            .map(OsStr::new)
            .collect();
        let mut terminal = PtySession::watch(&args);
        terminal.wait_for("press a key to exit");
        terminal.signal(signal);
        terminal.finish(0);
    }
}

#[test]
fn watch_no_wrap_clips_by_terminal_cells_without_splitting_utf8_or_graphemes() {
    let narrow = format!("{}é", "a".repeat(159));
    let wide = format!("{}界", "b".repeat(159));
    let combining = format!("{}e\u{301}X", "c".repeat(159));
    let emoji = format!("{}👩‍💻Z", "d".repeat(158));
    let payload = format!("{narrow}\n{wide}\n{combining}\n{emoji}");
    let args: Vec<_> = [
        "-t",
        "-w",
        "-q",
        "1",
        "-n",
        "0.1",
        "-x",
        AXE,
        "--applet",
        "echo",
        "--",
        payload.as_str(),
    ]
    .into_iter()
    .map(OsStr::new)
    .collect();
    let mut terminal = PtySession::watch(&args);
    terminal.finish(0);
    let output = std::str::from_utf8(&terminal.bytes).unwrap();
    assert!(output.contains(&format!("{narrow}\r\n")), "{output}");
    assert!(
        output.contains(&format!("{}\r\n", "b".repeat(159))),
        "{output}"
    );
    assert!(
        output.contains(&format!("{}e\u{301}\r\n", "c".repeat(159))),
        "{output}"
    );
    assert!(
        output.contains(&format!("{}👩‍💻\r\n", "d".repeat(158))),
        "{output}"
    );
    assert!(!output.contains('界'));
    assert!(!output.contains('X'));
    assert!(!output.contains('Z'));
}

fn process_identity(pid: u32) -> Option<(u32, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(") ")?;
    let mut fields = fields.split_whitespace();
    let parent = fields.nth(1)?.parse().ok()?;
    let start = fields.nth(17)?.parse().ok()?;
    Some((parent, start))
}

struct OwnedDescendant {
    pid: u32,
    parent: u32,
    start: u64,
}

impl Drop for OwnedDescendant {
    fn drop(&mut self) {
        if process_identity(self.pid) == Some((self.parent, self.start)) {
            // SAFETY: parent PID and start time identify the still-owned
            // fixture, not an unrelated process that reused its PID.
            unsafe { libc::kill(self.pid as i32, libc::SIGKILL) };
            let deadline = Instant::now() + WAIT;
            while process_identity(self.pid) == Some((self.parent, self.start))
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
fn watch_busy_fixture_child() {
    let Some(root) = std::env::var_os("AXE_WATCH_BUSY_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    fs::write(root.join("ready.tmp"), std::process::id().to_string()).unwrap();
    fs::rename(root.join("ready.tmp"), root.join("ready")).unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

struct RunningWatch {
    // Terminate an outstanding command before dropping its owning watch.
    owned: OwnedDescendant,
    terminal: PtySession,
    _scratch: Scratch,
}

impl RunningWatch {
    fn new() -> Self {
        let fixture = Scratch::new("watch-running");
        let executable = std::env::current_exe().unwrap();
        let args = [
            OsStr::new("-t"),
            OsStr::new("-w"),
            OsStr::new("-x"),
            executable.as_os_str(),
            OsStr::new("--exact"),
            OsStr::new("watch_busy_fixture_child"),
            OsStr::new("--nocapture"),
        ];
        let mut terminal = PtySession::watch_env(&args, Some(("AXE_WATCH_BUSY_ROOT", &fixture.0)));
        let deadline = Instant::now() + WAIT;
        while !fixture.0.join("ready").exists() {
            assert!(terminal.child.child().try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "watch did not start its command");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: u32 = fs::read_to_string(fixture.0.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        let parent = terminal.child.child().id();
        let (actual_parent, start) = process_identity(pid).unwrap();
        assert_eq!(actual_parent, parent);
        Self {
            owned: OwnedDescendant { pid, parent, start },
            terminal,
            _scratch: fixture,
        }
    }
}

#[test]
fn watch_q_interrupts_and_reaps_its_running_command() {
    let mut fixture = RunningWatch::new();
    let start = Instant::now();
    fixture.terminal.master.write_all(b"q").unwrap();
    fixture.terminal.finish(0);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(
        process_identity(fixture.owned.pid).is_none(),
        "watch left its command unreaped"
    );
}

#[test]
fn watch_termination_signals_cancel_and_reap_the_running_command_before_terminal_cleanup() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let mut fixture = RunningWatch::new();
        fixture.terminal.signal(signal);
        fixture.terminal.finish(0);
        assert!(
            process_identity(fixture.owned.pid).is_none(),
            "watch left its command unreaped"
        );
    }
}

#[test]
fn supported_procps_usage_errors_have_command_specific_statuses() {
    for name in [
        "free", "top", "vmstat", "watch", "pmap", "pwdx", "w", "skill", "snice", "tload",
        "hugetop", "slabtop", "killall",
    ] {
        let output = run(name, &["--axe-invalid-option"]);
        assert_eq!(output.status.code(), Some(1), "{name}: {output:?}");
        assert!(!output.stderr.is_empty(), "{name}: {output:?}");
    }
    for name in ["pgrep", "pkill", "sysctl"] {
        let output = run(name, &["--axe-invalid-option"]);
        assert_eq!(output.status.code(), Some(2), "{name}: {output:?}");
    }
    let output = run("pwdx", &["not-a-pid"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

const KERNEL_STAT: &str = "cpu 100 0 100 700 100 0 0 0 0 0\ncpu0 100 0 100 700 100 0 0 0 0\nintr 10000\nctxt 20000\nbtime 1000\nprocesses 100\nprocs_running 1\nprocs_blocked 108\n";

#[test]
fn vmstat_summary_preserves_page_allocation_records_and_fixed_numeric_fields() {
    let fixture = memory_fixture();
    fs::write(fixture.0.join("kernel-stat"), KERNEL_STAT).unwrap();
    fs::write(fixture.0.join("vmstat"),
        "pswpin 10240\npswpout 20480\npgpgin 3000\npgpgout 4000\npgalloc_dma 1025\npgalloc_dma32 24249657\npgalloc_normal 10664744640\npgfree 12345678901234\n").unwrap();
    for args in [&["-s"][..], &["-s", "-S", "M"][..]] {
        let Some(output) = isolated(&fixture.0, "memory", "vmstat", args) else {
            return;
        };
        let output = text(&output);
        let records: Vec<_> = output
            .lines()
            .map(|line| {
                let numeric = line.trim_start().split_once(' ').unwrap().0;
                let value = numeric.parse::<u64>().unwrap();
                let width = numeric.len().max(13);
                assert_eq!(line[..width].trim(), numeric, "{line:?}");
                assert_eq!(line.as_bytes()[width], b' ', "{line:?}");
                (value, &line[width + 1..])
            })
            .collect();
        assert_eq!(records.len(), 34, "{output}");
        assert_eq!(
            &records[24..30],
            &[
                (1025, "pages alloc in dma"),
                (24249657, "pages alloc in dma32"),
                (0, "pages alloc in high"),
                (0, "pages alloc in movable"),
                (10664744640, "pages alloc in normal"),
                (12345678901234, "pages free"),
            ]
        );
        assert_eq!(
            &records[20..24],
            &[
                (3000, "K paged in"),
                (4000, "K paged out"),
                (10240, "pages swapped in"),
                (20480, "pages swapped out"),
            ]
        );
    }
}

#[test]
fn vmstat_narrow_columns_recover_padding_after_overflow_without_losing_counter_values() {
    let fixture = memory_fixture();
    fs::write(fixture.0.join("kernel-stat"), KERNEL_STAT).unwrap();
    fs::write(
        fixture.0.join("meminfo"),
        MEMINFO
            .replace("MemTotal: 65536", "MemTotal: 65520092")
            .replace("MemFree: 2048", "MemFree: 6932916")
            .replace("Buffers: 1000", "Buffers: 36")
            .replace("Cached: 4096", "Cached: 30585548")
            .replace("SReclaimable: 2048", "SReclaimable: 1722384")
            .replace("SwapTotal: 8192", "SwapTotal: 33554428")
            .replace("SwapFree: 7168", "SwapFree: 17772"),
    )
    .unwrap();
    write_vmstat(&fixture.0, 0, 0);
    let Some(output) = isolated(&fixture.0, "memory", "vmstat", &[]) else {
        return;
    };
    let output = text(&output);
    let data = output.lines().nth(2).unwrap();
    assert_eq!(
        data,
        " 1 108 33536656 6932916 36 32307932  0    0   150   200  500 1000 10 10 70 10  0  0"
    );
    assert_eq!(
        vm_rows(output),
        [vec![
            1, 108, 33536656, 6932916, 36, 32307932, 0, 0, 150, 200, 500, 1000, 10, 10, 70, 10, 0,
            0,
        ]]
    );
}

fn write_process_stat(
    root: &Path,
    pid: &str,
    state: char,
    virtual_kib: u64,
    resident_kib: u64,
    stack: u64,
) {
    let original = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let (identity, fields) = original.rsplit_once(") ").unwrap();
    let mut fields: Vec<_> = fields.split_whitespace().map(str::to_owned).collect();
    // SAFETY: sysconf only reads the system page size.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    assert_eq!(resident_kib * 1024 % page_size, 0);
    fields[0] = state.to_string();
    fields[20] = (virtual_kib * 1024).to_string();
    fields[21] = (resident_kib * 1024 / page_size).to_string();
    fields[25] = stack.to_string();
    fs::write(root.join("process-pid"), pid).unwrap();
    fs::write(
        root.join("stat"),
        format!("{identity}) {}\n", fields.join(" ")),
    )
    .unwrap();
}

#[test]
fn top_batch_columns_preserve_fitting_kib_values_and_mark_truncated_users_and_disk_sleep() {
    use std::fmt::Write as _;

    let fixture = memory_fixture();
    let pid = std::process::id().to_string();
    fs::write(fixture.0.join("kernel-stat"), KERNEL_STAT).unwrap();
    fs::write(
        fixture.0.join("meminfo"),
        MEMINFO.replace("MemTotal: 65536", "MemTotal: 16777216"),
    )
    .unwrap();
    fs::write(
        fixture.0.join("passwd"),
        b"robot-bgl:x:0:0:fixture:/nonexistent:/nonexistent\n",
    )
    .unwrap();
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    for (state, virt, res, expected_virt, expected_res, expected_mem, sleep, disk_sleep) in [
        ('S', 99516, 16384, "99516", "16384", 0.1, 1, 0),
        ('D', 10000000, 1024000, "9765.6m", "1.0g", 6.1, 0, 1),
    ] {
        write_process_stat(&fixture.0, &pid, state, virt, res / 2, 0x7800);
        let mut process_status = String::with_capacity(status.len());
        for line in status.lines() {
            match line.split_once(':') {
                Some(("Uid", _)) => process_status.push_str("Uid:\t0\t0\t0\t0\n"),
                Some(("Gid", _)) => process_status.push_str("Gid:\t0\t0\t0\t0\n"),
                Some((field @ ("VmRSS" | "RssAnon"), _)) => {
                    writeln!(process_status, "{field}:\t{res} kB").unwrap();
                }
                Some((field @ ("RssFile" | "RssShmem"), _)) => {
                    writeln!(process_status, "{field}:\t0 kB").unwrap();
                }
                _ => {
                    process_status.push_str(line);
                    process_status.push('\n');
                }
            }
        }
        fs::write(fixture.0.join("status"), process_status).unwrap();
        let Some(output) = isolated(&fixture.0, "memory", "top", &["-b", "-n", "1", "-p", &pid])
        else {
            return;
        };
        let output = text(&output);
        let row = output
            .lines()
            .find(|line| line.split_whitespace().next() == Some(pid.as_str()))
            .unwrap();
        assert_eq!(&row[8..16], "robot-b+");
        assert_eq!(row[25..32].trim(), expected_virt);
        assert_eq!(row[33..39].trim(), expected_res);
        assert_eq!(
            row.split_whitespace()
                .nth(9)
                .unwrap()
                .parse::<f64>()
                .unwrap(),
            expected_mem
        );
        assert_eq!(&row[47..48], state.to_string());
        let tasks = output
            .lines()
            .find(|line| line.starts_with("Tasks:"))
            .unwrap();
        let task_counts = tasks
            .split(|character: char| !character.is_ascii_digit())
            .filter(|value| !value.is_empty())
            .map(|value| value.parse::<u64>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(task_counts, [1, 0, sleep, disk_sleep, 0, 0]);
        let memory = output
            .lines()
            .find(|line| line.starts_with("MiB Mem :"))
            .unwrap();
        assert_eq!(memory[10..18].trim(), "16384.0");
    }
}

#[test]
fn pmap_mapping_columns_distinguish_files_deleted_backing_and_kernel_names_in_all_formats() {
    let fixture = memory_fixture();
    let pid = std::process::id().to_string();
    write_process_stat(&fixture.0, &pid, 'S', 65536, 16384, 0x7800);
    let paths = [
        "/fixture/normal.so (deleted)",
        "",
        "[vdso]",
        "[vvar]",
        "[vvar_vclock]",
        "[heap]",
        "[stack]",
        "[anon:private name]",
        "/memfd:buffer (deleted)",
        "/SYSV00000001 (deleted)",
        "[anon:folder/name]",
    ];
    let mut maps = String::new();
    let mut smaps = String::new();
    for (index, path) in paths.iter().enumerate() {
        let start = (index + 1) * 0x1000;
        let line = format!(
            "{start:016x}-{:016x} rw-p 00000000 00:01 173 {path}\n",
            start + 0x1000
        );
        maps.push_str(&line);
        smaps.push_str(&line);
        smaps.push_str("Rss: 4 kB\nPrivate_Dirty: 1 kB\nShared_Dirty: 0 kB\n");
    }
    fs::write(fixture.0.join("maps"), maps).unwrap();
    fs::write(fixture.0.join("smaps"), smaps).unwrap();
    let expected = [
        "normal.so (deleted)",
        "  [ anon ]",
        "  [ anon ]",
        "  [ anon ]",
        "  [ anon ]",
        "  [ anon ]",
        "  [ stack ]",
        "  [ anon ]",
        "memfd:buffer (deleted)",
        "  [ shmid=0xad ]",
        "name]",
    ];
    for (options, offset) in [
        (&[][..], std::mem::size_of::<usize>() * 2 + 15),
        (&["-d"][..], std::mem::size_of::<usize>() * 2 + 42),
        (&["-x"][..], std::mem::size_of::<usize>() * 2 + 31),
    ] {
        let mut args = options.to_vec();
        args.push(&pid);
        let Some(output) = isolated(&fixture.0, "memory", "pmap", &args) else {
            return;
        };
        let output = text(&output);
        let rows: Vec<_> = output
            .lines()
            .filter(|line| {
                line.split_whitespace().next().is_some_and(|word| {
                    word.len() == std::mem::size_of::<usize>() * 2
                        && word.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
            .collect();
        assert_eq!(rows.len(), paths.len(), "{output}");
        for (row, name) in rows.iter().zip(expected) {
            assert_eq!(&row[offset..], name, "{row}");
        }
        if options == ["-x"] {
            let separator = format!(
                "\n{} ------- ------- ------- \n",
                "-".repeat(std::mem::size_of::<usize>() * 2)
            );
            let (_, totals) = output.split_once(&separator).unwrap();
            assert_eq!(labelled(totals, "total kB"), ["44", "44", "11"]);
        }
    }
    let Some(output) = isolated(&fixture.0, "memory", "pmap", &["-k", "-p", &pid]) else {
        return;
    };
    let names: Vec<_> = text(&output)
        .lines()
        .filter(|line| line.starts_with("0000"))
        .map(|line| &line[std::mem::size_of::<usize>() * 2 + 15..])
        .collect();
    assert_eq!(
        names,
        [
            "/fixture/normal.so (deleted)",
            "  [ anon ]",
            "[vdso]",
            "[vvar]",
            "[vvar_vclock]",
            "[heap]",
            "[stack]",
            "[anon:private name]",
            "/memfd:buffer (deleted)",
            "  [ shmid=0xad ]",
            "/name]",
        ]
    );
}

fn write_utmp_session(path: &Path, pid: i32) {
    let mut storage = std::mem::MaybeUninit::<libc::utmpx>::uninit();
    let size = std::mem::size_of::<libc::utmpx>();
    // SAFETY: the aligned utmpx storage is writable for size bytes. All-zero
    // values are valid for its integer/character fields; padding is also
    // initialized in place so the complete on-disk byte view is readable.
    unsafe { std::ptr::write_bytes(storage.as_mut_ptr().cast::<u8>(), 0, size) };
    // SAFETY: every field was initialized above; the struct is never moved
    // out of storage, preserving the explicitly initialized padding.
    let entry = unsafe { &mut *storage.as_mut_ptr() };
    entry.ut_type = libc::USER_PROCESS;
    entry.ut_pid = pid;
    for (slot, byte) in entry.ut_user.iter_mut().zip(b"layoutuser") {
        *slot = *byte as libc::c_char;
    }
    for (slot, byte) in entry.ut_line.iter_mut().zip(b"pts/2147483647") {
        *slot = *byte as libc::c_char;
    }
    for (slot, byte) in entry.ut_host.iter_mut().zip(b"example.test") {
        *slot = *byte as libc::c_char;
    }
    // SAFETY: the storage remains alive and every byte, including padding,
    // was initialized before the scalar field updates.
    let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), size) };
    fs::write(path, bytes).unwrap();
}

#[test]
fn w_supported_layouts_keep_session_values_in_their_fixed_columns() {
    if !Path::new("/var/run").is_dir() {
        return;
    }
    let fixture = memory_fixture();
    let mut process = ProcessFixture::new();
    write_utmp_session(
        &fixture.0.join("empty-run/utmp"),
        process.pid().parse().unwrap(),
    );
    for (args, from, short) in [
        (&[][..], false, false),
        (&["-f"][..], true, false),
        (&["-s"][..], false, true),
        (&["-s", "-f"][..], true, true),
    ] {
        let Some(output) = isolated(&fixture.0, "utmp", "w", args) else {
            return;
        };
        let output = text(&output);
        let header = output
            .lines()
            .find(|line| line.starts_with("USER"))
            .unwrap();
        let row = output
            .lines()
            .find(|line| line.starts_with("layoutus"))
            .unwrap();
        assert_eq!(&row[..8], "layoutus");
        assert_eq!(&row[9..17], "pts/2147");
        assert_eq!(header.find("TTY"), Some(9));
        let mut position = 18;
        if from {
            assert_eq!(&row[position..position + 16], "example.test    ");
            assert_eq!(header.find("FROM"), Some(position));
            position += 16;
        }
        if !short {
            assert_eq!(&row[position..position + 8], " 01Jan70");
            assert_eq!(header.find("LOGIN@"), Some(position + 1));
            position += 8;
        }
        assert_eq!(header.find("IDLE"), Some(position + 2 + usize::from(short)));
        position += 7;
        if !short {
            assert_eq!(header.find("JCPU"), Some(position + 2));
            assert!(row[position..position + 7].trim().ends_with('s'));
            position += 7;
            assert_eq!(header.find("PCPU"), Some(position + 2));
            assert!(row[position..position + 7].trim().ends_with('s'));
            position += 7;
        }
        assert_eq!(header.find("WHAT"), Some(position + 1));
        assert!(
            row[position + 1..].contains("process_fixture_child"),
            "{row}"
        );
    }
}
