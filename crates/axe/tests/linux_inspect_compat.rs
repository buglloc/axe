#![cfg(target_os = "linux")]

use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::net::{TcpListener, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-linux-inspect-{}-{}-{}",
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

    fn file(&self, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn axe(applet: &str, args: &[&str], paths: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", applet, "--"])
        .args(args)
        .args(paths)
        // The private device fixture hides /dev/null; keep the caller's open stdin.
        .stdin(std::process::Stdio::inherit())
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("AXE_STORE_MODE", "off")
        .env_remove("S_TIME_FORMAT")
        .env_remove("S_TIME_DEF_TIME")
        .output()
        .expect("run inspection applet")
}

fn success(output: &Output) -> &str {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    std::str::from_utf8(&output.stdout).unwrap()
}

#[test]
fn lsof_pid_selection_file_identity_intersections_and_no_match() {
    let scratch = Scratch::new();
    let held = scratch.file("held", b"held open\n");
    let hardlink = scratch.0.join("hardlink");
    let symbolic = scratch.0.join("symbolic");
    fs::hard_link(&held, &hardlink).unwrap();
    symlink(&held, &symbolic).unwrap();
    let file = File::open(&held).unwrap();
    let pid = std::process::id().to_string();
    let output = axe("lsof", &["-nP", "-p", &pid], &[]);
    let rows = success(&output).lines().skip(1).collect::<Vec<_>>();
    assert!(!rows.is_empty());
    for row in rows {
        assert_eq!(row.split_whitespace().nth(1), Some(pid.as_str()), "{row}");
    }
    for path in [&held, &hardlink, &symbolic] {
        let output = axe("lsof", &["-t", "-a", "-p", &pid], &[path]);
        assert_eq!(success(&output), format!("{pid}\n"));
        let output = axe("lsof", &["-t", "-p", "4294967295"], &[path]);
        assert_eq!(success(&output), format!("{pid}\n"));
    }
    let command = fs::read_to_string("/proc/self/comm").unwrap();
    let output = axe("lsof", &["-a", "-p", &pid, "-Fpcfnt"], &[&symbolic]);
    assert_eq!(
        success(&output),
        format!(
            "p{pid}\nc{}\nf{}\ntREG\nn{}\n",
            command.trim(),
            file.as_raw_fd(),
            held.display()
        ),
    );
    let separate = axe("lsof", &["-a", "-p", &pid, "-F", "pcfnt"], &[&symbolic]);
    assert_eq!(success(&separate), success(&output));
    let bare = axe("lsof", &["-a", "-p", &pid, "-F", "--"], &[&held]);
    assert!(success(&bare).contains(&format!("n{}\n", held.display())));
    assert!(
        success(&axe("lsof", &["-a", "-p", &pid, "-F"], &[])).starts_with(&format!("p{pid}\n"))
    );
    let output = axe("lsof", &["-a", "-p", &pid, "-Fpcfnt0"], &[&hardlink]);
    assert_eq!(
        success(&output),
        format!(
            "p{pid}\0c{}\0\nf{}\0tREG\0n{}\0\n",
            command.trim(),
            file.as_raw_fd(),
            held.display()
        )
    );
    let output = axe("lsof", &["-a", "-p", &pid, "-Fp0"], &[&held]);
    assert_eq!(success(&output), format!("p{pid}\0\n"));
    let missing = scratch.0.join("missing");
    for (args, paths) in [
        (vec!["-t", "-p", "4294967295"], vec![]),
        (vec!["-t", "-a", "-p", "4294967295"], vec![held.as_path()]),
        (
            vec!["-t", "-a", "-p", &pid, "-c", "axe-absent-command"],
            vec![],
        ),
        (vec!["-t", "-a", "-p", &pid], vec![missing.as_path()]),
    ] {
        let output = axe("lsof", &args, &paths);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    let output = axe("lsof", &["-t", "-p", &pid, "-c", "axe-absent-command"], &[]);
    assert_eq!(success(&output), format!("{pid}\n"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let filter = format!("-iTCP:{}", listener.local_addr().unwrap().port());
    let output = axe("lsof", &["-nP", "-t", "-a", "-p", &pid, &filter], &[]);
    assert_eq!(success(&output), format!("{pid}\n"));
    let output = axe("lsof", &["-nP", "-t", "-a", "-p", &pid, &filter], &[&held]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    let null = File::open("/dev/null").unwrap();
    let output = axe(
        "lsof",
        &["-a", "-p", &pid, "-Fft"],
        &[Path::new("/dev/null")],
    );
    assert!(success(&output).contains(&format!("f{}\ntCHR\n", null.as_raw_fd())));
}

fn descriptor_row(text: &str, fd: i32, access: char) -> Vec<&str> {
    let descriptor = format!("{fd}{access}");
    text.lines()
        .skip(1)
        .find(|line| line.split_whitespace().nth(3) == Some(descriptor.as_str()))
        .unwrap_or_else(|| panic!("missing descriptor {descriptor}: {text}"))
        .split_whitespace()
        .collect()
}

fn descriptor_fields(text: &str, fd: i32) -> Vec<&str> {
    let descriptor = format!("f{fd}");
    let mut lines = text.lines().skip_while(|line| *line != descriptor.as_str());
    assert_eq!(lines.next(), Some(descriptor.as_str()), "{text}");
    lines
        .take_while(|line| !line.starts_with('f') && !line.starts_with('p'))
        .collect()
}

#[test]
fn lsof_descriptor_types_keep_access_device_offset_and_protocol_meanings() {
    let scratch = Scratch::new();
    let held = scratch.file("descriptor-data", b"ninebytes");
    let mut read = File::open(&held).unwrap();
    read.seek(SeekFrom::Start(3)).unwrap();
    let write = File::options().write(true).open(&held).unwrap();
    let both = File::options().read(true).write(true).open(&held).unwrap();
    let null = File::open("/dev/null").unwrap();
    let (read_pipe, write_pipe) = std::io::pipe().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let datagram = UdpSocket::bind("127.0.0.1:0").unwrap();
    let pid = std::process::id().to_string();
    let output = axe("lsof", &["-nP", "-a", "-p", &pid], &[]);
    let text = success(&output);
    for (fd, access) in [
        (read.as_raw_fd(), 'r'),
        (write.as_raw_fd(), 'w'),
        (both.as_raw_fd(), 'u'),
    ] {
        let row = descriptor_row(text, fd, access);
        assert_eq!(row[4], "REG");
        assert_eq!(row[6], "9");
        assert_eq!(row[7], read.metadata().unwrap().ino().to_string());
    }
    let null_metadata = null.metadata().unwrap();
    let row = descriptor_row(text, null.as_raw_fd(), 'r');
    assert_eq!(row[4], "CHR");
    assert_eq!(
        row[5],
        format!(
            "{},{}",
            libc::major(null_metadata.rdev()),
            libc::minor(null_metadata.rdev())
        )
    );
    assert_eq!(row[6], "0t0");
    assert_eq!(row[7], null_metadata.ino().to_string());
    for (fd, access) in [(read_pipe.as_raw_fd(), 'r'), (write_pipe.as_raw_fd(), 'w')] {
        let row = descriptor_row(text, fd, access);
        assert_eq!(row[4], "FIFO");
        assert_eq!(row[6], "0t0");
        assert_eq!(row[8], "pipe");
    }
    let socket_inode = fs::metadata(format!("/proc/self/fd/{}", listener.as_raw_fd()))
        .unwrap()
        .ino();
    let address = listener.local_addr().unwrap().to_string();
    let row = descriptor_row(text, listener.as_raw_fd(), 'u');
    assert_eq!(
        &row[4..],
        &[
            "IPv4",
            &socket_inode.to_string(),
            "0t0",
            "TCP",
            &address,
            "(LISTEN)"
        ]
    );
    let datagram_inode = fs::metadata(format!("/proc/self/fd/{}", datagram.as_raw_fd()))
        .unwrap()
        .ino();
    let row = descriptor_row(text, datagram.as_raw_fd(), 'u');
    assert_eq!(
        &row[4..],
        &[
            "IPv4",
            &datagram_inode.to_string(),
            "0t0",
            "UDP",
            &datagram.local_addr().unwrap().to_string()
        ]
    );
    let output = axe("lsof", &["-nP", "-a", "-p", &pid, "-FfatrDsoiPn"], &[]);
    let text = success(&output);
    let fields = descriptor_fields(text, read.as_raw_fd());
    assert!(
        fields.contains(&"ar")
            && fields.contains(&"tREG")
            && fields.contains(&"s9")
            && fields.contains(&"o0t3"),
        "{fields:?}"
    );
    let fields = descriptor_fields(text, null.as_raw_fd());
    assert!(fields.contains(&"tCHR") && fields.contains(&"o0t0"));
    assert!(
        fields.contains(&format!("r0x{:x}", null_metadata.rdev()).as_str()),
        "{fields:?}"
    );
    assert!(
        fields.contains(&format!("D0x{:x}", null_metadata.dev()).as_str()),
        "{fields:?}"
    );
    assert!(
        !fields.iter().any(|field| field.starts_with('s')),
        "{fields:?}"
    );
    let fields = descriptor_fields(text, read_pipe.as_raw_fd());
    assert!(
        fields.contains(&"ar") && fields.contains(&"tFIFO") && fields.contains(&"npipe"),
        "{fields:?}"
    );
    let fields = descriptor_fields(text, listener.as_raw_fd());
    assert!(
        fields.contains(&"au")
            && fields.contains(&"PTCP")
            && fields.contains(&format!("n{address}").as_str()),
        "{fields:?}"
    );
    assert!(
        !fields
            .iter()
            .any(|field| matches!(field.as_bytes().first(), Some(b'D' | b's' | b'i'))),
        "{fields:?}"
    );
}

#[test]
fn lsof_text_fields_escape_control_bytes_without_forging_records() {
    if std::env::var_os("AXE_INSPECT_ESCAPE_CHILD").is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "lsof_text_fields_escape_control_bytes_without_forging_records",
                "--nocapture",
            ])
            .env("AXE_INSPECT_ESCAPE_CHILD", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    let pid = std::process::id().to_string();
    fs::write(
        format!("/proc/self/task/{pid}/comm"),
        b"cmd\np999999\n\\\t\x01",
    )
    .unwrap();
    let scratch = Scratch::new();
    let held = scratch.file("held\np999999\n\\\t\r\u{c}\u{8}\u{1}\u{1b}\u{7f}", b"held");
    let file = File::open(&held).unwrap();
    let command = r"cmd\np999999\n\\\t^A";
    let filename = r"held\np999999\n\\\t\r\f\b^A^[\x7f";
    let output = axe("lsof", &["-a", "-p", &pid, "-Fpcfnt"], &[&held]);
    let lines = success(&output).lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 5, "{output:?}");
    assert_eq!(lines[0], format!("p{pid}"));
    assert_eq!(lines[1], format!("c{command}"));
    assert_eq!(lines[2], format!("f{}", file.as_raw_fd()));
    assert_eq!(lines[3], "tREG");
    assert!(
        lines[4].starts_with('n') && lines[4].ends_with(filename),
        "{output:?}"
    );
    let output = axe("lsof", &["-a", "-p", &pid, "-F", "pcfnt0"], &[&held]);
    let text = success(&output);
    assert_eq!(
        text.bytes().filter(|byte| *byte == b'\n').count(),
        2,
        "{output:?}"
    );
    assert!(text.contains(&format!("c{command}\0\n")), "{output:?}");
    assert!(text.ends_with(&format!("{filename}\0\n")), "{output:?}");
    let raw_path = scratch
        .0
        .join(OsString::from_vec(b"nonutf8-\xff\x80".to_vec()));
    fs::write(&raw_path, b"raw path").unwrap();
    let raw_file = File::open(&raw_path).unwrap();
    let output = axe("lsof", &["-a", "-p", &pid, "-Ffn"], &[]);
    let lines = success(&output).lines().collect::<Vec<_>>();
    let descriptor = format!("f{}", raw_file.as_raw_fd());
    let index = lines
        .iter()
        .position(|line| *line == descriptor.as_str())
        .unwrap();
    assert!(
        lines[index + 1].starts_with('n') && lines[index + 1].ends_with(r"nonutf8-\xff\x80"),
        "{output:?}"
    );
}

fn module_elf(metadata: &[u8]) -> Vec<u8> {
    let names = b"\0.shstrtab\0.modinfo\0";
    let data_offset = 64 + names.len();
    let section_offset = (data_offset + metadata.len() + 7) & !7;
    let mut bytes = vec![0u8; section_offset + 3 * 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&1u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[40..48].copy_from_slice(&(section_offset as u64).to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[58..60].copy_from_slice(&64u16.to_le_bytes());
    bytes[60..62].copy_from_slice(&3u16.to_le_bytes());
    bytes[62..64].copy_from_slice(&1u16.to_le_bytes());
    bytes[64..data_offset].copy_from_slice(names);
    bytes[data_offset..data_offset + metadata.len()].copy_from_slice(metadata);
    for (index, name, kind, offset, size) in [
        (1, 1u32, 3u32, 64usize, names.len()),
        (2, 11u32, 1u32, data_offset, metadata.len()),
    ] {
        let section = section_offset + index * 64;
        bytes[section..section + 4].copy_from_slice(&name.to_le_bytes());
        bytes[section + 4..section + 8].copy_from_slice(&kind.to_le_bytes());
        bytes[section + 24..section + 32].copy_from_slice(&(offset as u64).to_le_bytes());
        bytes[section + 32..section + 40].copy_from_slice(&(size as u64).to_le_bytes());
        bytes[section + 48..section + 56].copy_from_slice(&1u64.to_le_bytes());
    }
    bytes
}

#[test]
fn modinfo_preserves_metadata_order_colons_aliases_and_license_short_option() {
    let scratch = Scratch::new();
    let metadata = b"license=GPL\0author=AXE fixture\0description=Metadata\0alias=first:*\0alias=second:*\0depends=\0vermagic=fixture\0parmtype=mode:uint\0parm=mode:Access mode\0";
    let path = scratch.file("fixture.ko", module_elf(metadata));
    let output = axe("modinfo", &[], &[&path]);
    assert_eq!(
        success(&output),
        format!(
            "filename:       {}\nlicense:        GPL\nauthor:         AXE fixture\ndescription:    Metadata\nalias:          first:*\nalias:          second:*\ndepends:        \nvermagic:       fixture\nparm:           mode:Access mode (uint)\n",
            path.display()
        )
    );
    assert_eq!(success(&axe("modinfo", &["-l"], &[&path])), "GPL\n");
    assert_eq!(
        success(&axe("modinfo", &["-p"], &[&path])),
        "mode:Access mode (uint)\n"
    );
    let output = axe("modinfo", &["-0", "-F", "alias"], &[&path]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"first:*\0second:*\0");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&module_elf(metadata)).unwrap();
    let compressed = scratch.file("fixture.ko.gz", encoder.finish().unwrap());
    assert_eq!(
        success(&axe("modinfo", &["-F", "license"], &[&compressed])),
        "GPL\n"
    );
}

struct Mounts(Vec<PathBuf>);

impl Mounts {
    fn bind(&mut self, source: &Path, destination: &Path) {
        let from = CString::new(source.as_os_str().as_bytes()).unwrap();
        let to = CString::new(destination.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid C strings; the caller verified a private mount namespace.
        let result = unsafe {
            libc::mount(
                from.as_ptr(),
                to.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        };
        assert_eq!(
            result,
            0,
            "bind {source:?} to {destination:?}: {}",
            std::io::Error::last_os_error()
        );
        self.0.push(destination.to_owned());
    }
}

impl Drop for Mounts {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            let path = CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: only mounts recorded after successful private-namespace binds are detached.
            unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) };
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

struct IpcObjects {
    shm: i32,
    sem: i32,
    msg: i32,
}

impl IpcObjects {
    fn new() -> Self {
        let mut objects = Self {
            shm: -1,
            sem: -1,
            msg: -1,
        };
        // SAFETY: IPC_PRIVATE creates objects only in the already-verified new IPC namespace.
        unsafe {
            objects.shm = libc::shmget(libc::IPC_PRIVATE, 4097, 0o640);
            assert!(objects.shm >= 0, "{}", std::io::Error::last_os_error());
            objects.sem = libc::semget(libc::IPC_PRIVATE, 3, 0o640);
            assert!(objects.sem >= 0, "{}", std::io::Error::last_os_error());
            objects.msg = libc::msgget(libc::IPC_PRIVATE, 0o640);
            assert!(objects.msg >= 0, "{}", std::io::Error::last_os_error());
        }
        #[repr(C)]
        struct Message {
            kind: libc::c_long,
            bytes: [u8; 3],
        }
        let message = Message {
            kind: 1,
            bytes: *b"msg",
        };
        // SAFETY: message begins with a positive long and has three accessible payload bytes.
        let result = unsafe {
            libc::msgsnd(
                objects.msg,
                std::ptr::from_ref(&message).cast(),
                3,
                libc::IPC_NOWAIT,
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        objects
    }
}

impl Drop for IpcObjects {
    fn drop(&mut self) {
        // SAFETY: nonnegative IDs were created by this test in its verified private IPC namespace.
        unsafe {
            if self.msg >= 0 {
                libc::msgctl(self.msg, libc::IPC_RMID, std::ptr::null_mut());
            }
            if self.sem >= 0 {
                libc::semctl(self.sem, 0, libc::IPC_RMID, 0 as libc::c_long);
            }
            if self.shm >= 0 {
                libc::shmctl(self.shm, libc::IPC_RMID, std::ptr::null_mut());
            }
        }
    }
}

#[test]
#[ignore = "requires unshare and permitted unprivileged user, mount and IPC namespaces"]
fn private_proc_sysfs_and_ipc_formats() {
    let namespaces = ["mnt", "user", "ipc"];
    if std::env::var_os("AXE_INSPECT_NAMESPACE_CHILD").is_none() {
        let mut command = Command::new(unshare_program());
        command
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "--ipc",
                "--fork",
                "--",
            ])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_proc_sysfs_and_ipc_formats",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("AXE_INSPECT_NAMESPACE_CHILD", "1");
        for namespace in namespaces {
            command.env(
                format!("AXE_INSPECT_PARENT_{namespace}"),
                fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
            );
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    for namespace in namespaces {
        let parent =
            PathBuf::from(std::env::var_os(format!("AXE_INSPECT_PARENT_{namespace}")).unwrap());
        assert_ne!(
            fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
            parent,
            "namespace must be isolated before any mount/IPC writes"
        );
    }
    // SAFETY: geteuid has no pointer arguments or side effects.
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let root = CString::new("/").unwrap();
    // SAFETY: the namespace identity checks above precede the first mount mutation.
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
    let scratch = Scratch::new();
    let mut mounts = Mounts(Vec::new());
    let null = File::open("/dev/null").unwrap();
    for (name, destination, contents) in [
        (
            "stat",
            "/proc/stat",
            "cpu  100 0 50 800 0 25 25 0 0 0\ncpu0 100 0 50 800 0 25 25 0 0 0\n",
        ),
        ("uptime", "/proc/uptime", "100.00 100.00\n"),
        (
            "diskstats",
            "/proc/diskstats",
            "8 0 sda 100 0 2048 0 50 0 1024 0 0 0 0 0 0 0 0\n8 16 sdb 200 0 4096 0 100 0 2048 0 0 0 0 0 0 0 0\n",
        ),
        (
            "modules",
            "/proc/modules",
            "fixture 4096 2 first,second, Live 0x0\nunused 8192 0 - Live 0x0\n",
        ),
        ("shmmax", "/proc/sys/kernel/shmmax", "8388608\n"),
        ("shmall", "/proc/sys/kernel/shmall", "3\n"),
        ("shmmni", "/proc/sys/kernel/shmmni", "32\n"),
    ] {
        let source = scratch.file(name, contents);
        mounts.bind(&source, Path::new(destination));
    }
    scratch.file("etc/passwd", b"root:x:0:0:root:/:/bin/false\n");
    scratch.file("etc/group", b"root:x:0:\n");
    mounts.bind(&scratch.0.join("etc"), Path::new("/etc"));
    let pci = "sys/bus/pci/devices/0000:00:01.0";
    for (name, value) in [
        ("vendor", "0x8086\n"),
        ("device", "0x1234\n"),
        ("class", "0x020300\n"),
        ("revision", "0x01\n"),
    ] {
        scratch.file(&format!("{pci}/{name}"), value);
    }
    let scsi = "sys/class/scsi_device/0:0:0:0/device";
    for (name, value) in [
        ("type", "0\n"),
        ("vendor", "AUDIT\n"),
        ("model", "FixtureDisk\n"),
        ("rev", "1.0\n"),
        ("block/sda/dev", "8:0\n"),
        ("block/sda/size", "2048\n"),
        ("block/sda/queue/logical_block_size", "512\n"),
        ("scsi_generic/sg0/dev", "1:3\n"),
    ] {
        scratch.file(&format!("{scsi}/{name}"), value);
    }
    mounts.bind(&scratch.0.join("sys"), Path::new("/sys"));
    scratch.file("dev/sda", b"not a block node");
    scratch.file("dev/sg0", b"not a character node");
    mounts.bind(&scratch.0.join("dev"), Path::new("/dev"));

    let date_before = chrono::Utc::now().date_naive();
    let output = axe("iostat", &["-cd", "sda", "2", "1"], &[]);
    let date_after = chrono::Utc::now().date_naive();
    let text = success(&output);
    let banner = text.lines().next().unwrap().split('\t').collect::<Vec<_>>();
    assert_eq!(banner.len(), 4, "{text}");
    let mut identity = std::mem::MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: uname initializes the complete utsname and its NUL-terminated fields.
    assert_eq!(unsafe { libc::uname(identity.as_mut_ptr()) }, 0);
    // SAFETY: uname succeeded.
    let identity = unsafe { identity.assume_init() };
    // SAFETY: uname NUL-terminated these fields.
    let (system, release, host, machine) = unsafe {
        (
            CStr::from_ptr(identity.sysname.as_ptr()),
            CStr::from_ptr(identity.release.as_ptr()),
            CStr::from_ptr(identity.nodename.as_ptr()),
            CStr::from_ptr(identity.machine.as_ptr()),
        )
    };
    assert_eq!(
        banner[0].trim_end(),
        format!(
            "{} {} ({})",
            system.to_string_lossy(),
            release.to_string_lossy(),
            host.to_string_lossy()
        )
    );
    let date = chrono::NaiveDate::parse_from_str(banner[1].trim(), "%m/%d/%y").unwrap();
    assert!(date == date_before || date == date_after);
    assert_eq!(banner[2], format!("_{}_", machine.to_string_lossy()));
    assert_eq!(banner[3], "(1 CPU)");
    let mut lines = text
        .lines()
        .skip_while(|line| !line.starts_with("avg-cpu:"));
    lines.next().unwrap();
    assert_eq!(
        lines.next().unwrap().split_whitespace().collect::<Vec<_>>(),
        ["10.00", "0.00", "10.00", "0.00", "0.00", "80.00"]
    );
    let row = text
        .lines()
        .find(|line| line.starts_with("sda "))
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(
        row,
        ["sda", "1.50", "10.24", "5.12", "0.00", "1024", "512", "0"]
    );
    assert!(!text.lines().any(|line| line.starts_with("sdb ")));
    let output = axe("iostat", &["-d", "sda", "1", "2"], &[]);
    let rows = success(&output)
        .lines()
        .filter(|line| line.starts_with("sda "))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], "1.50");
    assert_eq!(
        &rows[1][1..],
        &["0.00", "0.00", "0.00", "0.00", "0", "0", "0"]
    );
    for flags in ["-x", "-xz", "-cx"] {
        let output = axe("iostat", &[flags, "sda"], &[]);
        let text = success(&output);
        assert!(
            text.contains("avg-cpu:") && text.contains("Device ") && text.contains("await="),
            "{output:?}"
        );
    }
    let output = axe("iostat", &["-dx", "sda"], &[]);
    assert!(!success(&output).contains("avg-cpu:"));
    assert!(success(&output).contains("await="));
    assert!(!axe("iostat", &["-o", "JSON"], &[]).status.success());
    let output = axe("lsmod", &[], &[]);
    let rows = success(&output).lines().skip(1).collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].split_whitespace().collect::<Vec<_>>(),
        ["fixture", "4096", "2", "first,second"]
    );
    assert_eq!(
        rows[1].split_whitespace().collect::<Vec<_>>(),
        ["unused", "8192", "0"]
    );
    assert!(rows.iter().all(|row| *row == row.trim_end()), "{rows:?}");

    for selector in ["0000:00:01.0", "00:01.0", "01.0", "*:00:01.*"] {
        let output = axe("lspci", &["-n", "-s", selector], &[]);
        assert_eq!(success(&output), "00:01.0 0203: 8086:1234 (rev 01)\n");
    }
    let ids = scratch.file("pci.ids", b"8086  Intel fixture\n\t1234  Device fixture\nC 02  Network controller\n\t03  ATM network controller\n");
    assert_eq!(
        success(&axe("lspci", &["-nn", "-i"], &[&ids])),
        "00:01.0 ATM network controller [0203]: Intel fixture Device fixture [8086:1234] (rev 01)\n"
    );
    assert_eq!(
        success(&axe("lspci", &["-D", "-n"], &[])),
        "0000:00:01.0 0203: 8086:1234 (rev 01)\n"
    );
    assert!(!axe("lspci", &["-s", "0000:00:40.0"], &[]).status.success());
    for (name, value) in [
        ("vendor", "0x8086\n"),
        ("device", "0x5678\n"),
        ("class", "0x020000\n"),
        ("revision", "0x00\n"),
    ] {
        scratch.file(&format!("sys/bus/pci/devices/0001:00:01.0/{name}"), value);
    }
    assert_eq!(
        success(&axe("lspci", &["-n", "-s", "0001:00:01.0"], &[])),
        "0001:00:01.0 0200: 8086:5678\n"
    );
    for domain in ["10000", "7fffffff"] {
        let address = format!("{domain}:e0:1d.0");
        for (name, value) in [
            ("vendor", "0x8086\n"),
            ("device", "0x5678\n"),
            ("class", "0x020000\n"),
            ("revision", "0x00\n"),
        ] {
            scratch.file(&format!("sys/bus/pci/devices/{address}/{name}"), value);
        }
        let output = axe("lspci", &["-n", "-s", &address], &[]);
        assert_eq!(success(&output), format!("{address} 0200: 8086:5678\n"));
    }
    for flags in [vec![], vec!["-n"], vec!["-nn"]] {
        let output = axe("lspci", &flags, &[]);
        let text = success(&output);
        assert!(
            text.lines().any(|line| line.starts_with("10000:e0:1d.0 ")),
            "{output:?}"
        );
        assert!(
            text.lines()
                .any(|line| line.starts_with("7fffffff:e0:1d.0 ")),
            "{output:?}"
        );
    }
    assert!(
        !axe("lspci", &["-n", "-s", "80000000:e0:1d.0"], &[])
            .status
            .success()
    );

    let prefix = "[0:0:0:0]    disk    AUDIT    FixtureDisk      1.0   ";
    assert_eq!(
        success(&axe("lsscsi", &[], &[])),
        format!("{prefix}-        \n")
    );
    assert_eq!(
        success(&axe("lsscsi", &["-g"], &[])),
        format!("{prefix}-          -        \n")
    );
    assert_eq!(
        success(&axe("lsscsi", &["-s"], &[])),
        format!("{prefix}-          1.04MB\n")
    );
    assert_eq!(
        success(&axe("lsscsi", &["-ss"], &[])),
        format!("{prefix}-          1.00MiB\n")
    );
    assert_eq!(
        success(&axe("lsscsi", &["-k", "-g"], &[])),
        format!("{prefix}/dev/sda   /dev/sg0 \n")
    );
    scratch.file("dev/actual-generic", b"");
    mounts.bind(
        &PathBuf::from(format!("/proc/self/fd/{}", null.as_raw_fd())),
        Path::new("/dev/actual-generic"),
    );
    assert_eq!(
        success(&axe("lsscsi", &["-g"], &[])),
        format!("{prefix}-          /dev/actual-generic\n")
    );

    assert!(
        !success(&axe("ipcs", &["-m"], &[]))
            .lines()
            .any(|line| line.starts_with("0x"))
    );
    let missing = axe("ipcs", &["-m", "-i", "2147483647"], &[]);
    assert!(missing.status.success(), "{missing:?}");
    assert!(missing.stdout.is_empty());
    assert!(
        missing
            .stderr
            .windows(b"2147483647".len())
            .any(|part| part == b"2147483647")
    );
    // SAFETY: sysconf has no pointer arguments or side effects.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let limits = |output: &Output| {
        success(output)
            .lines()
            .filter_map(|line| line.split_once('=')?.1.trim().parse::<u64>().ok())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        limits(&axe("ipcs", &["-m", "-l"], &[])),
        [32, 8192, 3 * (page_size / 1024), 1]
    );
    scratch.file("shmall", format!("{}\n", u64::MAX));
    let total = u64::MAX - u64::MAX % (page_size / 1024);
    assert_eq!(limits(&axe("ipcs", &["-ml"], &[]))[2], total);
    let objects = IpcObjects::new();
    for (flag, id, size, count) in [
        ("-m", objects.shm, "4097", "0"),
        ("-q", objects.msg, "3", "1"),
    ] {
        let output = axe("ipcs", &[flag], &[]);
        let row = success(&output)
            .lines()
            .find(|line| line.starts_with("0x"))
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(
            &row[..6],
            &["0x00000000", &id.to_string(), "root", "640", size, count]
        );
        let output = axe("ipcs", &[flag, "-c"], &[]);
        let row = success(&output)
            .lines()
            .find(|line| line.starts_with(&id.to_string()))
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(&row[1..], &["640", "root", "root", "root", "root"]);
        let output = axe("ipcs", &[flag, "-i", &id.to_string()], &[]);
        assert!(success(&output).contains("mode=0640"));
        assert!(!success(&output).contains("0o640"));
        let output = axe("ipcs", &[flag, "-t"], &[]);
        assert!(success(&output).contains("Not set"));
    }
    let output = axe("ipcs", &["-s"], &[]);
    let row = success(&output)
        .lines()
        .find(|line| line.starts_with("0x"))
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(
        &row[..],
        &["0x00000000", &objects.sem.to_string(), "root", "640", "3"]
    );
    let summary = axe("ipcs", &["-u"], &[]);
    assert_eq!(
        success(&summary),
        format!(
            "\n------ Messages Status --------\nallocated queues = 1\nused headers = 1\nused space = 3 bytes\n\n------ Shared Memory Status --------\nsegments allocated 1\npages allocated {}\npages resident  0\npages swapped   0\nSwap performance: 0 attempts\t 0 successes\n\n------ Semaphore Status --------\nused arrays = 1\nallocated semaphores = 3\n\n",
            4097u64.div_ceil(page_size)
        )
    );
    let output = axe("ipcs", &["-s", "-i", &objects.sem.to_string()], &[]);
    assert!(success(&output).contains("semnum     value      ncount     zcount     pid       \n"));
    let output = axe("ipcs", &["-m", "-p"], &[]);
    let row = success(&output)
        .lines()
        .find(|line| line.starts_with(&objects.shm.to_string()))
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(&row[1..], &["root", &std::process::id().to_string(), "0"]);
}
