use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "axe-json-diff-repair-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, applet: &str, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", applet, "--"])
            .args(args)
            .current_dir(&self.0)
            .env_clear()
            .env("HOME", &self.0)
            .env("LC_ALL", "C")
            .env("PATH", "/nonexistent")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Err(error) = child.stdin.take().unwrap().write_all(input) {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        child.wait_with_output().unwrap()
    }

    fn diff(&self, args: &[&str], left: &[u8], right: &[u8]) -> Output {
        fs::write(self.0.join("left"), left).unwrap();
        fs::write(self.0.join("right"), right).unwrap();
        let mut args = args.to_vec();
        args.extend(["left", "right"]);
        self.run("diff", &args, b"")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn expect_output(output: Output, status: i32, stdout: &[u8]) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_eq!(output.stdout, stdout, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn diff_blank_lines_are_empty_unless_trailing_whitespace_is_ignored() {
    let scratch = Scratch::new();
    expect_output(
        scratch.diff(&["-B"], b"\n", b" \n"),
        1,
        b"1c1\n< \n---\n>  \n",
    );
    expect_output(scratch.diff(&["-B"], b"", b"\n"), 0, b"");
    for option in ["-w", "-b", "-Z"] {
        expect_output(scratch.diff(&["-B", option], b"", b" \t\n"), 0, b"");
    }
    expect_output(
        scratch.diff(&["-B"], b"\n", b"\r\n"),
        1,
        b"1c1\n< \n---\n> \r\n",
    );
    expect_output(
        scratch.diff(&["-B", "--strip-trailing-cr"], b"\n", b"\r\n"),
        0,
        b"",
    );
}

#[test]
fn diff_ignoring_trailing_space_also_ignores_final_newline_presence() {
    let scratch = Scratch::new();
    for (left, right) in [
        (b"a".as_slice(), b"a\n".as_slice()),
        (b"a \t", b"a\n"),
        (b"a\n", b"a \t"),
    ] {
        expect_output(scratch.diff(&["-Z"], left, right), 0, b"");
    }
    expect_output(
        scratch.diff(&["-Z"], b"a b\n", b"ab\n"),
        1,
        b"1c1\n< a b\n---\n> ab\n",
    );
}

#[test]
fn diff_labels_name_binary_and_brief_reports() {
    let scratch = Scratch::new();
    expect_output(
        scratch.diff(&["--label=L", "--label=R"], b"a\0", b"b\0"),
        1,
        b"Binary files L and R differ\n",
    );
    for (left, right) in [(b"a\0".as_slice(), b"b\0".as_slice()), (b"a\n", b"b\n")] {
        expect_output(
            scratch.diff(&["-q", "--label=L", "--label=R"], left, right),
            1,
            b"Files L and R differ\n",
        );
    }
    expect_output(
        scratch.diff(&["-s", "--label=L", "--label=R"], b"a\n", b"a\n"),
        0,
        b"Files L and R are identical\n",
    );
    expect_output(
        scratch.diff(&["--label=L"], b"a\0", b"b\0"),
        1,
        b"Binary files L and right differ\n",
    );
}

#[test]
fn sdiff_binary_comparisons_are_results_not_errors() {
    let scratch = Scratch::new();
    fs::write(scratch.0.join("left"), b"a\0b\n").unwrap();
    for (right, status, expected) in [
        (b"a\0b\n".as_slice(), 0, b"".as_slice()),
        (
            b"a\0c\n".as_slice(),
            1,
            b"Binary files left and right differ\n".as_slice(),
        ),
    ] {
        fs::write(scratch.0.join("right"), right).unwrap();
        expect_output(
            scratch.run("sdiff", &["left", "right"], b""),
            status,
            expected,
        );
        fs::write(scratch.0.join("out"), b"old").unwrap();
        expect_output(
            scratch.run("sdiff", &["-o", "out", "left", "right"], b""),
            status,
            expected,
        );
        assert_eq!(fs::read(scratch.0.join("out")).unwrap(), b"");
    }
    let failure = scratch.run("sdiff", &["-o", "missing/out", "left", "right"], b"");
    assert_eq!(failure.status.code(), Some(2));
    assert!(failure.stdout.is_empty());
    assert!(!failure.stderr.is_empty());
    let text = scratch.run("sdiff", &["-a", "left", "right"], b"");
    assert_eq!(text.status.code(), Some(1));
    assert!(text.stderr.is_empty());
    assert!(text.stdout.contains(&0));
}

#[test]
fn jq_slicing_null_returns_null() {
    let scratch = Scratch::new();
    expect_output(
        scratch.run(
            "jq",
            &["-c", "[.[-1:], .[:2], .[0:1], .[:], .[\"bad\":]]"],
            b"null\n",
        ),
        0,
        b"[null,null,null,null,null]\n",
    );
    expect_output(
        scratch.run("jq", &["-c", "try (.[-1:]) catch \"bad\""], b"null\n"),
        0,
        b"null\n",
    );
}

#[test]
fn jq_global_regex_preserves_adjacent_and_trailing_empty_matches() {
    let scratch = Scratch::new();
    expect_output(
        scratch.run(
            "jq",
            &[
                "-cn",
                "[\"aa\" | match(\"a*\"; \"g\") | [.offset, .length, .string]]",
            ],
            b"",
        ),
        0,
        b"[[0,2,\"aa\"],[2,0,\"\"]]\n",
    );
    expect_output(
        scratch.run(
            "jq",
            &[
                "-cn",
                "[\"a猫\" | match(\"a*\"; \"g\") | [.offset, .length, .string]]",
            ],
            b"",
        ),
        0,
        b"[[0,1,\"a\"],[1,0,\"\"],[2,0,\"\"]]\n",
    );
    expect_output(
        scratch.run(
            "jq",
            &["-cn", "[\"aa\" | match(\"a*\"; \"gn\") | .string]"],
            b"",
        ),
        0,
        b"[\"aa\"]\n",
    );
    expect_output(
        scratch.run("jq", &["-cn", "\"aa\" | gsub(\"a*\"; \"X\")"], b""),
        0,
        b"\"XX\"\n",
    );
}

#[test]
fn jq_regex_multiline_flag_changes_dot_not_anchors() {
    let scratch = Scratch::new();
    expect_output(
        scratch.run("jq", &["-cn", "[\"a\\nb\" | splits(\"^\"; \"m\")]"], b""),
        0,
        b"[\"\",\"a\\nb\"]\n",
    );
    expect_output(
        scratch.run(
            "jq",
            &["-cn", "\"a\\nb\" | [test(\"a.b\"), test(\"a.b\"; \"m\"), test(\"a.b\"; \"s\"), test(\"a.b\"; \"p\"), test(\"^b\"; \"m\")]"],
            b"",
        ),
        0,
        b"[false,true,false,true,false]\n",
    );
}

#[test]
fn jq_sequence_input_requires_initial_framing_and_complete_numbers() {
    let scratch = Scratch::new();
    expect_output(
        scratch.run("jq", &["-ce", "--seq", "."], b"false\ntrue\n"),
        4,
        b"",
    );
    expect_output(
        scratch.run("jq", &["-c", "--seq", "."], b"ignored\n\x1e1\n\x1e2"),
        0,
        b"\x1e1\n",
    );
    expect_output(
        scratch.run("jq", &["-c", "--seq", "."], b"\x1e1\x1e2\n"),
        0,
        b"\x1e2\n",
    );
    expect_output(
        scratch.run("jq", &["-c", "--seq", "."], b"\x1e1 \x1e2\n"),
        0,
        b"\x1e1\n\x1e2\n",
    );
    expect_output(
        scratch.run("jq", &["-c", "--seq", "."], b"\x1e[1]\n\x1e\"ok\""),
        0,
        b"\x1e[1]\n\x1e\"ok\"\n",
    );
    expect_output(scratch.run("jq", &["-c", "."], b"2"), 0, b"2\n");
}

#[test]
fn jq_arithmetic_rounds_large_integer_operands_before_computing() {
    let scratch = Scratch::new();
    expect_output(
        scratch.run(
            "jq",
            &["-cn", "[9007199254740993, (9007199254740993 + 1), (9007199254740993 - 1), (9007199254740993 * 1), (9007199254740992 + 1 + 1)]"],
            b"",
        ),
        0,
        b"[9007199254740993,9007199254740992,9007199254740991,9007199254740992,9007199254740992]\n",
    );
    expect_output(
        scratch.run(
            "jq",
            &["-c", "[., (. + 1), (. - 1), (. * 1)]"],
            b"9007199254740993\n",
        ),
        0,
        b"[9007199254740993,9007199254740992,9007199254740991,9007199254740992]\n",
    );
    expect_output(
        scratch.run("jq", &["-cn", "[10,20,30][1 + 1]"], b""),
        0,
        b"30\n",
    );
    expect_output(
        scratch.run(
            "jq",
            &[
                "-cn",
                "1000000000000000000000000 - 1000000000000000000000000",
            ],
            b"",
        ),
        0,
        b"0\n",
    );
}

#[test]
fn jq_unbounded_slice_is_a_slice_not_iteration() {
    let scratch = Scratch::new();
    expect_output(
        scratch.run(
            "jq",
            &["-c", ".[:]"],
            "[1,2]\n[]\n\"猫a\"\nnull\n".as_bytes(),
        ),
        0,
        "[1,2]\n[]\n\"猫a\"\nnull\n".as_bytes(),
    );
    expect_output(
        scratch.run("jq", &["-c", "[.[:], .[]]"], b"[1,2]\n"),
        0,
        b"[[1,2],1,2]\n",
    );
    expect_output(
        scratch.run("jq", &["-c", ".[:] |= reverse"], b"[1,2]\n"),
        0,
        b"[2,1]\n",
    );
    expect_output(
        scratch.run("jq", &["-c", "try .[:] catch \"bad\""], b"{}\n"),
        0,
        b"\"bad\"\n",
    );
}
