#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-archive-repair-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create scratch directory");
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
        .env("LC_ALL", "C")
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn applet");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write input");
    child.wait_with_output().expect("wait for applet")
}

#[test]
fn base_decoders_reject_carriage_returns_unless_ignoring_garbage() {
    let scratch = Scratch::new("decode-cr");
    for (applet, format) in [
        ("base64", None),
        ("base32", None),
        ("basenc", Some("--base64")),
        ("basenc", Some("--base64url")),
        ("basenc", Some("--base32")),
        ("basenc", Some("--base32hex")),
        ("basenc", Some("--base16")),
        ("basenc", Some("--base2msbf")),
        ("basenc", Some("--base2lsbf")),
    ] {
        let mut args = vec!["-d"];
        args.extend(format);
        let output = run(scratch.path(), applet, &args, b"\r\n");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{applet} {args:?}: {output:?}"
        );
        assert!(output.stdout.is_empty());
        let output = run(scratch.path(), applet, &args, b"\n");
        assert!(output.status.success(), "{applet} {args:?}: {output:?}");
        args.push("-i");
        let output = run(scratch.path(), applet, &args, b"\r\n");
        assert!(output.status.success(), "{applet} {args:?}: {output:?}");
    }
    let output = run(scratch.path(), "base64", &["-d"], b"jQe\r\n3");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output.stdout, b"\x8d\x07");
    let output = run(scratch.path(), "base64", &["-di"], b"jQe\r\n3");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"\x8d\x07\xb7");
}

#[test]
fn base64url_accepts_unpadded_final_quanta() {
    let scratch = Scratch::new("base64url-tail");
    for (input, expected) in [
        (&b"Aw"[..], &b"\x03"[..]),
        (&b"AwQ"[..], &b"\x03\x04"[..]),
        (&b"AwQFAw"[..], &b"\x03\x04\x05\x03"[..]),
        (&b"Aw=="[..], &b"\x03"[..]),
        (&b"AwQ="[..], &b"\x03\x04"[..]),
    ] {
        let output = run(scratch.path(), "basenc", &["--base64url", "-d"], input);
        assert!(output.status.success(), "{input:?}: {output:?}");
        assert_eq!(output.stdout, expected);
    }
    for input in [&b"A"[..], &b"Aw="[..], &b"Aw==="[..]] {
        let output = run(scratch.path(), "basenc", &["--base64url", "-d"], input);
        assert_eq!(output.status.code(), Some(1), "{input:?}: {output:?}");
    }
}

#[test]
fn base32_short_padded_quanta_do_not_emit_uncommitted_bytes() {
    let scratch = Scratch::new("base32-short-padding");
    for (applet, format, alphabet) in [
        ("base32", None, &b"ME======"[..]),
        ("basenc", Some("--base32"), &b"ME======"[..]),
        ("basenc", Some("--base32hex"), &b"C4======"[..]),
    ] {
        let mut args = vec!["-d"];
        args.extend(format);
        for length in 3..8 {
            let output = run(scratch.path(), applet, &args, &alphabet[..length]);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{applet} {length}: {output:?}"
            );
            assert!(output.stdout.is_empty(), "{applet} {length}: {output:?}");
            let mut input = alphabet.to_vec();
            input.extend_from_slice(&alphabet[..length]);
            let output = run(scratch.path(), applet, &args, &input);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{applet} {length}: {output:?}"
            );
            assert_eq!(output.stdout, b"a", "{applet} {length}: {output:?}");
        }
        let output = run(scratch.path(), applet, &args, alphabet);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"a");
        let output = run(scratch.path(), applet, &args, &alphabet[..2]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"a");
    }
}

#[test]
fn wide_strings_split_at_non_ascii_units_in_the_c_locale() {
    let scratch = Scratch::new("strings-wide");
    for (encoding, width, big_endian) in [
        ("b", 2, true),
        ("l", 2, false),
        ("B", 4, true),
        ("L", 4, false),
    ] {
        for alignment in 0..width {
            let mut input = vec![0; alignment];
            for byte in b"hello\xe9world\0" {
                let value = u32::from(*byte);
                if big_endian {
                    input.extend_from_slice(&value.to_be_bytes()[4 - width..]);
                } else {
                    input.extend_from_slice(&value.to_le_bytes()[..width]);
                }
            }
            let output = run(
                scratch.path(),
                "strings",
                &["-e", encoding, "-n3", "-td", "-s", "|"],
                &input,
            );
            assert!(output.status.success(), "{output:?}");
            assert_eq!(
                output.stdout,
                format!("{alignment:7} hello|{:7} world|", alignment + 6 * width).as_bytes(),
                "{encoding} alignment {alignment}: {output:?}"
            );
        }
    }
}

#[test]
fn strings_all_whitespace_includes_vertical_tabs() {
    let scratch = Scratch::new("strings-whitespace");
    let input = b"alpha\tbeta\nnext\rlast\x0bfinal\x0cEND\0done";
    let output = run(scratch.path(), "strings", &["-n3", "-w"], input);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        b"alpha\tbeta\nnext\rlast\x0bfinal\x0cEND\ndone\n"
    );
    let output = run(scratch.path(), "strings", &["-n3"], b"one\x0btwo");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"one\ntwo\n");
}

#[test]
fn zstd_forced_stdout_copies_plaintext_after_complete_frames() {
    let scratch = Scratch::new("zstd-tail");
    let first = zstd::stream::encode_all(&b"first\0\xff\n"[..], 1).unwrap();
    let second = zstd::stream::encode_all(&b"second\n"[..], 1).unwrap();
    for frames in [
        vec![first.as_slice()],
        vec![first.as_slice(), second.as_slice()],
    ] {
        for tail in [&b"GARBAGE"[..], &[0][..], &[0; 4][..], &[0; 8][..]] {
            let mut compressed = frames.concat();
            compressed.extend_from_slice(tail);
            let mut expected = b"first\0\xff\n".to_vec();
            if frames.len() == 2 {
                expected.extend_from_slice(b"second\n");
            }
            expected.extend_from_slice(tail);
            let output = run(scratch.path(), "zstd", &["-dcf"], &compressed);
            assert!(output.status.success(), "{output:?}");
            assert_eq!(output.stdout, expected);
            let output = run(scratch.path(), "zstd", &["-dc"], &compressed);
            assert!(!output.status.success(), "{output:?}");
        }
    }
    for tail in [
        &first[..4],
        &b"\x50\x2a\x4d\x18"[..],
        &b"\x50\x2a\x4d\x18\x04\0\0\0x"[..],
    ] {
        let mut compressed = first.clone();
        compressed.extend_from_slice(tail);
        let output = run(scratch.path(), "zstd", &["-dcf"], &compressed);
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"first\0\xff\n");
    }
    let mut compressed = first;
    compressed.extend_from_slice(b"\x50\x2a\x4d\x18\x03\0\0\0xyz");
    compressed.extend_from_slice(&second);
    compressed.extend_from_slice(b"tail");
    let output = run(scratch.path(), "zstd", &["-dcf"], &compressed);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"first\0\xff\nsecond\ntail");
}

fn gzip_member(payload: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn gzip_concatenated_listing_ratio_includes_member_framing() {
    let scratch = Scratch::new("gzip-list-ratio");
    let mut compressed = gzip_member(b"first member\0\xff\n");
    let single_size = compressed.len();
    fs::write(scratch.path().join("single.gz"), &compressed).unwrap();
    compressed.extend_from_slice(&gzip_member(b"second member\n"));
    fs::write(scratch.path().join("concat.gz"), &compressed).unwrap();
    for (file, compressed_size, plain_size, overhead) in [
        ("single.gz", single_size, 15, 18),
        ("concat.gz", compressed.len(), 29, 0),
    ] {
        let output = run(scratch.path(), "gzip", &["-l", file], b"");
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        let fields: Vec<_> = text.lines().nth(1).unwrap().split_whitespace().collect();
        assert_eq!(fields[0], compressed_size.to_string());
        assert_eq!(fields[1], plain_size.to_string());
        let ratio = 100.0 - (compressed_size - overhead) as f64 * 100.0 / plain_size as f64;
        assert_eq!(fields[2], format!("{ratio:.1}%"));
    }
}

#[test]
fn gzip_listing_rejects_trailing_junk_without_publishing_a_row_or_header() {
    let scratch = Scratch::new("gzip-list-junk");
    let good = gzip_member(b"payload\n");
    let mut junk = good.clone();
    junk.extend_from_slice(b"GARBAGE");
    fs::write(scratch.path().join("good.gz"), &good).unwrap();
    fs::write(scratch.path().join("junk.gz"), &junk).unwrap();
    for args in [&["-l", "junk.gz"][..], &["-lf", "junk.gz"][..], &["-l"][..]] {
        let output = run(scratch.path(), "gzip", args, &junk);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert!(!output.stderr.is_empty(), "{args:?}: {output:?}");
    }
    let valid = run(scratch.path(), "gzip", &["-l", "good.gz"], b"");
    assert!(valid.status.success(), "{valid:?}");
    for args in [
        &["-l", "junk.gz", "good.gz"][..],
        &["-l", "good.gz", "junk.gz"][..],
    ] {
        let output = run(scratch.path(), "gzip", args, b"");
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert_eq!(output.stdout, valid.stdout);
    }
    assert_eq!(fs::read(scratch.path().join("junk.gz")).unwrap(), junk);
    assert!(!scratch.path().join("junk").exists());
}

#[test]
fn base32_invalid_alphabet_bytes_commit_the_preceding_padded_prefix() {
    let scratch = Scratch::new("base32-invalid-after-padding");
    for (applet, format, encoded, decoded) in [
        ("base32", None, &b"ME======"[..], &b"a"[..]),
        ("basenc", Some("--base32"), &b"2U======"[..], &b"\xd5"[..]),
        ("basenc", Some("--base32hex"), &b"5S======"[..], &b"/"[..]),
    ] {
        let mut args = vec!["-d"];
        args.extend(format);
        for invalid in [&b"!"[..], &b"\r\n"[..], &b" "[..]] {
            let mut input = encoded[..3].to_vec();
            input.extend_from_slice(invalid);
            input.extend_from_slice(&encoded[3..]);
            let output = run(scratch.path(), applet, &args, &input);
            assert_eq!(output.status.code(), Some(1), "{input:?}: {output:?}");
            assert_eq!(output.stdout, decoded, "{input:?}: {output:?}");
        }
        let output = run(scratch.path(), applet, &args, &encoded[..7]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
}

#[test]
fn incomplete_compression_magic_is_plaintext_only_in_forced_stdout_mode() {
    let scratch = Scratch::new("incomplete-magic");
    for (applet, magic) in [
        ("xz", &b"\xfd7zXZ\0"[..]),
        ("unxz", &b"\xfd7zXZ\0"[..]),
        ("xzcat", &b"\xfd7zXZ\0"[..]),
        ("zstd", &b"\x28\xb5\x2f\xfd"[..]),
        ("zstd", &b"\x50\x2a\x4d\x18"[..]),
    ] {
        for length in 1..magic.len() {
            let prefix = &magic[..length];
            let output = run(scratch.path(), applet, &["-dcf"], prefix);
            assert!(output.status.success(), "{applet} {prefix:?}: {output:?}");
            assert_eq!(output.stdout, prefix);
            let output = run(scratch.path(), applet, &["-dc"], prefix);
            assert!(!output.status.success(), "{applet} {prefix:?}: {output:?}");
        }
        let output = run(scratch.path(), applet, &["-dcf"], magic);
        assert!(!output.status.success(), "{applet} {magic:?}: {output:?}");
    }
    let frame = zstd::stream::encode_all(&b"payload\n"[..], 1).unwrap();
    for magic in [&b"\x28\xb5\x2f\xfd"[..], &b"\x50\x2a\x4d\x18"[..]] {
        for length in 1..magic.len() {
            let mut input = frame.clone();
            input.extend_from_slice(&magic[..length]);
            let output = run(scratch.path(), "zstd", &["-dcf"], &input);
            assert!(output.status.success(), "{input:?}: {output:?}");
            assert_eq!(output.stdout, [&b"payload\n"[..], &magic[..length]].concat());
        }
    }
}
