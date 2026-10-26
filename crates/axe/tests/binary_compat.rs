use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("axe-binary-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(directory: &Path, applet: &str, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .arg(applet)
        .args(args)
        .current_dir(directory)
        .env("LC_ALL", "C")
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn assert_output(output: Output, expected: &[u8]) {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, expected, "{output:?}");
}

#[test]
fn wide_strings_can_start_at_every_byte_alignment() {
    let scratch = Scratch::new("wide-offsets");
    for (encoding, width, big_endian) in [
        ("l", 2, false),
        ("b", 2, true),
        ("L", 4, false),
        ("B", 4, true),
    ] {
        for offset in 0..width {
            let mut bytes = vec![0xff; offset];
            for character in b"Junk" {
                let unit = u32::from(*character);
                if big_endian {
                    bytes.extend_from_slice(&unit.to_be_bytes()[4 - width..]);
                } else {
                    bytes.extend_from_slice(&unit.to_le_bytes()[..width]);
                }
            }
            bytes.extend(std::iter::repeat_n(0, width));
            let option = format!("-e{encoding}");
            let expected = format!("{offset:7} Junk\n");
            assert_output(
                run(&scratch.0, "strings", &[&option, "-n4", "-td"], &bytes),
                expected.as_bytes(),
            );
            fs::write(scratch.0.join("input"), &bytes).unwrap();
            assert_output(
                run(
                    &scratch.0,
                    "strings",
                    &[&option, "-n4", "-td", "input"],
                    b"",
                ),
                expected.as_bytes(),
            );
        }
    }
}

#[test]
fn wide_scanner_resynchronizes_after_a_run_ends() {
    let scratch = Scratch::new("wide-resync");
    assert_output(
        run(
            &scratch.0,
            "strings",
            &["-el", "-n4", "-td"],
            b"\0A\0B\0C\0D\0\0Z\0e\0r\0o\0\0\0",
        ),
        b"      1 ABCD\n     10 Zero\n",
    );
}

#[test]
fn data_only_strings_fall_back_for_plain_stdin_and_files() {
    let scratch = Scratch::new("plain-data");
    let plain = b"plain printable text\n";
    fs::write(scratch.0.join("plain"), plain).unwrap();
    assert_output(run(&scratch.0, "strings", &["-d"], plain), plain);
    assert_output(run(&scratch.0, "strings", &["-d", "plain"], b""), plain);
    assert_output(
        run(&scratch.0, "strings", &["-d", "-td"], b"\0alpha\0beta\0"),
        b"      1 alpha\n      7 beta\n",
    );
}

#[test]
fn file_charsets_come_from_content_for_text_and_recognized_formats() {
    let scratch = Scratch::new("charsets");
    for (index, bytes, expected) in [
        (
            0,
            b"#!/bin/sh\necho hi\n".as_slice(),
            b"text/x-shellscript; charset=us-ascii\n".as_slice(),
        ),
        (
            1,
            b"caf\xe9\n".as_slice(),
            b"text/plain; charset=iso-8859-1\n".as_slice(),
        ),
        (
            2,
            b"\xff\xfeh\0e\0l\0l\0o\0\n\0".as_slice(),
            b"text/plain; charset=utf-16le\n".as_slice(),
        ),
        (
            3,
            b"\xfe\xff\0h\0e\0l\0l\0o\0\n".as_slice(),
            b"text/plain; charset=utf-16be\n".as_slice(),
        ),
        (
            4,
            b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\n%%EOF\n".as_slice(),
            b"application/pdf; charset=us-ascii\n".as_slice(),
        ),
        (
            5,
            b"#!/bin/sh\necho caf\xc3\xa9\n".as_slice(),
            b"text/x-shellscript; charset=utf-8\n".as_slice(),
        ),
        (
            6,
            b"plain\0binary\n".as_slice(),
            b"application/octet-stream; charset=binary\n".as_slice(),
        ),
    ] {
        assert_output(run(&scratch.0, "file", &["-bi", "-"], bytes), expected);
        let name = format!("input-{index}");
        fs::write(scratch.0.join(&name), bytes).unwrap();
        assert_output(run(&scratch.0, "file", &["-bi", &name], b""), expected);
    }
    assert_output(
        run(
            &scratch.0,
            "file",
            &["-b", "--mime-encoding", "-"],
            b"#!/bin/sh\necho hi\n",
        ),
        b"us-ascii\n",
    );
}

#[test]
fn file_utf16_validation_rejects_embedded_nul_and_incomplete_units() {
    let scratch = Scratch::new("invalid-utf16");
    for bytes in [
        b"\xff\xfeh\0\0\0".as_slice(),
        b"\xff\xfeh".as_slice(),
        b"\xff\xfe\0\xd8".as_slice(),
    ] {
        assert_output(
            run(&scratch.0, "file", &["-b", "--mime-encoding", "-"], bytes),
            b"binary\n",
        );
    }
}

fn elf_header(file_type: u16, interpreter: bool, big_endian: bool) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes[..7].copy_from_slice(if big_endian {
        b"\x7fELF\x02\x02\x01"
    } else {
        b"\x7fELF\x02\x01\x01"
    });
    let write16 = |bytes: &mut [u8], offset, value: u16| {
        let value = if big_endian {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        bytes[offset..offset + 2].copy_from_slice(&value);
    };
    let value = if big_endian {
        1u32.to_be_bytes()
    } else {
        1u32.to_le_bytes()
    };
    bytes[20..24].copy_from_slice(&value);
    write16(&mut bytes, 16, file_type);
    write16(&mut bytes, 18, 62);
    write16(&mut bytes, 52, 64);
    if interpreter {
        let value = if big_endian {
            64u64.to_be_bytes()
        } else {
            64u64.to_le_bytes()
        };
        bytes[32..40].copy_from_slice(&value);
        write16(&mut bytes, 54, 56);
        write16(&mut bytes, 56, 1);
        let value = if big_endian {
            3u32.to_be_bytes()
        } else {
            3u32.to_le_bytes()
        };
        bytes[64..68].copy_from_slice(&value);
    }
    bytes
}

#[test]
fn file_elf_mime_distinguishes_objects_executables_libraries_and_pie() {
    let scratch = Scratch::new("elf-mime");
    for endian in [false, true] {
        for (file_type, interpreter, expected) in [
            (1, false, "application/x-object; charset=binary\n"),
            (2, false, "application/x-executable; charset=binary\n"),
            (3, false, "application/x-sharedlib; charset=binary\n"),
            (3, true, "application/x-pie-executable; charset=binary\n"),
        ] {
            let bytes = elf_header(file_type, interpreter, endian);
            assert_output(
                run(&scratch.0, "file", &["-bi", "-"], &bytes),
                expected.as_bytes(),
            );
            fs::write(scratch.0.join("elf"), bytes).unwrap();
            assert_output(
                run(&scratch.0, "file", &["-bi", "elf"], b""),
                expected.as_bytes(),
            );
        }
    }
}

#[test]
fn file_description_includes_elf_header_details() {
    let scratch = Scratch::new("elf-description");
    let bytes = elf_header(2, false, false);
    assert_output(
        run(&scratch.0, "file", &["-b", "-"], &bytes),
        b"ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked\n",
    );
}

#[test]
fn file_keeps_bounded_regular_file_json_detection_and_valid_scalar_json() {
    let scratch = Scratch::new("json-boundary");
    let bytes = [b"{\"x\":\"".as_slice(), &vec![b'a'; 66_000], b"\"}\n"].concat();
    fs::write(scratch.0.join("large.json"), &bytes).unwrap();
    assert_output(
        run(&scratch.0, "file", &["-bi", "large.json"], b""),
        b"text/plain; charset=us-ascii\n",
    );
    assert_output(
        run(&scratch.0, "file", &["-bi", "-"], &bytes),
        b"application/json; charset=us-ascii\n",
    );
    assert_output(
        run(&scratch.0, "file", &["-bi", "-"], b"null\n"),
        b"application/json; charset=us-ascii\n",
    );
}

#[test]
fn file_text_samples_do_not_misclassify_characters_split_at_the_size_bound() {
    let scratch = Scratch::new("sample-encoding-boundary");
    let mut utf16 = b"\xff\xfe".to_vec();
    for _ in 0..32_766 {
        utf16.extend_from_slice(b"a\0");
    }
    utf16.extend_from_slice(&0xd83du16.to_le_bytes());
    utf16.extend_from_slice(&0xde00u16.to_le_bytes());
    fs::write(scratch.0.join("utf16"), utf16).unwrap();
    assert_output(
        run(&scratch.0, "file", &["-b", "--mime-encoding", "utf16"], b""),
        b"utf-16le\n",
    );
    let mut utf8 = vec![b'a'; 65_535];
    utf8.extend_from_slice("é".as_bytes());
    fs::write(scratch.0.join("utf8"), utf8).unwrap();
    assert_output(
        run(&scratch.0, "file", &["-b", "--mime-encoding", "utf8"], b""),
        b"utf-8\n",
    );
}
