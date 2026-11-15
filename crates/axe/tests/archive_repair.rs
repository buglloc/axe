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
        assert_eq!(output.status.code(), Some(1), "{applet} {args:?}: {output:?}");
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
            assert_eq!(output.status.code(), Some(1), "{applet} {length}: {output:?}");
            assert!(output.stdout.is_empty(), "{applet} {length}: {output:?}");
            let mut input = alphabet.to_vec();
            input.extend_from_slice(&alphabet[..length]);
            let output = run(scratch.path(), applet, &args, &input);
            assert_eq!(output.status.code(), Some(1), "{applet} {length}: {output:?}");
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
    for (encoding, width, big_endian) in
        [("b", 2, true), ("l", 2, false), ("B", 4, true), ("L", 4, false)]
    {
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
    for frames in [vec![first.as_slice()], vec![first.as_slice(), second.as_slice()]] {
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
        &first[..3],
        &b"\x50\x2a\x4d"[..],
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
