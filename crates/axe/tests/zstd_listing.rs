#![cfg(unix)]

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const HEADER: &str = "Frames  Skips  Compressed  Uncompressed  Ratio  Check  Filename\n";
const UNKNOWN: &[u8] = b"\x28\xb5\x2f\xfd\x04\x58\x79\0\0first member\0\xff\n\xea\x0d\xdf\x09";
const SECOND: &[u8] = b"\x28\xb5\x2f\xfd\x04\x58\x71\0\0second member\n\x39\xba\xcc\xb2";
const KNOWN: &[u8] = b"\x28\xb5\x2f\xfd\x20\x03\x19\0\0abc";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-zstd-listing-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, args: &[&OsStr]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", "zstd", "--"])
            .args(args)
            .current_dir(&self.0)
            .env_clear()
            .env("LC_ALL", "C")
            .env("PATH", "/nonexistent")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn list(&self, args: &[&str]) -> Output {
        self.run(&args.iter().map(OsStr::new).collect::<Vec<_>>())
    }

    fn put(&self, name: &str, bytes: &[u8]) {
        fs::write(self.0.join(name), bytes).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn table(output: &Output, status: i32, row: &str) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_eq!(output.stdout, format!("{HEADER}{row}").as_bytes());
}

#[test]
fn historical_listing_cases_preserve_structural_checksum_semantics() {
    let scratch = Scratch::new();
    scratch.put("data.zst", UNKNOWN);
    let output = scratch.list(&["-l", "data.zst"]);
    table(
        &output,
        0,
        "     1      0      28   B                       XXH64  data.zst\n",
    );
    assert!(output.stderr.is_empty());

    scratch.put("data.zst", &[UNKNOWN, SECOND].concat());
    table(
        &scratch.list(&["--list", "data.zst"]),
        0,
        "     2      0      55   B                       XXH64  data.zst\n",
    );

    scratch.put("data.zst", &[UNKNOWN, b"trailing text"].concat());
    table(
        &scratch.list(&["-l", "data.zst"]),
        1,
        "File \"data.zst\" not compressed by zstd \n",
    );

    scratch.put("data.zst", &UNKNOWN[..UNKNOWN.len() - 1]);
    let output = scratch.list(&["-l", "data.zst"]);
    table(
        &output,
        1,
        "     0      0      27   B                       XXH64  data.zst\n",
    );
    assert_eq!(
        output.stderr,
        b"Error: could not read checksum \nError while parsing \"data.zst\" \n"
    );

    let mut corrupt = UNKNOWN.to_vec();
    *corrupt.last_mut().unwrap() ^= 0xff;
    scratch.put("data.zst", &corrupt);
    let output = scratch.list(&["-l", "data.zst"]);
    table(
        &output,
        0,
        "     1      0      28   B                       XXH64  data.zst\n",
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn known_sizes_ratios_totals_and_unknown_sizes_match_zstd() {
    let scratch = Scratch::new();
    scratch.put("known.zst", KNOWN);
    scratch.put("other.zst", KNOWN);
    let output = scratch.list(&["-l", "known.zst", "other.zst"]);
    table(
        &output,
        0,
        concat!(
            "     1      0      12   B         3   B  0.250   None  known.zst\n",
            "     1      0      12   B         3   B  0.250   None  other.zst\n",
            "----------------------------------------------------------------- \n",
            "     2      0      24   B         6   B  0.250         2 files\n"
        ),
    );
    scratch.put("other.zst", UNKNOWN);
    let output = scratch.list(&["-l", "known.zst", "other.zst"]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(text.ends_with("     2      0      40   B                              2 files\n"));

    scratch.put("mixed.zst", &[KNOWN, UNKNOWN].concat());
    table(
        &scratch.list(&["-l", "mixed.zst"]),
        0,
        "     2      0      40   B                       XXH64  mixed.zst\n",
    );
}

#[test]
fn skippable_frames_and_sparse_payloads_are_scanned_without_decoding() {
    let scratch = Scratch::new();
    let skip = b"\x5f\x2a\x4d\x18\x03\0\0\0abc";
    scratch.put("skips.zst", &[skip.as_slice(), KNOWN, skip.as_slice()].concat());
    table(
        &scratch.list(&["-l", "skips.zst"]),
        0,
        "     3      2      34   B         3   B  0.088   None  skips.zst\n",
    );

    let mut sparse = File::create(scratch.0.join("sparse.zst")).unwrap();
    sparse.write_all(b"\x50\x2a\x4d\x18").unwrap();
    sparse.write_all(&(1_u32 << 30).to_le_bytes()).unwrap();
    sparse.set_len((1_u64 << 30) + 8).unwrap();
    let output = scratch.list(&["-l", "sparse.zst"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        format!("{HEADER}     1      1    1.00 GiB         0   B  0.000   None  sparse.zst\n").as_bytes()
    );

    // A structurally complete compressed block need not contain a decodable payload.
    scratch.put("opaque.zst", b"\x28\xb5\x2f\xfd\x20\x03\x0d\0\0\xff");
    table(
        &scratch.list(&["-l", "opaque.zst"]),
        0,
        "     1      0      10   B         3   B  0.300   None  opaque.zst\n",
    );
    scratch.put("rle.zst", b"\x28\xb5\x2f\xfd\x20\x03\x1b\0\0a");
    assert!(scratch.list(&["-l", "rle.zst"]).status.success());
}

#[test]
fn verbose_and_quiet_levels_control_detail_and_errors() {
    let scratch = Scratch::new();
    scratch.put("known.zst", KNOWN);
    let verbose = scratch.list(&["-lv", "known.zst"]);
    assert!(verbose.status.success());
    assert_eq!(
        verbose.stdout,
        b"known.zst \n# Zstandard Frames: 1\nDictID: 0\nWindow Size: 3 B (3 B)\nCompressed Size: 12 B (12 B)\nDecompressed Size: 3 B (3 B)\nRatio: 0.2500\nCheck: None\n\n"
    );
    let default = scratch.list(&["-l", "known.zst"]);
    for flags in ["-lq", "-lqq", "-lvq", "-lqv"] {
        let output = scratch.list(&[flags, "known.zst"]);
        assert!(output.status.success());
        assert_eq!(output.stdout, default.stdout, "{flags}");
    }
    scratch.put("unknown.zst", UNKNOWN);
    let verbose = scratch.list(&["-lv", "unknown.zst"]);
    assert_eq!(
        verbose.stdout,
        b"unknown.zst \n# Zstandard Frames: 1\nDictID: 0\nWindow Size: 2.00 MiB (2097152 B)\nCompressed Size: 28 B (28 B)\nCheck: XXH64 09df0dea\n\n"
    );
    scratch.put("bad.zst", &UNKNOWN[..UNKNOWN.len() - 1]);
    let quiet = scratch.list(&["-lqq", "bad.zst"]);
    assert_eq!(quiet.status.code(), Some(1));
    assert!(quiet.stderr.is_empty());
    assert!(!scratch.list(&["-lq", "bad.zst"]).stderr.is_empty());
}

#[test]
fn invalid_structure_and_operands_report_failure_without_hiding_good_files() {
    let scratch = Scratch::new();
    for (name, data) in [
        ("empty", b"".as_slice()),
        ("header", &KNOWN[..6]),
        ("block", &KNOWN[..10]),
        ("skip", b"\x50\x2a\x4d\x18\x03\0\0\0a"),
        ("reserved", b"\x28\xb5\x2f\xfd\x20\x03\x07\0\0"),
        ("bad-header", b"\x28\xb5\x2f\xfd\x28\x03\x19\0\0abc"),
    ] {
        scratch.put(name, data);
        let output = scratch.list(&["-l", name]);
        assert_eq!(output.status.code(), Some(1), "{name}: {output:?}");
    }
    fs::create_dir(scratch.0.join("directory")).unwrap();
    scratch.put("known.zst", KNOWN);
    let output = scratch.list(&["-l", "missing", "directory", "known.zst"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("0.250   None  known.zst"));
    assert!(String::from_utf8_lossy(&output.stdout).ends_with("1 files\n"));
    assert_eq!(
        output.stderr,
        b"Error : missing is not a file \nError : directory is not a file \n"
    );

    for args in [&["-l"][..], &["-l", "-"], &["-l", "known.zst", "-"]] {
        let output = scratch.list(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("does not support reading from standard input")
        );
    }
}

#[test]
fn paths_are_preserved_as_bytes_in_tables_and_errors() {
    let scratch = Scratch::new();
    let name = OsStr::from_bytes(b"frame-\xff.zst");
    fs::write(scratch.0.join(name), KNOWN).unwrap();
    let output = scratch.run(&[OsStr::new("-l"), name]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.ends_with(b"  frame-\xff.zst\n"));
    fs::remove_file(scratch.0.join(name)).unwrap();
    let output = scratch.run(&[OsStr::new("-l"), name]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, b"Error : frame-\xff.zst is not a file \n");
}
