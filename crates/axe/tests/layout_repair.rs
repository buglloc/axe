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
                "axe-layout-repair-{}-{}",
                std::process::id(),
                NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create layout scratch directory: {error}"),
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
        .expect("spawn layout applet");
    let mut stdin = child.stdin.take().expect("piped stdin");
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = child.wait_with_output().expect("wait for layout applet");
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
fn sort_general_numeric_orders_signed_infinities() {
    let scratch = Scratch::new();
    assert_success(
        run(
            scratch.path(),
            "sort",
            &["-g"],
            b"NaN\n-nan\n-inf\n+inf\n1e2\n10\n-0\n0\n1.01\n",
        ),
        b"-nan\nNaN\n-inf\n-0\n0\n1.01\n10\n1e2\n+inf\n",
    );
}

#[test]
fn fmt_preserves_carriage_returns_in_formatted_words() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "fmt", &[], b"a\r\nb\r\nlast\r"),
        b"a\r b\r last\r\n",
    );
}

#[test]
fn fmt_nonmatching_prefix_records_keep_their_terminators() {
    let scratch = Scratch::new();
    for input in [&b"a"[..], &b"a\r\nb\xff\r"[..], &b"a\n"[..]] {
        assert_success(
            run(scratch.path(), "fmt", &["-p", "> ", "-w12"], input),
            input,
        );
    }
}

#[test]
fn fmt_missing_operand_does_not_suppress_later_files() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("a"), b"abc\tdef").unwrap();
    let output = run(scratch.path(), "fmt", &["missing", "a"], b"");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"abc\tdef\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing"));
}

#[test]
fn csplit_suppressed_numeric_boundary_may_follow_the_final_record() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "csplit", &["--suppress-matched", "-", "2"], b"a"),
        b"1\n0\n",
    );
    assert_eq!(fs::read(scratch.path().join("xx00")).unwrap(), b"a");
    assert_eq!(fs::read(scratch.path().join("xx01")).unwrap(), b"");
    let output = run(
        scratch.path(),
        "csplit",
        &["--suppress-matched", "-f", "bad", "-", "3"],
        b"a",
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(!scratch.path().join("bad00").exists());
}

#[test]
fn fold_control_bytes_advance_columns_except_nul() {
    let scratch = Scratch::new();
    for (args, expected) in [
        (&["-w1"][..], &b"a\x08b\rc\n\x0b\n\x0c\nd\n\0e\n"[..]),
        (&["-w2"][..], &b"a\x08b\rc\x0b\n\x0cd\n\0e\n"[..]),
        (&["-s", "-w3"][..], &b"a\x08b\rc\x0b\x0c\nd\n\0e\n"[..]),
    ] {
        assert_success(
            run(scratch.path(), "fold", args, b"a\x08b\rc\x0b\x0cd\n\0e\n"),
            expected,
        );
    }
}

#[test]
fn fold_respects_locale_and_keeps_multibyte_units_in_byte_mode() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "fold", &["-w2"], "界é\n".as_bytes()),
        b"\xe7\x95\n\x8c\xc3\n\xa9\n",
    );
    assert_success(
        run_with_locale(
            scratch.path(),
            "fold",
            &["-b", "-w2"],
            "界é\tA\n".as_bytes(),
            "C.UTF-8",
        ),
        "界\né\n\tA\n".as_bytes(),
    );
}

#[test]
fn csplit_suppresses_records_at_repeated_numeric_boundaries() {
    let scratch = Scratch::new();
    assert_success(
        run(
            scratch.path(),
            "csplit",
            &["--suppress-matched", "-", "2", "2"],
            b"a\nb\nc",
        ),
        b"2\n0\n0\n",
    );
    assert_eq!(fs::read(scratch.path().join("xx00")).unwrap(), b"a\n");
    assert_eq!(fs::read(scratch.path().join("xx01")).unwrap(), b"");
    assert_eq!(fs::read(scratch.path().join("xx02")).unwrap(), b"");
}

#[test]
fn pr_preserves_input_page_breaks_and_continuous_record_numbers() {
    let scratch = Scratch::new();
    for (args, expected) in [
        (&["-t"][..], &b"a\n\x0cb\nc\n"[..]),
        (&["-t", "-n:3"][..], &b"  1:a\n\x0c  2:b\n  3:c\n"[..]),
        (&["-t", "-d"][..], &b"a\n\x0cb\n\nc\n\n"[..]),
    ] {
        assert_success(run(scratch.path(), "pr", args, b"a\x0cb\nc\n"), expected);
    }
    assert_success(run(scratch.path(), "pr", &["-t"], b"\x0c\x0ca"), b"\x0c\x0ca\n");
}

#[test]
fn pr_does_not_double_space_after_an_unterminated_record() {
    let scratch = Scratch::new();
    assert_success(run(scratch.path(), "pr", &["-t", "-d"], b"a\nb"), b"a\n\nb\n");
}

#[test]
fn pr_empty_truncated_columns_do_not_shift_following_content() {
    let scratch = Scratch::new();
    assert_success(
        run(
            scratch.path(),
            "pr",
            &["-t", "-a", "-3", "-w20"],
            b"a\tb\tc\n\tX\nlast\t",
        ),
        b"a\t      last\n",
    );
    assert_success(
        run(
            scratch.path(),
            "pr",
            &["-t", "-a", "-3", "-w20"],
            b"  alpha  beta\t gamma\n\t delta  epsilon\n",
        ),
        b"  alph\n",
    );
}

#[test]
fn ptx_leading_context_whitespace_does_not_drop_a_fitting_tail() {
    let scratch = Scratch::new();
    assert_success(
        run(
            scratch.path(),
            "ptx",
            &["-w30", "-g1"],
            b"  alpha\tbeta\n\nGamma delta.",
        ),
        b"     alpha beta Gamma delta.\n Gamma delta.     alpha beta\n delta/   alpha beta  Gamma\n    beta  Gamma delta.   /alpha\n",
    );
}

#[test]
fn fmt_tagged_paragraphs_use_the_secondary_indent() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "fmt", &["-t", "-w15"], b"1234567\x08\tX\n"),
        b"1234567\x08\n   X\n",
    );
}

#[test]
fn fmt_uniform_layout_uses_paragraph_suffix_costs() {
    let scratch = Scratch::new();
    assert_success(
        run(
            scratch.path(),
            "fmt",
            &["-u", "-w15"],
            b" one two three four five\nsix seven eight nine\n",
        ),
        b" one two three\n four five\nsix seven\neight nine\n",
    );
}

#[test]
fn fmt_uniform_spacing_retains_control_word_boundaries() {
    let scratch = Scratch::new();
    assert_success(
        run(scratch.path(), "fmt", &["-u", "-w15"], b"a\r\nb\r\nlast\r"),
        b"a \r b \r last \r\n",
    );
    assert_success(
        run(
            scratch.path(),
            "fmt",
            &["-u", "-w15"],
            b"a\x08b\rc\x0b\x0cd\n\0e\n",
        ),
        b"a\x08b \rc \x0b \x0cd \0e\n",
    );
}
