#![cfg(unix)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "axe-core-text-{}-{}",
                std::process::id(),
                NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create text scratch directory: {error}"),
            }
        }
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

fn run(root: &Path, applet: &str, args: &[&str], input: &[u8]) -> Output {
    run_with_locale(root, applet, args, input, "C")
}

fn run_with_locale(root: &Path, applet: &str, args: &[&str], input: &[u8], locale: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", applet, "--"])
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("LANG", locale)
        .env("LC_ALL", locale)
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn text applet");
    let mut stdin = child.stdin.take().expect("piped stdin");
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = child.wait_with_output().expect("wait for text applet");
        if let Err(error) = writer.join().expect("stdin writer panicked") {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        output
    })
}

fn assert_success(output: Output, expected: &[u8]) {
    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, expected);
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
}

#[test]
fn csplit_numeric_and_regexp_records_preserve_bytes() {
    let input = b"alpha\0beta\n\xff\x80\tend\r\nlast";
    for pattern in ["2", "/end/", "/last/-1"] {
        let scratch = Scratch::new();
        fs::write(scratch.path().join("input"), input).unwrap();
        assert_success(
            run(scratch.path(), "csplit", &["input", pattern], b""),
            b"11\n12\n",
        );
        assert_eq!(fs::read(scratch.path().join("xx00")).unwrap(), &input[..11]);
        assert_eq!(fs::read(scratch.path().join("xx01")).unwrap(), &input[11..]);
        assert_eq!(fs::read(scratch.path().join("input")).unwrap(), input);
    }
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "csplit", &["-", "2"], b"a\n\xff\n"),
        b"2\n2\n",
    );
    assert_eq!(fs::read(scratch.path().join("xx00")).unwrap(), b"a\n");
    assert_eq!(fs::read(scratch.path().join("xx01")).unwrap(), b"\xff\n");
}

#[test]
fn csplit_error_removes_parts_unless_keep_files_is_requested() {
    let input = b"a\n\xff\0\nb\n";
    for pattern in ["9", "/missing/"] {
        for keep in [false, true] {
            let scratch = Scratch::new();
            fs::write(scratch.path().join("input"), input).unwrap();
            let args = if keep {
                vec!["-k", "input", "2", pattern]
            } else {
                vec!["input", "2", pattern]
            };
            let output = run(scratch.path(), "csplit", &args, b"");
            assert_eq!(output.status.code(), Some(1));
            assert!(!output.stderr.is_empty());
            if keep {
                assert_eq!(fs::read(scratch.path().join("xx00")).unwrap(), &input[..2]);
                assert_eq!(fs::read(scratch.path().join("xx01")).unwrap(), &input[2..]);
            } else {
                assert!(!scratch.path().join("xx00").exists());
                assert!(!scratch.path().join("xx01").exists());
            }
            assert_eq!(fs::read(scratch.path().join("input")).unwrap(), input);
        }
    }
}

#[test]
fn csplit_suppress_matched_discards_only_the_matched_record() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("input"), b"a\0\n\xff end\nlast").unwrap();
    assert_success(
        run(
            scratch.path(),
            "csplit",
            &["--suppress-matched", "input", "/end/"],
            b"",
        ),
        b"3\n4\n",
    );
    assert_eq!(fs::read(scratch.path().join("xx00")).unwrap(), b"a\0\n");
    assert_eq!(fs::read(scratch.path().join("xx01")).unwrap(), b"last");
}

#[test]
fn pr_merge_uses_separator_and_stops_at_last_record() {
    let scratch = Scratch::new();
    for (a, b, expected) in [
        (&b"a\nb\nc\n"[..], &b"1\n2\n"[..], &b"a|1\nb|2\nc|\n"[..]),
        (&b"a\n"[..], &b"1\n"[..], &b"a|1\n"[..]),
        (&b""[..], &b"1\n"[..], &b"|1\n"[..]),
        (&b""[..], &b""[..], &b""[..]),
        (&b"\n"[..], &b"\n"[..], &b"|\n"[..]),
    ] {
        fs::write(scratch.path().join("a"), a).unwrap();
        fs::write(scratch.path().join("b"), b).unwrap();
        for flag in ["-t", "-T"] {
            assert_success(
                run(scratch.path(), "pr", &[flag, "-m", "-s|", "a", "b"], b""),
                expected,
            );
        }
    }
    let mut a = Vec::new();
    let mut b = Vec::new();
    let mut expected = Vec::new();
    for i in 0..67 {
        writeln!(a, "a{i}").unwrap();
        writeln!(b, "b{i}").unwrap();
        writeln!(expected, "a{i}|b{i}").unwrap();
    }
    fs::write(scratch.path().join("a"), a).unwrap();
    fs::write(scratch.path().join("b"), b).unwrap();
    assert_success(
        run(scratch.path(), "pr", &["-t", "-m", "-s|", "a", "b"], b""),
        &expected,
    );
}

#[test]
fn pr_column_positions_and_line_number_tabs_match_gnu() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "pr", &["-t", "-2", "-w10"], b"a\nb\n"),
        b"a    b\n",
    );
    assert_success(
        run(
            scratch.path(),
            "pr",
            &["-t", "-a", "-2", "-n:", "-w30"],
            b"alpha\nbeta\ngamma\ndelta\nepsilon\n",
        ),
        b"    1:alpha    \t   2:beta\n    3:gamma    \t   4:delta\n    5:epsilon\n",
    );
    assert_success(
        run(
            scratch.path(),
            "pr",
            &["-t", "-2", "-w20"],
            &b"alpha\nbeta\ngamma\ndelta\nepsilon\n".repeat(2),
        ),
        b"alpha\t  alpha\nbeta\t  beta\ngamma\t  gamma\ndelta\t  delta\nepsilon\t  epsilon\n",
    );
}

#[test]
fn pr_numbering_does_not_shift_untruncated_input_tabs() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "pr", &["-t", "-n:", "-e8"], b"\tX\nA\tB\n"),
        b"    1:        X\n    2:A       B\n",
    );
}

#[test]
fn pr_preserves_invalid_bytes_and_renders_requested_controls() {
    let scratch = Scratch::new();
    assert_success(run(scratch.path(), "pr", &["-t"], b"\xff\n"), b"\xff\n");
    assert_success(run(scratch.path(), "pr", &["-t"], b"a\0b\n"), b"a\0b\n");
    assert_success(
        run(scratch.path(), "pr", &["-t", "-c"], b"a\0b\x01\n"),
        b"a^@b^A\n",
    );
    assert_success(
        run(
            scratch.path(),
            "pr",
            &["-t", "-v"],
            b"alpha\0beta\n\xff\x80\tend\r\nlast",
        ),
        b"alpha\\000beta\n\\377\\200\tend\\015\nlast\n",
    );
}

#[test]
fn ptx_joins_context_and_preserves_invalid_bytes() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "ptx", &["-w20"], b"a \xffb\n"),
        b"             a \xffb\n       a \xff   b\n",
    );
    assert_success(
        run(scratch.path(), "ptx", &["-w20"], b"alpha beta\n"),
        b"             alpha/\n     alpha   beta\n",
    );
    assert_success(
        run(scratch.path(), "ptx", &["-w20"], b"a b\nc d\n"),
        b"             a b c d\n         a   b c d\n       a b   c d\n     a b c   d\n",
    );
}

#[test]
fn ptx_word_regexp_whitespace_does_not_invert_context_ranges() {
    let scratch = Scratch::new();
    for (width, pattern, input, expected) in [
        (
            "-w10",
            "[ab] *",
            &b"a b\n"[..],
            &b"        a b\n    /   b\n"[..],
        ),
        (
            "-w16",
            "[ab]+ *",
            &b"aaaa    b\n"[..],
            &b"           aaaa/\n       /   b\n"[..],
        ),
    ] {
        assert_success(
            run(scratch.path(), "ptx", &[width, "-W", pattern], input),
            expected,
        );
    }
}

#[test]
fn ptx_references_and_word_filters_keep_full_context() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("input"), b"a b\n").unwrap();
    assert_success(
        run(scratch.path(), "ptx", &["-A", "-w30", "input"], b""),
        b"input:1:            a b\ninput:1:        a   b\n",
    );
    fs::write(scratch.path().join("words"), b"b\n").unwrap();
    assert_success(
        run(
            scratch.path(),
            "ptx",
            &["-o", "words", "-w20"],
            b"a b\nc d\n",
        ),
        b"         a   b c d\n",
    );
    assert_success(
        run(
            scratch.path(),
            "ptx",
            &["-i", "words", "-w20"],
            b"a b\nc d\n",
        ),
        b"             a b c d\n       a b   c d\n     a b c   d\n",
    );
}

#[test]
fn ptx_wraparound_truncation_and_explicit_context_match_gnu() {
    let scratch = Scratch::new();
    for (args, input, expected) in [
        (
            vec![],
            &b"alpha beta gamma\nbeta delta\n"[..],
            &b"                                       alpha beta gamma beta delta\n                               alpha   beta gamma beta delta\n                    alpha beta gamma   beta delta\n               alpha beta gamma beta   delta\n                          alpha beta   gamma beta delta\n"[..],
        ),
        (
            vec!["-f", "-w40", "-g2"],
            &b"Alpha beta GAMMA\nbeta delta\n"[..],
            &b"  beta delta          Alpha beta GAMMA\n  delta        Alpha  beta GAMMA beta\n    Alpha beta GAMMA  beta delta\n     beta GAMMA beta  delta          Alpha\n          Alpha beta  GAMMA beta delta\n"[..],
        ),
        (
            vec!["-w40"],
            &b"alpha\0beta \xffgamma\n"[..],
            &b"                       alpha\0beta \xffgamma\n              alpha\0   beta \xffgamma\n        alpha\0beta \xff   gamma\n"[..],
        ),
        (
            vec!["-S", "\n", "-w100"],
            &b"a b\nc d\n"[..],
            &b"                                                     a b\n                                                 a   b\n                                                     c d\n                                                 c   d\n"[..],
        ),
    ] {
        assert_success(run(scratch.path(), "ptx", &args, input), expected);
    }
    fs::write(scratch.path().join("input"), b"alpha beta\ngamma delta\n").unwrap();
    assert_success(
        run(scratch.path(), "ptx", &["-A", "-w50", "input"], b""),
        b"input:1:  delta               alpha beta gamma\ninput:1:              alpha   beta gamma delta\ninput:2:         beta gamma   delta          alpha\ninput:2:         alpha beta   gamma delta\n",
    );
    fs::write(
        scratch.path().join("input"),
        b"alpha beta gamma\nbeta delta\n",
    )
    .unwrap();
    fs::write(scratch.path().join("words"), b"beta\n").unwrap();
    assert_success(
        run(
            scratch.path(),
            "ptx",
            &["-o", "words", "-w40", "input"],
            b"",
        ),
        b"   delta       alpha   beta gamma beta\n          beta gamma   beta delta     alpha\n",
    );
    assert_success(
        run(scratch.path(), "ptx", &["-i", "words", "-w40", "input"], b""),
        b"   beta delta          alpha beta gamma\n     beta gamma beta   delta          alpha\n          alpha beta   gamma beta delta\n",
    );
}

#[test]
fn tab_conversion_preserves_zero_width_nul() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "expand", &[], b"\0\tX\n"),
        b"\0        X\n",
    );
    assert_success(
        run(scratch.path(), "unexpand", &["-a", "-t4"], b"\0    X\n"),
        b"\0\tX\n",
    );
    let mut input = vec![b'a'; 4095];
    input.extend_from_slice(b"\0\tX\n");
    let mut expected = vec![b'a'; 4095];
    expected.extend_from_slice(b"\0 X\n");
    assert_success(run(scratch.path(), "expand", &[], &input), &expected);
}

#[test]
fn fold_mixed_invalid_bytes_do_not_change_control_widths() {
    let scratch = Scratch::new();
    for (args, input, expected) in [
        (&["-w2"][..], &b"a\0bc\n"[..], &b"a\0b\nc\n"[..]),
        (&["-w2"][..], &b"a\0b\xff\n"[..], &b"a\0b\n\xff\n"[..]),
        (&["-b", "-w2"][..], &b"a\0b\xff\n"[..], &b"a\0\nb\xff\n"[..]),
        (
            &["-s", "-w4"][..],
            &b"a b\0\xffcd\n"[..],
            &b"a \nb\0\xffcd\n"[..],
        ),
        (
            &["-w4"][..],
            &b"alpha\0beta\n\xff\x80\tend\r\nlast"[..],
            &b"alph\na\0bet\na\n\xff\x80\n\t\nend\r\nlast"[..],
        ),
    ] {
        assert_success(run(scratch.path(), "fold", args, input), expected);
    }
    assert_success(
        run(
            scratch.path(),
            "fold",
            &["-w2"],
            &b"a\0b\xff\n".repeat(2000),
        ),
        &b"a\0b\n\xff\n".repeat(2000),
    );
}

#[test]
fn fold_rechecks_retained_columns_before_appending_a_wide_character() {
    let scratch = Scratch::new();
    for (input, expected) in [
        (" ab界\n", " \nab\n界\n"),
        (" ab界界\n", " \nab\n界\n界\n"),
        (" ab\u{301}界\n", " \nab\u{301}\n界\n"),
    ] {
        assert_success(
            run_with_locale(
                scratch.path(),
                "fold",
                &["-s", "-w3"],
                input.as_bytes(),
                "C.UTF-8",
            ),
            expected.as_bytes(),
        );
    }
}

#[test]
fn fmt_prefix_width_and_goal_preserve_gnu_line_breaks() {
    let scratch = Scratch::new();
    for args in [vec!["-p", "> ", "-w20"], vec!["-p", "> ", "-w20", "-g18"]] {
        assert_success(
            run(
                scratch.path(),
                "fmt",
                &args,
                b"> one two three four five\n> six seven eight\n",
            ),
            b"> one two three\n> four five six\n> seven eight\n",
        );
    }
    assert_success(
        run(
            scratch.path(),
            "fmt",
            &["-w20"],
            b"one two three four five\nsix seven eight\n",
        ),
        b"one two three four\nfive six seven eight\n",
    );
    assert_success(
        run(
            scratch.path(),
            "fmt",
            &["-p", "> ", "-w10"],
            b"> aa bbbbb\n",
        ),
        b"> aa bbbbb\n",
    );
    assert_success(
        run(scratch.path(), "fmt", &["-w8"], b"one four\n"),
        b"one four\n",
    );
    assert_success(
        run(scratch.path(), "fmt", &["-g8"], b"one four five\n"),
        b"one four five\n",
    );
}

#[test]
fn fmt_preserves_other_paragraph_spacing_and_binary_contracts() {
    let scratch = Scratch::new();
    for (args, input, expected) in [
        (
            vec!["-w25"],
            &b"one two three four five six seven eight nine ten\ncontinued line\n\n  indented text here and again more\n"[..],
            &b"one two three four five\nsix seven eight nine ten\ncontinued line\n\n  indented text here and\n  again more\n"[..],
        ),
        (
            vec!["-s", "-w15"],
            &b"one two\nthree four five six seven\n"[..],
            &b"one two\nthree four five\nsix seven\n"[..],
        ),
        (
            vec!["-u", "-w30"],
            &b"one   two.  Three   four\n"[..],
            &b"one two.  Three four\n"[..],
        ),
        (
            vec!["-w10"],
            &b"one\0two three\xff four\n"[..],
            &b"one\0two\nthree\xff\nfour\n"[..],
        ),
    ] {
        assert_success(run(scratch.path(), "fmt", &args, input), expected);
    }
}

#[test]
fn base_decoding_errors_emit_only_the_valid_prefix() {
    let scratch = Scratch::new();
    for (applet, args, input, expected) in [
        ("base32", vec!["-d"], &b"MZXW!6===\n"[..], &b"fo"[..]),
        ("base32", vec!["-d"], &b"MZXW6!YTB\n"[..], &b"foo"[..]),
        ("base64", vec!["-d"], &b"Zm9v!YmFy\n"[..], &b"foo"[..]),
        ("base64", vec!["-d"], &b"Zm9!\n"[..], &b"fo"[..]),
        ("base64", vec!["-d"], &b"Zm9vY\n"[..], &b"foo"[..]),
        (
            "basenc",
            vec!["--base64", "-d"],
            &b"Zm9v!YmFy\n"[..],
            &b"foo"[..],
        ),
        (
            "basenc",
            vec!["--base32hex", "-d"],
            &b"CPNM!U===\n"[..],
            &b"fo"[..],
        ),
        (
            "basenc",
            vec!["--base64url", "-d"],
            &b"Zm9v!YmFy\n"[..],
            &b"foo"[..],
        ),
        (
            "basenc",
            vec!["--base16", "-d"],
            &b"666f!6f\n"[..],
            &b"fo"[..],
        ),
        (
            "basenc",
            vec!["--base2msbf", "-d"],
            &b"0110011001101111!01101111\n"[..],
            &b"fo"[..],
        ),
    ] {
        let output = run(scratch.path(), applet, &args, input);
        assert_eq!(output.status.code(), Some(1), "{applet}");
        assert_eq!(output.stdout, expected, "{applet}");
        assert!(
            output.stderr.ends_with(b"invalid input\n"),
            "{applet}: {:?}",
            output.stderr
        );
        fs::write(scratch.path().join("input"), input).unwrap();
        let mut file_args = args;
        file_args.push("input");
        let output = run(scratch.path(), applet, &file_args, b"");
        assert_eq!(output.status.code(), Some(1), "{applet} file");
        assert_eq!(output.stdout, expected, "{applet} file");
        assert!(
            output.stderr.ends_with(b"invalid input\n"),
            "{applet} file: {:?}",
            output.stderr
        );
        assert_eq!(fs::read(scratch.path().join("input")).unwrap(), input);
    }
    assert_success(
        run(scratch.path(), "base64", &["-di"], b"Zm9v!YmFy\n"),
        b"foobar",
    );
    assert_success(
        run(scratch.path(), "base32", &["-di"], b"MZXW!6===\n"),
        b"foo",
    );
    let mut input = b"Zm9v".repeat(4097);
    input.extend_from_slice(b"!YmFy\n");
    let output = run(scratch.path(), "base64", &["-d"], &input);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"foo".repeat(4097));
    assert!(
        output.stderr.ends_with(b"invalid input\n"),
        "{:?}",
        output.stderr
    );
}

#[test]
fn tsort_retains_its_byte_safe_nul_token_extension() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "tsort", &[], b"a\0x b\nb c\n"),
        b"a\0x\nb\nc\n",
    );
}
