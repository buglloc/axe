#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::write::GzEncoder;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-misc-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch directory");
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

fn run(directory: &Path, applet: &str, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", applet, "--"])
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("HOME", directory)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", "/nonexistent")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn applet");
    child.stdin.take().expect("piped stdin").write_all(input).expect("write input");
    child.wait_with_output().expect("wait for applet")
}

fn success(output: &Output) {
    assert!(output.status.success(), "{output:?}");
}

fn gzip_member(payload: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(payload).expect("encode fixture");
    encoder.finish().expect("finish fixture")
}

#[test]
fn gzip_plain_passthrough_requires_force_and_stdout_but_never_list() {
    let scratch = Scratch::new("gzip-plain");
    let plain = b"hello\n";
    fs::write(scratch.path().join("plain.gz"), plain).expect("write plain input");
    for args in [
        &["-df", "plain.gz"][..],
        &["-lf", "plain.gz"][..],
        &["-lcf", "plain.gz"][..],
        &["-t", "plain.gz"][..],
    ] {
        let output = run(scratch.path(), "gzip", args, b"");
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert_eq!(fs::read(scratch.path().join("plain.gz")).unwrap(), plain);
        assert!(!scratch.path().join("plain").exists());
    }
    for args in [
        &["-dcf", "plain.gz"][..],
        &["-cdf", "plain.gz"][..],
    ] {
        let output = run(scratch.path(), "gzip", args, b"");
        success(&output);
        assert_eq!(output.stdout, plain);
    }
    for args in [&["-tf", "plain.gz"][..], &["-ctf", "plain.gz"][..]] {
        let output = run(scratch.path(), "gzip", args, b"");
        success(&output);
        assert!(output.stdout.is_empty());
    }
    for input in [b"".as_slice(), b"x".as_slice(), b"\x1f".as_slice(), plain.as_slice()] {
        let output = run(scratch.path(), "gzip", &["-df"], input);
        success(&output);
        assert_eq!(output.stdout, input);
    }
}

#[test]
fn gzip_concatenated_members_and_trailing_bytes_keep_gnu_statuses() {
    let scratch = Scratch::new("gzip-members");
    let mut input = gzip_member(b"world\n");
    input.extend_from_slice(&gzip_member(b"second\n"));
    let valid_length = input.len();
    input.extend_from_slice(b"hello");
    for args in [&["-dc"][..], &["-dcq"][..], &["-t"][..]] {
        let output = run(scratch.path(), "gzip", args, &input);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert_eq!(output.stdout, if args == ["-t"] { b"".as_slice() } else { b"world\nsecond\n".as_slice() });
        if args == ["-dcq"] {
            assert!(output.stderr.is_empty());
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).contains("decompression OK, trailing garbage ignored"));
        }
    }
    let output = run(scratch.path(), "gzip", &["-dcf"], &input);
    success(&output);
    assert_eq!(output.stdout, b"world\nsecond\nhello");
    let output = run(scratch.path(), "gzip", &["-tf"], &input);
    success(&output);
    assert!(output.stdout.is_empty());

    fs::write(scratch.path().join("members.gz"), &input).unwrap();
    let output = run(scratch.path(), "gzip", &["-df", "members.gz"], b"");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(fs::read(scratch.path().join("members")).unwrap(), b"world\nsecond\n");
    assert!(!scratch.path().join("members.gz").exists());

    input.truncate(valid_length);
    input.extend_from_slice(&[0, 0, 0]);
    let output = run(scratch.path(), "gzip", &["-dc"], &input);
    success(&output);
    assert_eq!(output.stdout, b"world\nsecond\n");
    assert!(output.stderr.is_empty());
    let output = run(scratch.path(), "gzip", &["-dcf"], &input);
    success(&output);
    assert_eq!(output.stdout, b"world\nsecond\n\0\0\0");

    input.truncate(valid_length);
    input.push(b'x');
    let output = run(scratch.path(), "gzip", &["-dc"], &input);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output.stdout, b"world\nsecond\n");
}

#[test]
fn gzip_force_does_not_hide_corrupt_gzip_members() {
    let scratch = Scratch::new("gzip-corrupt");
    let mut input = gzip_member(b"payload\n");
    let checksum = input.len() - 8;
    input[checksum] ^= 1;
    for args in [&["-dcf"][..], &["-tf"][..]] {
        let output = run(scratch.path(), "gzip", args, &input);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
    }
    input.truncate(3);
    let output = run(scratch.path(), "gzip", &["-dcf"], &input);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

#[test]
fn file_distinguishes_empty_regular_files_and_empty_standard_input_in_all_modes() {
    let scratch = Scratch::new("file-empty");
    fs::write(scratch.path().join("empty"), b"").unwrap();
    for (flags, regular, stdin) in [
        (&[][..], "empty", "empty"),
        (&["-i"][..], "inode/x-empty; charset=binary", "application/x-empty; charset=binary"),
        (&["--mime-type"][..], "inode/x-empty", "application/x-empty"),
        (&["--mime-encoding"][..], "binary", "binary"),
        (&["--mime-type", "--mime-encoding"][..], "inode/x-empty; charset=binary", "application/x-empty; charset=binary"),
    ] {
        let mut args = flags.to_vec();
        args.push("empty");
        let output = run(scratch.path(), "file", &args, b"");
        success(&output);
        assert_eq!(output.stdout, format!("empty: {regular}\n").as_bytes());
        *args.last_mut().unwrap() = "-";
        let output = run(scratch.path(), "file", &args, b"");
        success(&output);
        assert_eq!(output.stdout, format!("/dev/stdin: {stdin}\n").as_bytes());
    }
    let output = run(scratch.path(), "file", &["--mime-encoding", "-"], b"ascii\n");
    success(&output);
    assert_eq!(output.stdout, b"/dev/stdin: us-ascii\n");
}

#[cfg(unix)]
#[test]
fn file_symlinks_support_no_dereference_and_dangling_mime_semantics() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new("file-links");
    fs::write(scratch.path().join("target"), b"").unwrap();
    symlink("target", scratch.path().join("link")).unwrap();
    symlink("missing", scratch.path().join("broken")).unwrap();
    for (flags, expected) in [
        (&[][..], "broken symbolic link to missing"),
        (&["-i"][..], "inode/symlink"),
        (&["--mime-type"][..], "inode/symlink"),
        (&["--mime-encoding"][..], ""),
    ] {
        let mut args = flags.to_vec();
        args.extend(["-b", "broken"]);
        let output = run(scratch.path(), "file", &args, b"");
        success(&output);
        assert_eq!(output.stdout, format!("{expected}\n").as_bytes());
    }
    for flags in [&["-h"][..], &["-L", "-h"][..], &["--no-dereference"][..]] {
        let mut args = flags.to_vec();
        args.extend(["-bi", "link"]);
        let output = run(scratch.path(), "file", &args, b"");
        success(&output);
        assert_eq!(output.stdout, b"inode/symlink; charset=binary\n");
    }
    let output = run(scratch.path(), "file", &["-hLbi", "link"], b"");
    success(&output);
    assert_eq!(output.stdout, b"inode/x-empty; charset=binary\n");
    let output = run(scratch.path(), "file", &["--help"], b"");
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("--no-dereference"));
}

#[test]
fn file_error_option_reports_failures_and_continues_with_remaining_files() {
    let scratch = Scratch::new("file-error");
    fs::write(scratch.path().join("empty"), b"").unwrap();
    let output = run(scratch.path(), "file", &["missing"], b"");
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("cannot open `missing' (No such file or directory)"), "{text}");
    let output = run(scratch.path(), "file", &["-E", "missing", "empty"], b"");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output.stdout, b"missing: ERROR: cannot stat `missing' (No such file or directory)\nempty:   empty\n");
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn file_escapes_names_and_pads_the_escaped_display_width() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let scratch = Scratch::new("file-names");
    let unusual = OsString::from_vec(b"\xc3\xa9\n\xff".to_vec());
    fs::write(scratch.path().join(&unusual), b"").unwrap();
    fs::write(scratch.path().join("short"), b"").unwrap();
    let invoke = |no_pad: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
        command.args(["--applet", "file", "--"]);
        if no_pad { command.arg("-N"); }
        command.arg(&unusual).arg("short").current_dir(scratch.path())
            .env("LC_ALL", "C").env("PATH", "/nonexistent").output().unwrap()
    };
    let output = invoke(false);
    success(&output);
    assert_eq!(output.stdout, b"\\303\\251\\012\\377: empty\nshort:            empty\n");
    let output = invoke(true);
    success(&output);
    assert_eq!(output.stdout, b"\\303\\251\\012\\377: empty\nshort: empty\n");
}

#[cfg(unix)]
#[test]
fn file_device_numbers_follow_the_platform_libc_encoding() {
    use std::os::unix::fs::MetadataExt;
    let scratch = Scratch::new("file-device");
    let device = fs::metadata("/dev/null").unwrap().rdev() as libc::dev_t;
    let output = run(scratch.path(), "file", &["-b", "/dev/null"], b"");
    success(&output);
    assert_eq!(output.stdout, format!("character special ({}/{})\n", libc::major(device), libc::minor(device)).as_bytes());
    let output = run(scratch.path(), "file", &["-bi", "/dev/null"], b"");
    success(&output);
    assert_eq!(output.stdout, b"inode/chardevice; charset=binary\n");
}

#[test]
fn tree_human_sizes_accept_clusters_repetition_patterns_and_literal_paths() {
    let scratch = Scratch::new("tree-options");
    let root = scratch.path().join("root");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("big"), vec![0; 5000]).unwrap();
    fs::write(root.join(".hidden"), b"hidden").unwrap();
    fs::write(root.join("-h"), b"pattern").unwrap();
    fs::write(root.join("nested/leaf"), b"leaf").unwrap();
    for flags in [
        &["-ih", "--noreport"][..],
        &["-i", "-h", "-h", "--noreport"][..],
        &["-ihL", "1", "--noreport"][..],
    ] {
        let mut args = flags.to_vec();
        args.push("root");
        let output = run(scratch.path(), "tree", &args, b"");
        success(&output);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.lines().any(|line| line == "[4.9K]  big"), "{text}");
        assert!(!text.contains(" KB"));
        if flags.contains(&"1") { assert!(!text.contains("leaf"), "{text}"); }
    }
    let output = run(scratch.path(), "tree", &["-ha", "root"], b"");
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains(".hidden"));
    let output = run(scratch.path(), "tree", &["-iP", "-h", "--noreport", "root"], b"");
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.lines().any(|line| line == "-h"), "{text}");
    assert!(!text.contains("big"), "{text}");

    fs::create_dir(scratch.path().join("-h")).unwrap();
    fs::write(scratch.path().join("-h/inside"), b"").unwrap();
    let output = run(scratch.path(), "tree", &["--", "-h"], b"");
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("-h\n"), "{text}");
    assert!(text.contains("inside"), "{text}");
    assert!(!text.contains("big"), "{text}");
    let output = run(scratch.path(), "tree", &["--help"], b"");
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("--human-readable"));
}

#[test]
fn tree_size_metadata_precedes_names_and_includes_directory_entries() {
    let scratch = Scratch::new("tree-size");
    fs::create_dir(scratch.path().join("nested")).unwrap();
    fs::write(scratch.path().join("big"), vec![0; 5000]).unwrap();
    let output = run(scratch.path(), "tree", &["-ih", "--noreport"], b"");
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.lines().any(|line| line.starts_with('[') && line.ends_with("]  .")), "{text}");
    assert!(text.lines().any(|line| line.starts_with('[') && line.ends_with("]  nested")), "{text}");
    let output = run(scratch.path(), "tree", &["-is", "--noreport"], b"");
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).lines().any(|line| line == "[       5000]  big"));
}

#[test]
fn tree_busybox_invocation_preserves_the_option_terminator() {
    use std::os::unix::process::CommandExt;

    let scratch = Scratch::new("tree-argv0");
    fs::create_dir(scratch.path().join("-h")).unwrap();
    fs::write(scratch.path().join("-h/inside"), b"").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .arg0("tree")
        .args(["--", "-h"])
        .current_dir(scratch.path())
        .env("PATH", "/nonexistent")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("-h\n"), "{text}");
    assert!(text.contains("inside"), "{text}");
}

#[test]
fn file_special_kinds_share_mime_modes_and_broken_link_errors_remain_mode_specific() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let scratch = Scratch::new("file-special");
    fs::create_dir(scratch.path().join("directory")).unwrap();
    let _socket = UnixListener::bind(scratch.path().join("socket")).unwrap();
    nix::unistd::mkfifo(
        &scratch.path().join("fifo"),
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    ).unwrap();
    for (name, media_type) in [
        ("directory", "inode/directory"),
        ("socket", "inode/socket"),
        ("fifo", "inode/fifo"),
    ] {
        for (flags, expected) in [
            (&["-bi"][..], format!("{media_type}; charset=binary\n")),
            (&["-b", "--mime-type"][..], format!("{media_type}\n")),
            (&["-b", "--mime-encoding"][..], "binary\n".to_string()),
        ] {
            let mut args = flags.to_vec();
            args.push(name);
            let output = run(scratch.path(), "file", &args, b"");
            success(&output);
            assert_eq!(output.stdout, expected.as_bytes());
        }
    }
    symlink("missing", scratch.path().join("broken")).unwrap();
    let output = run(scratch.path(), "file", &["-bE", "broken"], b"");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output.stdout, b"ERROR: broken symbolic link to missing (No such file or directory)\n");
    let output = run(scratch.path(), "file", &["-bEi", "broken"], b"");
    success(&output);
    assert_eq!(output.stdout, b"inode/symlink\n");
}

#[test]
fn tree_size_output_retains_filtering_pruning_and_output_file_options() {
    let scratch = Scratch::new("tree-filters");
    let root = scratch.path().join("root");
    fs::create_dir_all(root.join("keep")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("skip")).unwrap();
    fs::write(root.join("keep/match.txt"), vec![0; 5000]).unwrap();
    fs::write(root.join("empty/miss.bin"), b"no match").unwrap();
    fs::write(root.join("skip/match.txt"), b"excluded").unwrap();
    let args = [
        "-ihP", "*.txt", "-I", "skip", "--prune", "--dirsfirst", "--noreport",
        "-o", "listing", "root",
    ];
    let output = run(scratch.path(), "tree", &args, b"");
    success(&output);
    assert!(output.stdout.is_empty());
    let text = fs::read_to_string(scratch.path().join("listing")).unwrap();
    assert!(text.lines().any(|line| line.ends_with("]  keep")), "{text}");
    assert!(text.lines().any(|line| line == "[4.9K]  match.txt"), "{text}");
    assert!(!text.contains("empty"), "{text}");
    assert!(!text.contains("skip"), "{text}");
    assert!(!text.contains("miss.bin"), "{text}");
}

#[test]
fn gzip_rejects_plain_force_input_before_touching_an_existing_destination() {
    let scratch = Scratch::new("gzip-existing");
    fs::write(scratch.path().join("plain.gz"), b"not gzip\n").unwrap();
    fs::write(scratch.path().join("plain"), b"keep destination\n").unwrap();
    let output = run(scratch.path(), "gzip", &["-df", "plain.gz"], b"");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read(scratch.path().join("plain.gz")).unwrap(), b"not gzip\n");
    assert_eq!(fs::read(scratch.path().join("plain")).unwrap(), b"keep destination\n");
}
