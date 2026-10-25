use std::fs::{self, File, FileTimes};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        Self::new_under(label, &std::env::temp_dir())
    }

    /// Scratch directory whose whole canonical path other users can traverse,
    /// for tests that drop privileges: temp_dir, or /dev/shm when temp_dir
    /// resolves under a private parent.
    fn new_public(label: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let candidate = std::env::temp_dir();
        let candidate = fs::canonicalize(&candidate).unwrap_or(candidate);
        let traversable = candidate.ancestors().all(|directory| {
            fs::metadata(directory).is_ok_and(|metadata| metadata.permissions().mode() & 0o001 != 0)
        });
        let parent = if traversable {
            candidate
        } else {
            PathBuf::from("/dev/shm")
        };
        Self::new_under(label, &parent)
    }

    fn new_under(label: &str, parent: &Path) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = parent.join(format!("axe-diff-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
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
        .arg("--")
        .args(args)
        .current_dir(directory)
        .env("LC_ALL", "C")
        .env("TZ", "UTC0")
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn assert_output(output: Output, status: i32, expected: &[u8]) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_eq!(output.stdout, expected, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn informational_options_do_not_override_delimited_file_operands() {
    let scratch = Scratch::new("option-operands");
    scratch.write("--help", b"one\n");
    scratch.write("--version", b"one\n");
    scratch.write("b", b"two\n");
    scratch.write("base", b"base\n");
    for name in ["--help", "--version"] {
        assert_output(
            run(&scratch.0, "cmp", &["--", name, "b"], b""),
            1,
            format!("{name} b differ: char 1, line 1\n").as_bytes(),
        );
        assert_output(
            run(&scratch.0, "diff", &["--", name, "b"], b""),
            1,
            b"1c1\n< one\n---\n> two\n",
        );
        assert_output(
            run(&scratch.0, "diff3", &["--", name, "base", "b"], b""),
            0,
            b"====\n1:1c\n  one\n2:1c\n  base\n3:1c\n  two\n",
        );
    }
}

#[test]
fn labels_are_option_values_even_when_they_look_informational() {
    let scratch = Scratch::new("option-values");
    scratch.write("a", b"old\n");
    scratch.write("b", b"new\n");
    assert_output(
        run(
            &scratch.0,
            "diff",
            &["-u", "--label", "--help", "--label=--version", "a", "b"],
            b"",
        ),
        1,
        b"--- --help\n+++ --version\n@@ -1 +1 @@\n-old\n+new\n",
    );
    scratch.write("base", b"base\n");
    assert_output(
        run(
            &scratch.0,
            "diff3",
            &["-m", "-L", "--help", "a", "base", "b"],
            b"",
        ),
        1,
        b"<<<<<<< --help\nold\n||||||| base\nbase\n=======\nnew\n>>>>>>> b\n",
    );
}

#[test]
fn cmp_accepts_gnu_byte_count_and_skip_suffixes() {
    let scratch = Scratch::new("cmp-suffixes");
    scratch.write("a", &[vec![b'x'; 1024], b"left".to_vec()].concat());
    scratch.write("b", &[vec![b'x'; 1024], b"right".to_vec()].concat());
    for flags in [&["-n", "1K"][..], &["--bytes=1KiB"][..], &["-n1024"][..]] {
        let mut args = flags.to_vec();
        args.extend(["a", "b"]);
        assert_output(run(&scratch.0, "cmp", &args, b""), 0, b"");
    }
    scratch.write("a", &[vec![0; 1024], b"equal".to_vec()].concat());
    scratch.write("b", &[vec![1; 1000], b"equal".to_vec()].concat());
    assert_output(
        run(&scratch.0, "cmp", &["-i", "1K:1kB", "a", "b"], b""),
        0,
        b"",
    );
    assert_output(
        run(&scratch.0, "cmp", &["a", "b", "1K", "1kB"], b""),
        0,
        b"",
    );
    let output = run(
        &scratch.0,
        "cmp",
        &["-n", "18446744073709551616", "a", "b"],
        b"",
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn diff_core_ignore_flags_preserve_actual_changed_line_bytes_and_numbers() {
    let scratch = Scratch::new("ignore-options");
    for (flags, left, right) in [
        (&["-w"][..], b"a b\n".as_slice(), b"ab\n".as_slice()),
        (&["-b"][..], b"a b  \n".as_slice(), b"a\t b\n".as_slice()),
        (&["-i"][..], b"hello\n".as_slice(), b"HELLO\n".as_slice()),
        (&["-B"][..], b"a\n\nb\n".as_slice(), b"a\nb\n".as_slice()),
        (
            &["-I", "^generated"][..],
            b"generated one\nx\n".as_slice(),
            b"generated two\nx\n".as_slice(),
        ),
    ] {
        scratch.write("a", left);
        scratch.write("b", right);
        let mut args = flags.to_vec();
        args.extend(["a", "b"]);
        assert_output(run(&scratch.0, "diff", &args, b""), 0, b"");
    }
    scratch.write("a", b"Title\nleft\n");
    scratch.write("b", b"TITLE\nright\n");
    assert_output(
        run(&scratch.0, "diff", &["-i", "a", "b"], b""),
        1,
        b"2c2\n< left\n---\n> right\n",
    );
}

#[test]
fn diff_binary_mode_and_forced_text_have_distinct_outputs() {
    let scratch = Scratch::new("binary");
    scratch.write("a", b"a\0b\n");
    scratch.write("b", b"a\0c\n");
    assert_output(
        run(&scratch.0, "diff", &["a", "b"], b""),
        1,
        b"Binary files a and b differ\n",
    );
    assert_output(
        run(&scratch.0, "diff", &["-q", "a", "b"], b""),
        1,
        b"Files a and b differ\n",
    );
    assert_output(
        run(&scratch.0, "diff", &["-a", "a", "b"], b""),
        1,
        b"1c1\n< a\0b\n---\n> a\0c\n",
    );
}

#[test]
fn side_by_side_pairs_replacements_and_leaves_real_deletions_unpaired() {
    let scratch = Scratch::new("columns");
    scratch.write("a", b"a\nb\ngone\n");
    scratch.write("b", b"a\nc\n");
    for width in [&["-W", "20"][..], &["--width=20"][..]] {
        let mut args = vec!["-y"];
        args.extend(width);
        args.extend(["a", "b"]);
        assert_output(
            run(&scratch.0, "diff", &args, b""),
            1,
            b"a\ta\nb     |\tc\ngone  <\n",
        );
    }
    assert_output(
        run(&scratch.0, "diff", &["-y", "-W20", "b", "b"], b""),
        0,
        b"a\ta\nc\tc\n",
    );
}

#[test]
fn context_timestamps_and_labelled_unified_headers_match_the_text_contract() {
    let scratch = Scratch::new("headers");
    scratch.write("a", b"a\nb\n");
    scratch.write("b", b"a\nc\n");
    for name in ["a", "b"] {
        fs::File::options()
            .write(true)
            .open(scratch.0.join(name))
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            )
            .unwrap();
    }
    assert_output(run(&scratch.0, "diff", &["-c", "a", "b"], b""), 1,
        b"*** a\tTue Nov 14 22:13:20 2023\n--- b\tTue Nov 14 22:13:20 2023\n***************\n*** 1,2 ****\n  a\n! b\n--- 1,2 ----\n  a\n! c\n");
    assert_output(
        run(
            &scratch.0,
            "diff",
            &["-u", "--label=LEFT", "--label=RIGHT", "a", "b"],
            b"",
        ),
        1,
        b"--- LEFT\n+++ RIGHT\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n",
    );
}

#[test]
fn shallow_directories_and_recursive_new_files_report_the_right_entries() {
    let scratch = Scratch::new("directories");
    scratch.write("left/a", b"x\n");
    scratch.write("right/a", b"y\n");
    scratch.write("left/equal", b"same\n");
    scratch.write("right/equal", b"same\n");
    scratch.write("left/sub/x", b"left\n");
    scratch.write("right/sub/x", b"right\n");
    assert_output(
        run(&scratch.0, "diff", &["left", "right"], b""),
        1,
        b"diff left/a right/a\n1c1\n< x\n---\n> y\nCommon subdirectories: left/sub and right/sub\n",
    );
    let other = Scratch::new("new-files");
    other.write("left/a", b"x\n");
    other.write("right/b", b"y\n");
    assert_output(
        run(&other.0, "diff", &["-rN", "left", "right"], b""),
        1,
        b"diff -rN left/a right/a\n1d0\n< x\ndiff -rN left/b right/b\n0a1\n> y\n",
    );
}

#[test]
fn diff3_merge_groups_same_changes_and_adjacent_edits_as_gnu_conflicts() {
    let scratch = Scratch::new("three-way-grouping");
    scratch.write("mine", b"A\nb\n");
    scratch.write("base", b"a\nb\n");
    scratch.write("yours", b"A\nb\n");
    assert_output(
        run(&scratch.0, "diff3", &["-m", "mine", "base", "yours"], b""),
        1,
        b"<<<<<<< base\na\n=======\nA\n>>>>>>> yours\nb\n",
    );
    scratch.write("yours", b"a\nB\n");
    assert_output(
        run(
            &scratch.0,
            "diff3",
            &["-m", "-A", "mine", "base", "yours"],
            b"",
        ),
        1,
        b"<<<<<<< mine\nA\nb\n||||||| base\na\nb\n=======\na\nB\n>>>>>>> yours\n",
    );
    scratch.write("mine", b"A\nb\nc\n");
    scratch.write("base", b"a\nb\nc\n");
    scratch.write("yours", b"a\nb\nC\n");
    assert_output(
        run(&scratch.0, "diff3", &["-m", "mine", "base", "yours"], b""),
        0,
        b"A\nb\nC\n",
    );
}

#[test]
fn diff3_reports_equal_sides_once_in_the_gnu_section_order() {
    let scratch = Scratch::new("three-way-report");
    for (mine, base, yours, expected) in [
        (
            b"A\nb\n".as_slice(),
            b"a\nb\n".as_slice(),
            b"a\nb\n".as_slice(),
            b"====1\n1:1c\n  A\n2:1c\n3:1c\n  a\n".as_slice(),
        ),
        (
            b"a\nb\n".as_slice(),
            b"a\nb\n".as_slice(),
            b"a\nB\n".as_slice(),
            b"====3\n1:2c\n2:2c\n  b\n3:2c\n  B\n".as_slice(),
        ),
        (
            b"A\nb\n".as_slice(),
            b"a\nb\n".as_slice(),
            b"A\nb\n".as_slice(),
            b"====2\n1:1c\n3:1c\n  A\n2:1c\n  a\n".as_slice(),
        ),
    ] {
        scratch.write("mine", mine);
        scratch.write("base", base);
        scratch.write("yours", yours);
        assert_output(
            run(&scratch.0, "diff3", &["mine", "base", "yours"], b""),
            0,
            expected,
        );
    }
}

#[test]
fn diff3_incomplete_lines_are_annotated_in_reports_but_not_changed_in_merges() {
    let scratch = Scratch::new("three-way-newline");
    scratch.write("mine", b"ours");
    scratch.write("base", b"base");
    scratch.write("yours", b"theirs");
    assert_output(run(&scratch.0, "diff3", &["mine", "base", "yours"], b""), 0,
        b"====\n1:1c\n  ours\n\\ No newline at end of file\n2:1c\n  base\n\\ No newline at end of file\n3:1c\n  theirs\n\\ No newline at end of file\n");
    assert_output(
        run(&scratch.0, "diff3", &["-m", "mine", "base", "yours"], b""),
        1,
        b"<<<<<<< mine\nours||||||| base\nbase=======\ntheirs>>>>>>> yours\n",
    );
}

#[test]
fn diff3_binary_rejection_happens_before_any_report_or_merge_output() {
    let scratch = Scratch::new("three-way-binary");
    scratch.write("mine", b"a\0x\n");
    scratch.write("base", b"a\0b\n");
    scratch.write("yours", b"a\0y\n");
    for flags in [&[][..], &["-m"][..]] {
        let mut args = flags.to_vec();
        args.extend(["mine", "base", "yours"]);
        let output = run(&scratch.0, "diff3", &args, b"");
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("diff failed: Binary files"),
            "{output:?}"
        );
    }
}

#[test]
fn diff3_accepts_partial_labels_stdin_and_ed_output() {
    let scratch = Scratch::new("three-way-input");
    scratch.write("mine", b"ours\n");
    scratch.write("base", b"base\n");
    scratch.write("yours", b"theirs\n");
    assert_output(
        run(
            &scratch.0,
            "diff3",
            &["-m", "-L", "OURS", "-", "base", "yours"],
            b"ours\n",
        ),
        1,
        b"<<<<<<< OURS\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> yours\n",
    );
    assert_output(
        run(&scratch.0, "diff3", &["-e", "mine", "base", "yours"], b""),
        0,
        b"1c\ntheirs\n.\n",
    );
}

#[test]
fn recursive_new_file_comparison_treats_missing_directory_roots_as_empty_trees() {
    let scratch = Scratch::new("missing-directory-root");
    scratch.write("left/a", b"x\n");
    scratch.write("left/sub/x", b"nested\n");
    assert_output(
        run(&scratch.0, "diff", &["-rN", "left", "right"], b""),
        1,
        b"diff -rN left/a right/a\n1d0\n< x\ndiff -rN left/sub/x right/sub/x\n1d0\n< nested\n",
    );
    assert_output(
        run(&scratch.0, "diff", &["-rN", "right", "left"], b""),
        1,
        b"diff -rN right/a left/a\n0a1\n> x\ndiff -rN right/sub/x left/sub/x\n0a1\n> nested\n",
    );
}

#[test]
fn diff3_ed_escapes_literal_terminators_after_completing_incomplete_lines() {
    let scratch = Scratch::new("ed-dot-terminator");
    scratch.write("mine", b"keep\nold\n");
    scratch.write("base", b"keep\nold\n");
    for yours in [b"keep\n.".as_slice(), b"keep\n.\n".as_slice()] {
        scratch.write("yours", yours);
        assert_output(
            run(&scratch.0, "diff3", &["-e", "mine", "base", "yours"], b""),
            0,
            b"2c\n..\n.\ns/.//\na\n.\n",
        );
    }
}

#[test]
fn ignore_matching_lines_preserves_basic_bracket_backslashes_and_named_classes() {
    let scratch = Scratch::new("bre-brackets");
    scratch.write("a", b"1\n");
    scratch.write("b", b"2\n");
    assert_output(
        run(&scratch.0, "diff", &["-I", r"[\d]", "a", "b"], b""),
        1,
        b"1c1\n< 1\n---\n> 2\n",
    );
    assert_output(
        run(&scratch.0, "diff", &["-I", "[[:digit:]]", "a", "b"], b""),
        0,
        b"",
    );
    scratch.write("a", b"d\n");
    scratch.write("b", b"\\\n");
    assert_output(
        run(&scratch.0, "diff", &["-I", r"[\d]", "a", "b"], b""),
        0,
        b"",
    );
}

#[test]
fn whitespace_modes_ignore_final_newline_presence_without_changing_default_comparison() {
    let scratch = Scratch::new("whitespace-final-newline");
    scratch.write("a", b"x\n");
    scratch.write("b", b"x");
    for flag in ["-b", "-w"] {
        assert_output(run(&scratch.0, "diff", &[flag, "a", "b"], b""), 0, b"");
        assert_output(run(&scratch.0, "diff", &[flag, "b", "a"], b""), 0, b"");
    }
    assert_output(
        run(&scratch.0, "diff", &["a", "b"], b""),
        1,
        b"1c1\n< x\n---\n> x\n\\ No newline at end of file\n",
    );
}

#[test]
fn brief_mode_suppresses_side_by_side_details_for_both_equal_and_different_files() {
    let scratch = Scratch::new("brief-columns");
    scratch.write("a", b"x\n");
    scratch.write("b", b"x\n");
    assert_output(
        run(&scratch.0, "diff", &["-q", "-y", "a", "b"], b""),
        0,
        b"",
    );
    assert_output(
        run(&scratch.0, "diff", &["-q", "-y", "-s", "a", "b"], b""),
        0,
        b"Files a and b are identical\n",
    );
    scratch.write("b", b"y\n");
    assert_output(
        run(&scratch.0, "diff", &["-q", "-y", "a", "b"], b""),
        1,
        b"Files a and b differ\n",
    );
}

#[test]
fn side_by_side_ignored_replacements_use_common_row_markers() {
    let scratch = Scratch::new("ignored-columns");
    scratch.write("a", b"#a\n");
    scratch.write("b", b"#b\n");
    assert_output(
        run(
            &scratch.0,
            "diff",
            &["-y", "-W20", "-I", "^#", "a", "b"],
            b"",
        ),
        0,
        b"#a\t#b\n",
    );
    assert_output(
        run(&scratch.0, "diff", &["-y", "-W20", "a", "b"], b""),
        1,
        b"#a    |\t#b\n",
    );
}

#[test]
fn diff3_ed_modes_emit_no_script_for_identical_incomplete_inputs() {
    let scratch = Scratch::new("ed-incomplete-noop");
    for name in ["mine", "base", "yours"] {
        scratch.write(name, b"x");
    }
    for flag in ["-A", "-e"] {
        assert_output(
            run(&scratch.0, "diff3", &[flag, "mine", "base", "yours"], b""),
            0,
            b"",
        );
    }
}

#[test]
fn cmp_byte_limits_stop_before_eof_or_differences_outside_the_selected_prefix() {
    let scratch = Scratch::new("cmp-bounded-prefix");
    for (left, right, limit) in [
        (b"abc-left".as_slice(), b"abc-right".as_slice(), "3"),
        (b"left".as_slice(), b"right".as_slice(), "0"),
        (b"".as_slice(), b"not empty".as_slice(), "0"),
    ] {
        scratch.write("a", left);
        scratch.write("b", right);
        for quiet in [false, true] {
            let mut args = vec!["-n", limit, "a", "b"];
            if quiet {
                args.insert(0, "-s");
            }
            assert_output(run(&scratch.0, "cmp", &args, b""), 0, b"");
        }
    }
    scratch.write("a", b"abc");
    scratch.write("b", b"abcd");
    assert_output(run(&scratch.0, "cmp", &["-n3", "a", "b"], b""), 0, b"");
    assert_output(run(&scratch.0, "cmp", &["-sn3", "a", "b"], b""), 0, b"");
    let output = run(&scratch.0, "cmp", &["-sn4", "a", "b"], b"");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn cmp_quiet_size_shortcut_uses_lengths_after_skips_and_limits() {
    let scratch = Scratch::new("cmp-quiet-ranges");
    scratch.write("a", b"paddingCOMMON");
    scratch.write("b", b"xxCOMMON");
    assert_output(
        run(&scratch.0, "cmp", &["-s", "-i7:2", "a", "b"], b""),
        0,
        b"",
    );
    assert_output(
        run(&scratch.0, "cmp", &["-s", "a", "b", "7", "2"], b""),
        0,
        b"",
    );
    scratch.write("a", b"paddingCOMMONleft");
    scratch.write("b", b"xxCOMMONright");
    assert_output(
        run(&scratch.0, "cmp", &["-s", "-i7:2", "-n6", "a", "b"], b""),
        0,
        b"",
    );
    let output = run(&scratch.0, "cmp", &["-s", "-i7:2", "-n7", "a", "b"], b"");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn cmp_verbose_output_does_not_continue_into_later_buffers_past_the_limit() {
    let scratch = Scratch::new("cmp-verbose-limit");
    let mut left = vec![b'a'; 20_000];
    let mut right = left.clone();
    left[1] = b'b';
    right[1] = b'y';
    right[10_000] = b'z';
    scratch.write("a", &left);
    scratch.write("b", &right);
    assert_output(
        run(&scratch.0, "cmp", &["-l", "-n3", "a", "b"], b""),
        1,
        b"2 142 171\n",
    );
}

#[test]
fn cmp_zero_limit_still_reports_missing_input_as_trouble() {
    let scratch = Scratch::new("cmp-zero-missing");
    scratch.write("present", b"data");
    for (left, right) in [("missing", "present"), ("present", "missing")] {
        for flags in [&["-n0"][..], &["-s", "--bytes=0"][..]] {
            let mut args = flags.to_vec();
            args.extend([left, right]);
            let output = run(&scratch.0, "cmp", &args, b"");
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
        }
    }
}

#[cfg(unix)]
#[test]
fn cmp_zero_limit_does_not_hide_an_unreadable_input() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;

    // SAFETY: geteuid only reads the current identity.
    let drop_root = unsafe { libc::geteuid() } == 0;
    // As root the input is only unreadable to a dropped uid, which must be
    // able to traverse to the scratch tree and the binary copy.
    let scratch = if drop_root {
        Scratch::new_public("cmp-zero-unreadable")
    } else {
        Scratch::new("cmp-zero-unreadable")
    };
    scratch.write("readable", b"data");
    scratch.write("unreadable", b"private");
    fs::set_permissions(
        scratch.0.join("readable"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::set_permissions(
        scratch.0.join("unreadable"),
        fs::Permissions::from_mode(0o0),
    )
    .unwrap();
    let executable = if drop_root {
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o755)).unwrap();
        let binary = scratch.0.join("axe");
        fs::copy(env!("CARGO_BIN_EXE_axe"), &binary).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let probe = Command::new(&binary)
            .arg("--version")
            .current_dir("/")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .gid(65_534)
            .uid(65_534)
            .status();
        match probe {
            Ok(status) if status.success() => {}
            other => {
                eprintln!("SKIP: the dropped UID cannot execute this AXE build ({other:?})");
                return;
            }
        }
        binary
    } else {
        PathBuf::from(env!("CARGO_BIN_EXE_axe"))
    };
    for (left, right) in [("unreadable", "readable"), ("readable", "unreadable")] {
        for quiet in [false, true] {
            let mut command = Command::new(&executable);
            command.args(["cmp", "--", "-n0"]);
            if quiet {
                command.arg("-s");
            }
            command
                .args([left, right])
                .current_dir(&scratch.0)
                .env_clear()
                .env("HOME", &scratch.0)
                .env("LC_ALL", "C")
                .env("PATH", "/nonexistent")
                .stdin(Stdio::null());
            if drop_root {
                command.gid(65_534).uid(65_534);
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            if !quiet {
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("Permission denied"),
                    "{output:?}"
                );
            }
        }
    }
}
#[cfg(unix)]
#[test]
fn cmp_reports_stdout_write_failures() {
    let scratch = Scratch::new("cmp-dev-full");
    scratch.write("a", b"a");
    scratch.write("b", b"b");
    for args in [
        &["cmp", "--", "a", "b"][..],
        &["cmp", "--", "-l", "a", "b"][..],
    ] {
        let stdout = File::create("/dev/full").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(args)
            .current_dir(&scratch.0)
            .env_clear()
            .env("HOME", &scratch.0)
            .env("LC_ALL", "C")
            .env("PATH", "/nonexistent")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(!output.stderr.is_empty(), "{output:?}");
    }
}
