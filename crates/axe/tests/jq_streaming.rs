#![cfg(unix)]

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-jq-stream-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
        command
            .args(["--applet", "jq", "--"])
            .args(args)
            .current_dir(&self.0)
            .env_clear()
            .env("HOME", &self.0)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("PATH", "/nonexistent")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = self.command(args).spawn().unwrap();
        if let Err(error) = child.stdin.take().unwrap().write_all(input) {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        child.wait_with_output().unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn expect(output: Output, status: i32, stdout: &[u8]) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_eq!(output.stdout, stdout, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn stream_emits_leaves_and_each_nonempty_container_end_in_input_order() {
    let scratch = Scratch::new();
    expect(
        scratch.run(
            &["--stream", "-c", "."],
            br#"[1,{"a":[2,3]},[],{}] null true "x" [] {}"#,
        ),
        0,
        b"[[0],1]\n[[1,\"a\",0],2]\n[[1,\"a\",1],3]\n[[1,\"a\",1]]\n[[1,\"a\"]]\n[[2],[]]\n[[3],{}]\n[[3]]\n[[],null]\n[[],true]\n[[],\"x\"]\n[[],[]]\n[[],{}]\n",
    );
    expect(
        scratch.run(
            &["--stream", "-c", "."],
            br#"{"a":[],"a":{},"b":[false,null]}"#,
        ),
        0,
        b"[[\"a\"],[]]\n[[\"a\"],{}]\n[[\"b\",0],false]\n[[\"b\",1],null]\n[[\"b\",1]]\n[[\"b\"]]\n",
    );
}

#[test]
fn stream_uses_existing_scalar_decoding_without_building_containers() {
    let scratch = Scratch::new();
    expect(
        scratch.run(
            &["--stream", "-c", "."],
            b"{\"k\xff\":[\"\\ud83d\\ude00\",+1,NaN,\"v\xfe\"]}",
        ),
        0,
        "[[\"k�\",0],\"😀\"]\n[[\"k�\",1],1]\n[[\"k�\",2],null]\n[[\"k�\",3],\"v�\"]\n[[\"k�\",3]]\n[[\"k�\"]]\n".as_bytes(),
    );
}

#[test]
fn stream_inputs_share_the_filter_cursor_and_update_input_filename() {
    let scratch = Scratch::new();
    fs::write(scratch.0.join("a"), "[1]").unwrap();
    fs::write(scratch.0.join("b"), "[2]").unwrap();
    expect(
        scratch.run(
            &[
                "--stream",
                "-nc",
                "[input_filename,input,input_filename,input,input_filename,input,input_filename]",
                "a",
                "b",
            ],
            b"",
        ),
        0,
        b"[null,[[0],1],\"a\",[[0]],\"a\",[[0],2],\"b\"]\n",
    );
    expect(
        scratch.run(&["--stream", "-c", "[.,input,inputs]"], b"[1,2] true"),
        0,
        b"[[[0],1],[[1],2],[[1]],[[],true]]\n",
    );
    expect(
        scratch.run(&["--stream", "-nc", "[inputs]", "a", "b"], b""),
        0,
        b"[[[0],1],[[0]],[[0],2],[[0]]]\n",
    );
}

#[test]
fn stream_slurp_collects_events_and_raw_input_takes_precedence() {
    let scratch = Scratch::new();
    expect(
        scratch.run(&["--stream", "-sc", "."], b"[1,2]"),
        0,
        b"[[[0],1],[[1],2],[[1]]]\n",
    );
    expect(
        scratch.run(&["--stream", "-nsc", "[inputs]"], b"[1,2]"),
        0,
        b"[[[[0],1],[[1],2],[[1]]]]\n",
    );
    expect(
        scratch.run(&["--stream-errors", "-Rc", "."], b"[broken]\nx\n"),
        0,
        b"\"[broken]\"\n\"x\"\n",
    );
    expect(
        scratch.run(&["--stream", "-Rsc", "."], b"a\nb\n"),
        0,
        b"\"a\\nb\\n\"\n",
    );
}

#[test]
fn stream_errors_include_jq_messages_paths_and_line_recovery() {
    let scratch = Scratch::new();
    expect(
        scratch.run(&["--stream-errors", "-c", "."], b"[1,x,2] 3\n4\n"),
        0,
        b"[[0],1]\n[\"Invalid numeric literal at line 1, column 5\",[1]]\n[[],4]\n",
    );
    for (input, expected) in [
        (
            b"[1,]".as_slice(),
            b"[[0],1]\n[\"Expected another array element at line 1, column 4\",[1]]\n".as_slice(),
        ),
        (
            b"{\"a\":}",
            b"[\"Missing value in key:value pair at line 1, column 6\",[\"a\"]]\n",
        ),
        (
            b"{\"a\":1,\"b\":",
            b"[[\"a\"],1]\n[\"Unfinished JSON term at EOF at line 1, column 11\",[\"b\"]]\n",
        ),
        (
            b"[1",
            b"[\"Unfinished JSON term at EOF at line 1, column 2\",[0]]\n",
        ),
        (
            b"\"a\\q\"\n1",
            b"[\"Invalid escape at line 1, column 5\",[]]\n[[],1]\n",
        ),
        (
            b"\"\\u0\"",
            b"[\"Invalid \\\\uXXXX escape at line 1, column 5\",[]]\n",
        ),
        (
            b"[1 2]",
            b"[\"Expected separator between values at line 1, column 5\",[0]]\n",
        ),
        (
            b"{\"a\"}",
            b"[\"Objects must consist of key:value pairs at line 1, column 5\",[null]]\n",
        ),
    ] {
        expect(
            scratch.run(&["--stream-errors", "-c", "."], input),
            0,
            expected,
        );
    }
    expect(
        scratch.run(&["--stream-errors", "-sc", "."], b"[1,x]\n2\n"),
        0,
        b"[[[0],1],[\"Invalid numeric literal at line 1, column 5\",[1]],[[],2]]\n",
    );
}

#[test]
fn ordinary_stream_errors_preserve_prefix_and_fail_instead_of_becoming_events() {
    let scratch = Scratch::new();
    let output = scratch.run(&["--stream", "-c", "."], b"[1,x]\n2\n");
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert_eq!(output.stdout, b"[[0],1]\n");
    assert!(output.stderr.starts_with(b"jq: parse error:"), "{output:?}");
}

#[test]
fn stream_sequence_mode_keeps_record_framing_and_recovers_after_truncation() {
    let scratch = Scratch::new();
    expect(
        scratch.run(
            &["--seq", "--stream-errors", "-c", "."],
            b"ignored\x1e[1\x1e[2]\n\x1e3\n",
        ),
        0,
        b"\x1e[\"Truncated value at line 1, column 11\",[0]]\n\x1e[[0],2]\n\x1e[[0]]\n\x1e[[],3]\n",
    );
    let output = scratch.run(&["--seq", "--stream", "-c", "."], b"\x1e[1,x]\n\x1e[2]\n");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"\x1e[[0],1]\n\x1e[[0],2]\n\x1e[[0]]\n");
    assert_eq!(
        output.stderr,
        b"jq: ignoring parse error: Invalid numeric literal at line 1, column 6 (need RS to resync)\n"
    );
}

#[test]
fn stream_exit_status_and_null_input_follow_filter_outputs() {
    let scratch = Scratch::new();
    expect(scratch.run(&["--stream", "-ec", "."], b""), 4, b"");
    expect(
        scratch.run(&["--stream", "-ec", "."], b"false"),
        0,
        b"[[],false]\n",
    );
    expect(
        scratch.run(&["--stream", "-ec", ".[1]"], b"false"),
        1,
        b"false\n",
    );
    expect(
        scratch.run(&["--stream-errors", "-ec", "."], b"["),
        0,
        b"[\"Unfinished JSON term at EOF at line 1, column 1\",[0]]\n",
    );
    expect(
        scratch.run(&["--stream", "-nc", "."], b"invalid"),
        0,
        b"null\n",
    );
    expect(
        scratch.run(&["--stream", "-nc", "try input catch ."], b""),
        0,
        b"\"break\"\n",
    );
}

#[test]
fn streaming_produces_a_valid_prefix_before_stdin_is_closed() {
    let scratch = Scratch::new();
    let mut child = scratch
        .command(&["--stream", "--unbuffered", "-c", "."])
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in io::BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    stdin.write_all(b"[0,").unwrap();
    stdin.flush().unwrap();
    let first = rx.recv_timeout(Duration::from_secs(5));
    if first.is_err() {
        drop(stdin);
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        reader.join().unwrap();
        panic!("no incremental event while stdin remained open: {first:?}; {output:?}");
    }
    let write = stdin.write_all(b"1]");
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    assert_eq!(first.unwrap().unwrap(), "[[0],0]");
    write.unwrap();
    expect(output, 0, b"");
    let remaining = rx.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(remaining, ["[[1],1]", "[[1]]"]);
}

#[test]
fn large_arrays_can_be_reduced_without_collecting_values_or_events() {
    let scratch = Scratch::new();
    let path = scratch.0.join("large.json");
    let mut file = io::BufWriter::new(fs::File::create(path).unwrap());
    file.write_all(b"[").unwrap();
    for _ in 0..100_000 {
        file.write_all(b"0,").unwrap();
    }
    file.write_all(b"0]").unwrap();
    file.flush().unwrap();
    expect(
        scratch.run(
            &[
                "--stream",
                "-nc",
                "reduce inputs as $event (0; .+1)",
                "large.json",
            ],
            b"",
        ),
        0,
        b"100002\n",
    );
}

#[test]
fn default_json_input_still_builds_values_and_stops_before_partial_values() {
    let scratch = Scratch::new();
    expect(
        scratch.run(&["-c", "."], b"{\"a\":[1,2]} [] 3"),
        0,
        b"{\"a\":[1,2]}\n[]\n3\n",
    );
    let output = scratch.run(&["-c", "."], b"[1,x]");
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn stream_error_recovery_preserves_jq_diagnostic_counters_and_file_boundaries() {
    let scratch = Scratch::new();
    expect(
        scratch.run(&["--stream-errors", "-c", "."], b"[1,x]\n[2,y]\n"),
        0,
        b"[[0],1]\n[\"Invalid numeric literal at line 1, column 5\",[1]]\n[[0],2]\n[\"Invalid numeric literal at line 1, column 10\",[1]]\n",
    );
    fs::write(scratch.0.join("bad"), "[1,x]").unwrap();
    fs::write(scratch.0.join("good"), "[2]").unwrap();
    expect(
        scratch.run(&["--stream-errors", "-c", ".", "bad", "good"], b""),
        0,
        b"[[0],1]\n[\"Invalid numeric literal at line 1, column 5\",[1]]\n[[0],2]\n[[0]]\n",
    );
}

#[test]
fn stream_string_tokens_and_paths_survive_reader_buffer_boundaries() {
    let scratch = Scratch::new();
    let key = format!("{}雪", "k".repeat(8191));
    let value = format!("{}\\\"\n😀", "v".repeat(8191));
    let input = serde_json::to_vec(&serde_json::json!({ (key.as_str()): [&value] })).unwrap();
    let output = scratch.run(&["--stream", "-c", "."], &input);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let events = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        [
            serde_json::json!([[&key, 0], &value]),
            serde_json::json!([[&key, 0]]),
            serde_json::json!([[&key]]),
        ]
    );
}
