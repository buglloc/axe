#![cfg(feature = "applet-strings")]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-strings-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
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

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_axe"))
        .arg("strings")
        .args(args)
        .output()
        .expect("run strings applet")
}

fn run_with_input(args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .arg("strings")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn strings applet");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input)
        .expect("write strings input");
    child.wait_with_output().expect("wait for strings applet")
}

#[test]
fn stdin_strings_cross_read_boundaries() {
    let mut input = vec![0; 65_534];
    input.extend_from_slice(b"boundary\0");

    let output = run_with_input(&["-n8", "-t", "d"], &input);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"  65534 boundary\n");
}

#[test]
fn gnu_prefix_offset_length_separator_and_parallel_order_are_compatible() {
    let scratch = Scratch::new("gnu-options");
    let first = scratch.path().join("first.bin");
    let second = scratch.path().join("second.bin");
    fs::write(&first, b"\0alpha\0tiny\0line\tvalue\0").expect("write first fixture");
    fs::write(&second, b"\0second\0").expect("write second fixture");

    let first_name = first.to_str().expect("UTF-8 scratch path");
    let second_name = second.to_str().expect("UTF-8 scratch path");
    let output = run(&[
        "--parallel=2",
        "-f",
        "-n5",
        "-t",
        "x",
        "-s",
        "|",
        first_name,
        second_name,
    ]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 output"),
        format!(
            "{first_name}:       1 alpha|{first_name}:       c line\tvalue|\
             {second_name}:       1 second|"
        )
    );
}

#[test]
fn parallel_output_larger_than_worker_queues_stays_ordered() {
    let scratch = Scratch::new("parallel-bounded");
    let mut paths = Vec::new();
    let mut expected = Vec::new();
    for (index, word) in ["first-one", "second-two", "third-three"]
        .into_iter()
        .enumerate()
    {
        let path = scratch.path().join(format!("{index}.bin"));
        let mut input = Vec::new();
        for _ in 0..25_000 {
            input.extend_from_slice(word.as_bytes());
            input.push(0);
            expected.extend_from_slice(word.as_bytes());
            expected.push(b'\n');
        }
        fs::write(&path, input).expect("write parallel fixture");
        paths.push(path);
    }
    let path_strings = paths
        .iter()
        .map(|path| path.to_str().expect("UTF-8 scratch path"))
        .collect::<Vec<_>>();
    let output = run(&[
        "--parallel=3",
        "-n5",
        path_strings[0],
        path_strings[1],
        path_strings[2],
    ]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, expected);
}

#[test]
fn unicode_encodings_json_and_null_records_preserve_offsets() {
    let scratch = Scratch::new("unicode");
    let utf8 = scratch.path().join("utf8.bin");
    let utf16 = scratch.path().join("utf16.bin");
    let gnu16 = scratch.path().join("gnu16.bin");
    fs::write(
        &utf8,
        b"\0\xd0\x9f\xd1\x80\xd0\xb8\xd0\xb2\xd0\xb5\xd1\x82\0bad\xffdata",
    )
    .expect("write UTF-8 fixture");

    let mut utf16_bytes = Vec::new();
    for unit in "hi\0Привет\0".encode_utf16() {
        utf16_bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(&utf16, utf16_bytes).expect("write UTF-16 fixture");
    fs::write(&gnu16, b"J\0u\0n\0k\0\0\0").expect("write GNU wide fixture");

    let utf8_name = utf8.to_str().expect("UTF-8 scratch path");
    let output = run(&["--encoding=utf8", "--json", "--null", "-n4", utf8_name]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| serde_json::from_slice::<serde_json::Value>(record).expect("valid JSON"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["file"], utf8_name);
    assert_eq!(records[0]["offset"], 1);
    assert_eq!(records[0]["encoding"], "utf8");
    assert_eq!(records[0]["string"], "Привет");
    assert_eq!(records[1]["offset"], 18);
    assert_eq!(records[1]["string"], "data");

    let utf16_name = utf16.to_str().expect("UTF-8 scratch path");
    let output = run(&["--encoding=utf16le", "-n4", "-t", "d", utf16_name]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, "      6 Привет\n".as_bytes());

    let gnu16_name = gnu16.to_str().expect("UTF-8 scratch path");
    let output = run(&["-e", "l", "-n4", gnu16_name]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Junk\n");
}

#[test]
fn data_mode_scans_allocated_elf_sections_only() {
    let scratch = Scratch::new("elf-data");
    let fixture = scratch.path().join("sections.elf");
    fs::write(&fixture, elf_with_allocated_and_debug_strings()).expect("write ELF fixture");
    let fixture = fixture.to_str().expect("UTF-8 scratch path");

    let data = run(&["-d", "-n6", fixture]);
    assert!(
        data.status.success(),
        "{}",
        String::from_utf8_lossy(&data.stderr)
    );
    assert_eq!(data.stdout, b"VISIBLE\n");

    let all = run(&["-a", "-n6", fixture]);
    assert!(
        all.status.success(),
        "{}",
        String::from_utf8_lossy(&all.stderr)
    );
    assert!(all.stdout.windows(7).any(|window| window == b"VISIBLE"));
    assert!(all.stdout.windows(6).any(|window| window == b"HIDDEN"));
}

fn elf_with_allocated_and_debug_strings() -> Vec<u8> {
    const SECTION_TABLE_OFFSET: usize = 64;
    const SECTION_SIZE: usize = 64;
    const NAMES_OFFSET: usize = 320;
    const DATA_OFFSET: usize = 352;
    const DEBUG_OFFSET: usize = 368;

    let names = b"\0.shstrtab\0.data\0.debug\0";
    let visible = b"\0VISIBLE\0";
    let hidden = b"\0HIDDEN\0";
    let mut elf = vec![0; 384];
    elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    put_u16(&mut elf, 16, 1);
    put_u16(&mut elf, 18, 62);
    put_u32(&mut elf, 20, 1);
    put_u64(&mut elf, 40, SECTION_TABLE_OFFSET as u64);
    put_u16(&mut elf, 52, 64);
    put_u16(&mut elf, 58, SECTION_SIZE as u16);
    put_u16(&mut elf, 60, 4);
    put_u16(&mut elf, 62, 1);

    put_section(
        &mut elf,
        SECTION_TABLE_OFFSET + SECTION_SIZE,
        1,
        3,
        0,
        NAMES_OFFSET,
        names.len(),
    );
    put_section(
        &mut elf,
        SECTION_TABLE_OFFSET + SECTION_SIZE * 2,
        11,
        1,
        2,
        DATA_OFFSET,
        visible.len(),
    );
    put_section(
        &mut elf,
        SECTION_TABLE_OFFSET + SECTION_SIZE * 3,
        17,
        1,
        0,
        DEBUG_OFFSET,
        hidden.len(),
    );
    elf[NAMES_OFFSET..NAMES_OFFSET + names.len()].copy_from_slice(names);
    elf[DATA_OFFSET..DATA_OFFSET + visible.len()].copy_from_slice(visible);
    elf[DEBUG_OFFSET..DEBUG_OFFSET + hidden.len()].copy_from_slice(hidden);
    elf
}

fn put_section(
    elf: &mut [u8],
    offset: usize,
    name: u32,
    kind: u32,
    flags: u64,
    file_offset: usize,
    size: usize,
) {
    put_u32(elf, offset, name);
    put_u32(elf, offset + 4, kind);
    put_u64(elf, offset + 8, flags);
    put_u64(elf, offset + 24, file_offset as u64);
    put_u64(elf, offset + 32, size as u64);
    put_u64(elf, offset + 48, 1);
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
