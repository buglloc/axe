#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-jq-{label}-{}-{}",
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

fn run(scratch: &Scratch, args: &[&str], input: &[u8]) -> Output {
    run_os(scratch, args, input)
}

fn run_os(
    scratch: &Scratch,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    input: &[u8],
) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "jq", "--"])
        .args(args)
        .current_dir(scratch.path())
        .env_clear()
        .env("HOME", scratch.path())
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jq");
    if let Err(error) = child.stdin.take().expect("piped stdin").write_all(input) {
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().expect("wait for jq")
}

fn expect_output(output: Output, status: i32, stdout: &[u8]) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_eq!(output.stdout, stdout, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn json_input_replaces_invalid_utf8_in_strings_and_keys() {
    let scratch = Scratch::new("json-utf8");
    expect_output(
        run(
            &scratch,
            &["-c", "."],
            b"\"a\xffb\"\n{\"k\xff\":\"v\xfe\"}\n",
        ),
        0,
        "\"a\u{fffd}b\"\n{\"k\u{fffd}\":\"v\u{fffd}\"}\n".as_bytes(),
    );
}

#[test]
fn json_input_rejects_comments_byte_strings_and_nonstring_keys() {
    let scratch = Scratch::new("json-boundary");
    for input in [
        b"# comment\n1\n".as_slice(),
        b"[1,# comment\n2]\n",
        b"b\"abc\"\n",
        b"[b\"abc\"]\n",
        b"{1:2}\n",
        b"{\"nested\":{null:2}}\n",
    ] {
        let output = run(&scratch, &["-c", "."], input);
        assert_eq!(output.status.code(), Some(5), "{input:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{input:?}: {output:?}");
        assert!(output.stderr.starts_with(b"jq: parse error:"), "{output:?}");
    }
    let output = run(&scratch, &["-c", "."], b"1\n# comment\n2\n");
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert_eq!(output.stdout, b"1\n");
}

#[test]
fn strings_require_complete_unicode_surrogate_pairs() {
    let scratch = Scratch::new("surrogates");
    for input in [
        br#""\ud800""#.as_slice(),
        br#""\udc00""#,
        br#""\ud800\u0061""#,
        br#"{"\udfff":0}"#,
    ] {
        let output = run(&scratch, &["-c", "."], input);
        assert_eq!(output.status.code(), Some(5), "{input:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    expect_output(
        run(&scratch, &["-c", "."], br#""\ud83d\ude00""#),
        0,
        "\"😀\"\n".as_bytes(),
    );
}

#[test]
fn nonfinite_values_serialize_as_jq_json_at_every_depth() {
    let scratch = Scratch::new("nonfinite");
    let json = b"[null,1.7976931348623157e+308,-1.7976931348623157e+308,{\"n\":null}]\n";
    expect_output(
        run(
            &scratch,
            &["-c", "."],
            b"[NaN,Infinity,-Infinity,{\"n\":NaN}]\n",
        ),
        0,
        json,
    );
    expect_output(
        run(&scratch, &["-nc", "[nan,infinite,-infinite,{n:nan}]"], b""),
        0,
        json,
    );
    expect_output(
        run(
            &scratch,
            &["-nr", "[nan,infinite,-infinite,{n:nan}] | tojson"],
            b"",
        ),
        0,
        json,
    );
    expect_output(run(&scratch, &["-nr", "nan | tostring"], b""), 0, b"null\n");
}

#[test]
fn leading_plus_numbers_are_normalized_at_all_json_input_boundaries() {
    let scratch = Scratch::new("leading-plus");
    expect_output(run(&scratch, &["-c", "."], b"+1\n"), 0, b"1\n");
    expect_output(run(&scratch, &["-sc", "."], b"+1\n+2.5\n"), 0, b"[1,2.5]\n");
    let fixture = b"+1\n[+2.5,{\"n\":+3}]\n";
    fs::write(scratch.path().join("plus.json"), fixture).unwrap();
    expect_output(
        run(&scratch, &["-c", ".", "plus.json"], b""),
        0,
        b"1\n[2.5,{\"n\":3}]\n",
    );
    expect_output(
        run(
            &scratch,
            &["-nc", "--argjson", "number", "+1.5", "$number"],
            b"",
        ),
        0,
        b"1.5\n",
    );
    expect_output(
        run(
            &scratch,
            &["-nc", "--slurpfile", "values", "plus.json", "$values"],
            b"",
        ),
        0,
        b"[1,[2.5,{\"n\":3}]]\n",
    );
    expect_output(
        run(&scratch, &["-nc", r#""[+1,+2.5]" | fromjson"#], b""),
        0,
        b"[1,2.5]\n",
    );
    expect_output(run(&scratch, &["-Rc", "."], b"+1\n"), 0, b"\"+1\"\n");
}

#[test]
fn leading_plus_extension_does_not_accept_incomplete_or_repeated_signs() {
    let scratch = Scratch::new("leading-plus-invalid");
    for input in [b"+\n".as_slice(), b"++1\n", b"+-1\n", b"[+-1]\n"] {
        let output = run(&scratch, &["-c", "."], input);
        assert_eq!(output.status.code(), Some(5), "{input:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(!output.stderr.is_empty(), "{output:?}");
    }
}

#[test]
fn fromjson_uses_the_same_json_boundary_and_requires_one_value() {
    let scratch = Scratch::new("fromjson");
    for source in [
        r##""# comment\n1" | fromjson"##,
        r#""b\"abc\"" | fromjson"#,
        r#""{1:2}" | fromjson"#,
        r#""1 2" | fromjson"#,
        r#""\"\\ud800\"" | fromjson"#,
    ] {
        let output = run(&scratch, &["-nc", source], b"");
        assert_eq!(output.status.code(), Some(5), "{source}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    expect_output(
        run(&scratch, &["-nc", r#""[NaN,Infinity]" | fromjson"#], b""),
        0,
        b"[null,1.7976931348623157e+308]\n",
    );
}

#[test]
fn backend_only_values_never_escape_as_non_json_output() {
    let scratch = Scratch::new("output-boundary");
    for filter in ["{(1):2}", r#"["abc" | tobytes]"#] {
        let output = run(&scratch, &["-nc", filter], b"");
        assert_eq!(output.status.code(), Some(5), "{filter}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
}

#[test]
fn null_input_runs_once_without_opening_unused_operands() {
    let scratch = Scratch::new("lazy-null");
    fs::write(scratch.path().join("a"), b"1\n").unwrap();
    fs::write(scratch.path().join("b"), b"2\n").unwrap();
    expect_output(run(&scratch, &["-nc", ".", "a", "b"], b""), 0, b"null\n");
    expect_output(run(&scratch, &["-n", ".", "missing"], b""), 0, b"null\n");
    expect_output(run(&scratch, &["-ns", ".", "missing"], b""), 0, b"null\n");
    expect_output(
        run(&scratch, &["-nc", "input", "a", "missing"], b""),
        0,
        b"1\n",
    );
}

#[test]
fn input_and_inputs_share_one_lazy_stream_across_files_and_stdin() {
    let scratch = Scratch::new("one-stream");
    fs::write(scratch.path().join("a"), b"1\n").unwrap();
    fs::write(scratch.path().join("b"), b"3\n").unwrap();
    for args in [
        &["-nc", "[inputs]", "a", "-", "b"][..],
        &["-c", "[.,inputs]", "a", "-", "b"][..],
        &["-nc", "[input,input,input]", "a", "-", "b"][..],
    ] {
        expect_output(run(&scratch, args, b"2\n"), 0, b"[1,2,3]\n");
    }
    expect_output(run(&scratch, &["-sc", ".", "a", "b"], b""), 0, b"[1,3]\n");
    expect_output(
        run(&scratch, &["-nsc", "input", "a", "b"], b""),
        0,
        b"[1,3]\n",
    );
}

#[test]
fn input_filename_tracks_consumption_including_explicit_null_input() {
    let scratch = Scratch::new("filename");
    fs::write(scratch.path().join("a"), b"1\n2\n").unwrap();
    fs::write(scratch.path().join("empty"), b"").unwrap();
    fs::write(scratch.path().join("b"), b"3\n").unwrap();
    expect_output(
        run(
            &scratch,
            &[
                "-nc",
                "[input_filename,input,input_filename,input,input_filename,input,input_filename]",
                "a",
                "empty",
                "b",
            ],
            b"",
        ),
        0,
        b"[null,1,\"a\",2,\"a\",3,\"b\"]\n",
    );
    expect_output(
        run(&scratch, &["-c", "[.,input_filename]", "a", "b"], b""),
        0,
        b"[1,\"a\"]\n[2,\"a\"]\n[3,\"b\"]\n",
    );
    expect_output(
        run(
            &scratch,
            &["-nc", "[inputs|[.,input_filename]]", "a", "b"],
            b"",
        ),
        0,
        b"[[1,\"a\"],[2,\"a\"],[3,\"b\"]]\n",
    );
    expect_output(
        run(
            &scratch,
            &["-nc", "[input_filename,input,input_filename]"],
            b"4\n",
        ),
        0,
        b"[null,4,\"<stdin>\"]\n",
    );
}

#[test]
fn exit_status_retains_the_last_produced_value_across_empty_or_filtered_files() {
    let scratch = Scratch::new("exit-status");
    fs::write(scratch.path().join("a"), b"true\n").unwrap();
    fs::write(scratch.path().join("empty"), b"").unwrap();
    fs::write(scratch.path().join("false"), b"false\n").unwrap();
    expect_output(run(&scratch, &["-e", ".", "a", "empty"], b""), 0, b"true\n");
    expect_output(
        run(&scratch, &["-e", "select(.)", "a", "false"], b""),
        0,
        b"true\n",
    );
    expect_output(
        run(&scratch, &["-e", ".", "a", "false"], b""),
        1,
        b"true\nfalse\n",
    );
    expect_output(run(&scratch, &["-e", "empty", "a", "false"], b""), 4, b"");
}

#[test]
fn raw_input_preserves_cr_and_replaces_invalid_utf8_in_lines_and_slurp() {
    let scratch = Scratch::new("raw");
    let input = b"one\r\ntw\xffo\nlast\r";
    expect_output(
        run(&scratch, &["-Rc", "."], input),
        0,
        "\"one\\r\"\n\"tw\u{fffd}o\"\n\"last\\r\"\n".as_bytes(),
    );
    expect_output(
        run(&scratch, &["-Rsc", "."], input),
        0,
        "\"one\\r\\ntw\u{fffd}o\\nlast\\r\"\n".as_bytes(),
    );
    fs::write(scratch.path().join("first"), [0xc3]).unwrap();
    fs::write(scratch.path().join("second"), [0xa9]).unwrap();
    expect_output(
        run(&scratch, &["-Rsc", ".", "first", "second"], b""),
        0,
        "\"é\"\n".as_bytes(),
    );
}

#[test]
fn rawfile_and_unix_pathnames_do_not_require_utf8() {
    let scratch = Scratch::new("path-bytes");
    let path = OsString::from_vec(b"file-\xff".to_vec());
    fs::write(scratch.path().join(&path), b"1\n").unwrap();
    expect_output(
        run_os(
            &scratch,
            [
                OsStr::new("-c"),
                OsStr::new("[.,input_filename]"),
                path.as_os_str(),
            ],
            b"",
        ),
        0,
        "[1,\"file-\u{fffd}\"]\n".as_bytes(),
    );
    expect_output(
        run_os(
            &scratch,
            [
                OsStr::new("-nc"),
                OsStr::new("--slurpfile"),
                OsStr::new("values"),
                path.as_os_str(),
                OsStr::new("$values"),
            ],
            b"",
        ),
        0,
        b"[1]\n",
    );
    fs::write(scratch.path().join(&path), b"one\xfftwo\r\n").unwrap();
    expect_output(
        run_os(
            &scratch,
            [
                OsStr::new("-nc"),
                OsStr::new("--rawfile"),
                OsStr::new("raw"),
                path.as_os_str(),
                OsStr::new("$raw"),
            ],
            b"",
        ),
        0,
        "\"one\u{fffd}two\\r\\n\"\n".as_bytes(),
    );
}

#[test]
fn compile_input_runtime_and_file_errors_keep_distinct_exit_classes() {
    let scratch = Scratch::new("errors");
    for (args, input, status) in [
        (&[".["][..], b"null\n".as_slice(), 3),
        (&["."][..], b"{\n".as_slice(), 5),
        (&["error(\"oops\")"][..], b"null\n".as_slice(), 5),
        (&["-n", "input", "missing"][..], b"".as_slice(), 2),
        (&["-n", "input"][..], b"".as_slice(), 5),
    ] {
        let output = run(&scratch, args, input);
        assert_eq!(output.status.code(), Some(status), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(!output.stderr.is_empty(), "{output:?}");
    }
}

#[test]
fn halt_error_writes_its_payload_without_a_prefix_or_newline() {
    let scratch = Scratch::new("halt");
    for (filter, status, stderr) in [
        (r#""oops" | halt_error(7)"#, 7, b"oops".as_slice()),
        (
            r#"{why:"oops"} | halt_error(8)"#,
            8,
            b"{\"why\":\"oops\"}".as_slice(),
        ),
        ("null | halt_error(9)", 9, b"".as_slice()),
        ("halt", 0, b"".as_slice()),
    ] {
        let output = run(&scratch, &["-n", filter], b"");
        assert_eq!(output.status.code(), Some(status), "{filter}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(output.stderr, stderr, "{filter}: {output:?}");
    }
}

#[test]
fn named_and_positional_arguments_are_available_in_args() {
    let scratch = Scratch::new("args");
    let output = run(
        &scratch,
        &[
            "-nc",
            "--arg",
            "x",
            "one",
            "--arg",
            "x",
            "ignored",
            "--argjson",
            "n",
            "[1,2]",
            "--args",
            "$ARGS",
            "alpha",
            "two words",
            "--",
            "-three",
        ],
        b"",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"named":{"x":"one","n":[1,2]},"positional":["alpha","two words","-three"]}),
    );
    for argument in ["# comment\n1", "b\"abc\"", "{1:2}"] {
        let output = run(
            &scratch,
            &["-n", "--argjson", "value", argument, "$value"],
            b"",
        );
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
}

#[test]
fn filter_files_and_ascii_output_follow_the_normal_jq_contract() {
    let scratch = Scratch::new("file-ascii");
    let filter_path = OsString::from_vec(b"filter-\xff.jq".to_vec());
    fs::write(
        scratch.path().join(&filter_path),
        "{\"café\": .x, face: \"😀\"}\n",
    )
    .unwrap();
    expect_output(
        run_os(
            &scratch,
            [OsStr::new("-caf"), filter_path.as_os_str()],
            "{\"x\":\"é\"}\n".as_bytes(),
        ),
        0,
        b"{\"caf\\u00e9\":\"\\u00e9\",\"face\":\"\\ud83d\\ude00\"}\n",
    );
    expect_output(run(&scratch, &["-nra", "\"é\""], b""), 0, b"\"\\u00e9\"\n");
    fs::write(scratch.path().join("filter.jq"), ". + 1\n").unwrap();
    fs::write(scratch.path().join("data.json"), "1\n2\n").unwrap();
    expect_output(
        run(
            &scratch,
            &["-c", "--from-file", "filter.jq", "data.json"],
            b"",
        ),
        0,
        b"2\n3\n",
    );
}

#[test]
fn stream_options_are_explicitly_unsupported_instead_of_misparsed_filters() {
    let scratch = Scratch::new("stream-option");
    let output = run(&scratch, &["--stream", "."], b"[1]\n");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not supported"),
        "{output:?}"
    );
}

#[test]
fn from_file_is_a_mode_flag_and_preserves_the_program_operand() {
    let scratch = Scratch::new("file-mode");
    fs::write(scratch.path().join("identity.jq"), ".\n").unwrap();
    for args in [
        &["-fn", "identity.jq"][..],
        &["-fcn", "identity.jq"][..],
        &["-fc", "--null-input", "identity.jq"][..],
        &["-f", "-n", "-c", "identity.jq"][..],
        &["--from-file", "--null-input", "identity.jq"][..],
        &["identity.jq", "-fn"][..],
    ] {
        expect_output(run(&scratch, args, b""), 0, b"null\n");
    }
    fs::write(scratch.path().join("binding.jq"), "$x\n").unwrap();
    expect_output(
        run(
            &scratch,
            &["-f", "--arg", "x", "one", "-nc", "binding.jq"],
            b"",
        ),
        0,
        b"\"one\"\n",
    );
    let output = run(&scratch, &["-f", "-n"], b"");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(!output.stderr.is_empty(), "{output:?}");
}

#[test]
fn halt_error_rejects_noninteger_and_out_of_range_status_values() {
    let scratch = Scratch::new("halt-status");
    for filter in [
        r#"null | halt_error("seven")"#,
        "null | halt_error(2147483648)",
    ] {
        let output = run(&scratch, &["-n", filter], b"");
        assert_eq!(output.status.code(), Some(5), "{filter}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(!output.stderr.is_empty(), "{output:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn output_flush_failure_overrides_a_successful_halt() {
    use std::os::unix::fs::MetadataExt;

    let scratch = Scratch::new("flush-halt");
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    assert_eq!(full.metadata().unwrap().rdev(), libc::makedev(1, 7));
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "jq", "--", "-n", "1, halt"])
        .current_dir(scratch.path())
        .env_clear()
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(full)
        .stderr(Stdio::piped())
        .output()
        .expect("run jq with a failing output sink");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.starts_with(b"jq:"), "{output:?}");
}
