#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, FileTimes};
use std::io::Read;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-tar-formats-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
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

fn axe(root: &Path, args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "tar", "--"])
        .args(args)
        .current_dir(root)
        .env("LC_ALL", "C")
        .output()
        .expect("run AXE tar")
}

fn success(output: Output) {
    assert!(
        output.status.success(),
        "status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn gnu_extract(root: &Path, archive: &str, destination: &str) -> PathBuf {
    let reference_path = std::env::var_os("AXE_REFERENCE_PATH")
        .expect("enter nix develop .#default to provide AXE_REFERENCE_PATH");
    let executable = std::env::split_paths(&reference_path)
        .map(|directory| directory.join("tar"))
        .find(|candidate| candidate.is_file())
        .expect("GNU tar in AXE_REFERENCE_PATH");
    let destination = root.join(destination);
    fs::create_dir(&destination).unwrap();
    success(
        Command::new(executable)
            .args([OsStr::new("xf"), OsStr::new(archive), OsStr::new("-C")])
            .arg(&destination)
            .current_dir(root)
            .env("PATH", reference_path)
            .env("LC_ALL", "C")
            .output()
            .expect("extract with original GNU tar"),
    );
    destination
}

fn raw_members(bytes: &[u8]) -> Vec<(tar::Header, Vec<u8>)> {
    tar::Archive::new(bytes)
        .entries()
        .unwrap()
        .raw(true)
        .map(|entry| {
            let mut entry = entry.unwrap();
            let header = entry.header().clone();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            (header, data)
        })
        .collect()
}

fn assert_portable_headers(members: &[(tar::Header, Vec<u8>)], pax: bool) {
    assert!(!members.is_empty());
    for (header, _) in members {
        assert!(header.as_ustar().is_some(), "not a USTAR header: {header:?}");
        assert!(header.as_gnu().is_none());
        let kind = header.entry_type();
        assert!(!kind.is_gnu_longname() && !kind.is_gnu_longlink() && !kind.is_gnu_sparse());
        assert!(pax || !kind.is_pax_local_extensions());
        for range in [108..116, 116..124, 124..136, 136..148, 329..337, 337..345] {
            assert_eq!(header.as_bytes()[range.start] & 0x80, 0, "base-256 field");
        }
    }
}

fn set_mtime(path: &Path, time: std::time::SystemTime) {
    File::open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(time))
        .unwrap();
}

#[test]
fn selected_format_controls_real_headers_and_default_stays_gnu() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("file"), b"payload").unwrap();
    set_mtime(&scratch.path().join("file"), UNIX_EPOCH);
    for format in [None, Some("gnu"), Some("ustar"), Some("posix"), Some("pax")] {
        let option = format.map(|format| format!("--format={format}"));
        let mut args = vec![OsStr::new("cf"), OsStr::new("archive.tar")];
        if let Some(option) = &option {
            args.push(OsStr::new(option));
        }
        args.push(OsStr::new("file"));
        success(axe(scratch.path(), &args));
        let bytes = fs::read(scratch.path().join("archive.tar")).unwrap();
        let members = raw_members(&bytes);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].1, b"payload");
        if matches!(format, None | Some("gnu")) {
            assert!(members[0].0.as_gnu().is_some());
        } else {
            assert_portable_headers(&members, format != Some("ustar"));
        }
        let output = gnu_extract(scratch.path(), "archive.tar", &format!("out-{format:?}"));
        assert_eq!(fs::read(output.join("file")).unwrap(), b"payload");
    }
}

#[test]
fn ustar_uses_prefix_and_preserves_byte_names_and_literal_symlinks() {
    let scratch = Scratch::new();
    let prefix = "p".repeat(140);
    let name = format!("{prefix}/{}", "n".repeat(90));
    fs::create_dir(scratch.path().join(&prefix)).unwrap();
    fs::write(scratch.path().join(&name), b"prefix data").unwrap();
    let raw = OsString::from_vec(b"raw-\xff".to_vec());
    fs::write(scratch.path().join(&raw), b"raw data").unwrap();
    symlink("a//../b", scratch.path().join("link")).unwrap();
    success(axe(
        scratch.path(),
        &["cf".as_ref(), "archive.tar".as_ref(), "--format".as_ref(), "ustar".as_ref(), name.as_ref(), &raw, "link".as_ref()],
    ));
    let bytes = fs::read(scratch.path().join("archive.tar")).unwrap();
    let members = raw_members(&bytes);
    assert_portable_headers(&members, false);
    assert_eq!(members.len(), 3);
    assert_eq!(members[0].0.path_bytes(), name.as_bytes());
    assert!(members[0].0.as_bytes()[345..500].iter().any(|byte| *byte != 0));
    let output = gnu_extract(scratch.path(), "archive.tar", "out");
    assert_eq!(fs::read(output.join(&name)).unwrap(), b"prefix data");
    assert_eq!(fs::read(output.join(&raw)).unwrap(), b"raw data");
    assert_eq!(fs::read_link(output.join("link")).unwrap(), Path::new("a//../b"));
}

#[test]
fn ustar_rejects_long_names_long_links_and_numeric_overflow() {
    let scratch = Scratch::new();
    let long = "n".repeat(101);
    fs::write(scratch.path().join(&long), b"long").unwrap();
    symlink(&long, scratch.path().join("link")).unwrap();
    fs::write(scratch.path().join("negative-time"), b"old").unwrap();
    set_mtime(&scratch.path().join("negative-time"), UNIX_EPOCH - Duration::from_secs(1));
    File::create(scratch.path().join("large")).unwrap().set_len(0o100000000000).unwrap();
    for name in [long.as_str(), "link", "negative-time", "large"] {
        let output = axe(scratch.path(), &["cf".as_ref(), "archive.tar".as_ref(), "--format=ustar".as_ref(), name.as_ref()]);
        assert_eq!(output.status.code(), Some(2), "{name}: {output:?}");
        assert!(!output.stderr.is_empty());
        let bytes = fs::read(scratch.path().join("archive.tar")).unwrap();
        assert!(raw_members(&bytes).is_empty(), "failed member emitted headers");
    }
}

#[test]
fn pax_preserves_long_unicode_and_non_utf8_paths_and_link_targets() {
    let scratch = Scratch::new();
    let long = "n".repeat(150);
    let unicode = "каталог";
    let raw = OsString::from_vec([b"raw-".as_slice(), &[0xff], &vec![b'x'; 120]].concat());
    fs::write(scratch.path().join(&long), b"long data").unwrap();
    fs::write(scratch.path().join(unicode), b"unicode data").unwrap();
    fs::write(scratch.path().join(&raw), b"raw data").unwrap();
    symlink(&long, scratch.path().join("long-link")).unwrap();
    symlink(unicode, scratch.path().join("unicode-link")).unwrap();
    symlink(&raw, scratch.path().join("raw-link")).unwrap();
    fs::hard_link(scratch.path().join(&long), scratch.path().join("hard-link")).unwrap();
    success(axe(scratch.path(), &[
        "cf".as_ref(), "archive.tar".as_ref(), "--format=posix".as_ref(),
        long.as_ref(), unicode.as_ref(), &raw, "long-link".as_ref(),
        "unicode-link".as_ref(), "raw-link".as_ref(), "hard-link".as_ref(),
    ]));
    let bytes = fs::read(scratch.path().join("archive.tar")).unwrap();
    let members = raw_members(&bytes);
    assert_portable_headers(&members, true);
    let pax: Vec<_> = members.iter()
        .filter(|(header, _)| header.entry_type().is_pax_local_extensions())
        .flat_map(|(_, data)| tar::PaxExtensions::new(data).map(|extension| {
            let extension = extension.unwrap();
            (extension.key_bytes().to_vec(), extension.value_bytes().to_vec())
        }))
        .collect();
    for (key, value) in [
        (b"path".as_slice(), long.as_bytes()),
        (b"path", unicode.as_bytes()),
        (b"path", raw.as_encoded_bytes()),
        (b"linkpath", long.as_bytes()),
        (b"linkpath", unicode.as_bytes()),
        (b"linkpath", raw.as_encoded_bytes()),
        (b"hdrcharset", b"BINARY"),
    ] {
        assert!(pax.iter().any(|(actual_key, actual_value)| actual_key == key && actual_value == value));
    }
    let output = gnu_extract(scratch.path(), "archive.tar", "out");
    assert_eq!(fs::read(output.join(&long)).unwrap(), b"long data");
    assert_eq!(fs::read(output.join(unicode)).unwrap(), b"unicode data");
    assert_eq!(fs::read(output.join(&raw)).unwrap(), b"raw data");
    for (name, target) in [("long-link", OsStr::new(&long)), ("unicode-link", OsStr::new(unicode)), ("raw-link", &raw)] {
        assert_eq!(fs::read_link(output.join(name)).unwrap(), Path::new(target));
    }
    assert_eq!(fs::metadata(output.join(&long)).unwrap().ino(), fs::metadata(output.join("hard-link")).unwrap().ino());
}

#[test]
fn pax_preserves_negative_and_fractional_mtime() {
    let scratch = Scratch::new();
    for (name, time, timestamp) in [
        ("negative", UNIX_EPOCH - Duration::from_millis(500), "-0.5"),
        ("fractional", UNIX_EPOCH + Duration::from_millis(1500), "1.5"),
    ] {
        fs::write(scratch.path().join(name), b"time data").unwrap();
        set_mtime(&scratch.path().join(name), time);
        success(axe(scratch.path(), &["cf".as_ref(), "archive.tar".as_ref(), "--format=pax".as_ref(), name.as_ref()]));
        let bytes = fs::read(scratch.path().join("archive.tar")).unwrap();
        let members = raw_members(&bytes);
        assert_portable_headers(&members, true);
        assert!(members.iter().filter(|(header, _)| header.entry_type().is_pax_local_extensions()).any(|(_, data)| {
            tar::PaxExtensions::new(data).any(|extension| {
                let extension = extension.unwrap();
                extension.key_bytes() == b"mtime" && extension.value_bytes() == timestamp.as_bytes()
            })
        }));
        let output = gnu_extract(scratch.path(), "archive.tar", &format!("out-{name}"));
        assert_eq!(fs::metadata(output.join(name)).unwrap().modified().unwrap(), time);
    }
}

#[test]
fn portable_formats_keep_directory_lists_excludes_compression_and_dereference() {
    let scratch = Scratch::new();
    fs::create_dir(scratch.path().join("first")).unwrap();
    fs::create_dir(scratch.path().join("second")).unwrap();
    fs::write(scratch.path().join("first/file"), b"first").unwrap();
    fs::write(scratch.path().join("second/skip"), b"excluded").unwrap();
    fs::write(scratch.path().join("second/other"), b"second").unwrap();
    fs::create_dir(scratch.path().join("second/tree")).unwrap();
    fs::write(scratch.path().join("second/tree/data"), b"nested").unwrap();
    symlink("file", scratch.path().join("first/link")).unwrap();
    fs::write(scratch.path().join("files"), b"file\nlink\n-C ../second\nother\nskip\ntree\n").unwrap();
    for format in ["ustar", "pax"] {
        let option = format!("--format={format}");
        success(axe(scratch.path(), &[
            "czhf".as_ref(), "archive.tar.gz".as_ref(), option.as_ref(),
            "--exclude=skip".as_ref(), "-C".as_ref(), "first".as_ref(),
            "-T".as_ref(), "files".as_ref(),
        ]));
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(File::open(scratch.path().join("archive.tar.gz")).unwrap())
            .read_to_end(&mut bytes).unwrap();
        let members = raw_members(&bytes);
        assert_portable_headers(&members, format == "pax");
        assert!(members.iter()
            .filter(|(header, _)| !header.entry_type().is_pax_local_extensions())
            .all(|(header, _)| header.entry_type().is_file() || header.entry_type().is_dir()));
        let output = gnu_extract(scratch.path(), "archive.tar.gz", format);
        assert_eq!(fs::read(output.join("file")).unwrap(), b"first");
        assert_eq!(fs::read(output.join("link")).unwrap(), b"first");
        assert!(!fs::symlink_metadata(output.join("link")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(output.join("other")).unwrap(), b"second");
        assert_eq!(fs::read(output.join("tree/data")).unwrap(), b"nested");
        assert!(!output.join("skip").exists());
    }
}
