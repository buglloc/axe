#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "axe-sed-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create sed scratch directory: {error}"),
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

fn run<A: AsRef<OsStr>>(directory: &Path, args: &[A], input: &[u8]) -> Output {
    run_locale(directory, args, input, "C")
}

fn sed_command<A: AsRef<OsStr>>(directory: &Path, args: &[A], locale: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", "sed", "--"])
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("HOME", directory)
        .env("LANG", locale)
        .env("LC_ALL", locale)
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_locale<A: AsRef<OsStr>>(directory: &Path, args: &[A], input: &[u8], locale: &str) -> Output {
    let mut child = sed_command(directory, args, locale)
        .spawn()
        .expect("spawn sed");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .expect("write sed stdin");
    child.wait_with_output().expect("wait for sed")
}

fn expect_output(output: Output, code: i32, stdout: &[u8]) {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    assert_eq!(output.stdout, stdout, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn stream_and_in_place_preserve_non_text_bytes_and_record_endings() {
    let scratch = Scratch::new();
    let cases: &[(&[u8], &[u8], bool)] = &[
        (b"a\xffb\n", b"A\xffb\n", false),
        (b"a\nb\xff\n", b"A\nb\xff\n", false),
        (b"a\r\nb\r\n", b"A\r\nb\r\n", false),
        (b"abc", b"Abc", false),
        (b"a\0b\n", b"A\0b\n", false),
        (b"a\xffb\0", b"A\xffb\0", true),
        (b"a\0b\xff", b"A\0b\xff", true),
        (b"a\r\n\xff\0b", b"A\r\n\xff\0b", true),
    ];
    for &(input, expected, null_data) in cases {
        let mut args = Vec::new();
        if null_data {
            args.push("-z")
        }
        args.push("s/a/A/");
        expect_output(run(scratch.path(), &args, input), 0, expected);

        let target = scratch.path().join("target");
        fs::write(&target, input).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        args.insert(0, "-i");
        args.push("target");
        expect_output(run(scratch.path(), &args, b""), 0, b"");
        assert_eq!(fs::read(&target).unwrap(), expected, "input={input:?}");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

#[test]
fn empty_scripts_and_explicit_stdin_are_identity_filters() {
    let scratch = Scratch::new();
    for args in [&[""][..], &["-e", ""][..], &["", "-"][..]] {
        expect_output(
            run(scratch.path(), args, b"a\xff\r\n\0b"),
            0,
            b"a\xff\r\n\0b",
        );
    }
    fs::write(scratch.path().join("-input"), b"from file\n").unwrap();
    expect_output(
        run(scratch.path(), &["-e", "", "--", "-input"], b""),
        0,
        b"from file\n",
    );
    expect_output(run(scratch.path(), &["s/a/A/", "-"], b"abc\n"), 0, b"Abc\n");
}

#[test]
fn files_and_stdin_share_line_numbers_hold_space_and_ranges_by_default() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("a"), b"a\n").unwrap();
    fs::write(scratch.path().join("b"), b"a\na\n").unwrap();
    expect_output(run(scratch.path(), &["-n", "$=", "a", "b"], b""), 0, b"3\n");
    expect_output(
        run(scratch.path(), &["1s/a/A/", "a", "b"], b""),
        0,
        b"A\na\na\n",
    );
    expect_output(
        run(scratch.path(), &["-n", "2,3p", "a", "-", "b"], b"middle\n"),
        0,
        b"middle\na\n",
    );
    expect_output(
        run(scratch.path(), &["-n", "1h;$g;$p", "a", "b"], b""),
        0,
        b"a\n",
    );
    expect_output(
        run(scratch.path(), &["-s", "-n", "$=", "a", "b"], b""),
        0,
        b"1\n2\n",
    );
    fs::write(scratch.path().join("a"), b"start\na\n").unwrap();
    fs::write(scratch.path().join("b"), b"a\nend\na\n").unwrap();
    expect_output(
        run(scratch.path(), &["/start/,/end/s/a/A/g", "a", "b"], b""),
        0,
        b"stArt\nA\nA\nend\na\n",
    );
    fs::write(scratch.path().join("a"), b"one\n").unwrap();
    fs::write(scratch.path().join("b"), b"two").unwrap();
    expect_output(
        run(scratch.path(), &["-n", "N;p", "a", "b"], b""),
        0,
        b"one\ntwo",
    );
}

#[test]
fn missing_inputs_report_each_error_but_do_not_skip_independent_files() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("a"), b"a\n").unwrap();
    fs::write(scratch.path().join("b"), b"a\n").unwrap();
    let output = run(
        scratch.path(),
        &["s/a/A/", "missing-one", "a", "missing-two", "b"],
        b"",
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(output.stdout, b"A\nA\n");
    assert!(!output.stderr.is_empty(), "{output:?}");
    let output = run(
        scratch.path(),
        &["-i.bak", "s/a/A/", "missing-one", "a", "missing-two", "b"],
        b"",
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty(), "{output:?}");
    for name in ["a", "b"] {
        assert_eq!(fs::read(scratch.path().join(name)).unwrap(), b"A\n");
        assert_eq!(
            fs::read(scratch.path().join(format!("{name}.bak"))).unwrap(),
            b"a\n"
        );
    }
    assert!(!scratch.path().join("missing-one").exists());
    assert!(!scratch.path().join("missing-two").exists());
}

#[test]
fn in_place_replaces_the_link_unless_following_is_explicit() {
    for follow in [false, true] {
        let scratch = Scratch::new();
        let target = scratch.path().join("target");
        let link = scratch.path().join("link");
        fs::write(&target, b"old\nold\n").unwrap();
        symlink("target", &link).unwrap();
        let mut args = vec!["-i"];
        if follow {
            args.push("--follow-symlinks")
        }
        args.extend(["s/old/new/", "link"]);
        expect_output(run(scratch.path(), &args, b""), 0, b"");
        assert_eq!(fs::read(&link).unwrap(), b"new\nnew\n");
        assert_eq!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            follow
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            if follow { b"new\nnew\n" } else { b"old\nold\n" }
        );
    }
}

#[test]
fn backup_expansion_and_non_utf8_paths_preserve_exact_names_and_contents() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("target"), b"old\n").unwrap();
    expect_output(
        run(scratch.path(), &["-i*.bak", "s/old/new/", "target"], b""),
        0,
        b"",
    );
    assert_eq!(fs::read(scratch.path().join("target")).unwrap(), b"new\n");
    assert_eq!(
        fs::read(scratch.path().join("target.bak")).unwrap(),
        b"old\n"
    );
    assert!(!scratch.path().join("target*.bak").exists());

    let name = OsString::from_vec(b"ta\xferget".to_vec());
    let backup = OsString::from_vec(b"ta\xferget\xff.bak".to_vec());
    fs::write(scratch.path().join(&name), b"a\xff\r\n").unwrap();
    let args = [
        OsString::from_vec(b"-i*\xff.bak".to_vec()),
        OsString::from("s/a/A/"),
        name.clone(),
    ];
    expect_output(run(scratch.path(), &args, b""), 0, b"");
    assert_eq!(fs::read(scratch.path().join(&name)).unwrap(), b"A\xff\r\n");
    assert_eq!(
        fs::read(scratch.path().join(&backup)).unwrap(),
        b"a\xff\r\n"
    );
    expect_output(
        run(
            scratch.path(),
            &[OsString::from("-n"), OsString::from("F"), name],
            b"",
        ),
        0,
        b"ta\xferget\n",
    );
}

#[test]
fn longest_matching_keeps_the_leftmost_start_and_exact_capture_bytes() {
    let scratch = Scratch::new();
    for (script, input, expected) in [
        ("s/(a|aa)/X/", b"aa\n".as_slice(), b"X\n".as_slice()),
        ("s/a|bbbb/X/", b"abbbb\n", b"Xbbbb\n"),
        (r"s/(a|aa)/[\1]/", b"aa\n", b"[aa]\n"),
        (r"s/(..)/[\1]/", b"\xffa\n", b"[\xffa]\n"),
        (r"s/\x00/X/g", b"a\0b\0\xff", b"aXbX\xff"),
        ("s/a*/X/g", b"aba\n", b"XbX\n"),
    ] {
        expect_output(run(scratch.path(), &["-E", script], input), 0, expected);
    }
}

#[test]
fn zero_regex_ranges_and_all_gnu_case_replacement_operations_work() {
    let scratch = Scratch::new();
    for (script, input, expected) in [
        ("0,/x/s/x/y/", b"x\nx\n".as_slice(), b"y\nx\n".as_slice()),
        ("0,/x/s/./Y/", b"a\nx\nx\n", b"Y\nY\nx\n"),
        (r"s/AB/\L&/g", b"AB CD\n", b"ab CD\n"),
        (r"s/AB/\L\u&/g", b"AB AB\n", b"Ab Ab\n"),
        (r"s/ab/\U\l&\E/g", b"ab ab\n", b"aB aB\n"),
        (r"s/\(a\?\)b/\u\1x/g", b"b ab\n", b"X Ax\n"),
        (r"s/\(AB\)\(cd\)/\L\1\E-\U\2\E/", b"ABcd\n", b"ab-CD\n"),
        (r"s/ab/\\L&/", b"ab\n", b"\\Lab\n"),
    ] {
        expect_output(run(scratch.path(), &[script], input), 0, expected);
    }
}

#[test]
fn quit_statuses_are_returned_only_after_output_and_in_place_publication() {
    let scratch = Scratch::new();
    for (args, expected) in [
        (&["q42"][..], b"first\n".as_slice()),
        (&["Q42"][..], b"".as_slice()),
        (&["p;Q42"][..], b"first\n".as_slice()),
        (&["-n", "q42"][..], b"".as_slice()),
    ] {
        expect_output(run(scratch.path(), args, b"first\nsecond\n"), 42, expected);
    }
    fs::write(scratch.path().join("a"), b"first\nsecond\n").unwrap();
    fs::write(scratch.path().join("b"), b"unchanged\n").unwrap();
    expect_output(
        run(scratch.path(), &["-i.bak", "q42", "a", "b"], b""),
        42,
        b"",
    );
    assert_eq!(fs::read(scratch.path().join("a")).unwrap(), b"first\n");
    assert_eq!(
        fs::read(scratch.path().join("a.bak")).unwrap(),
        b"first\nsecond\n"
    );
    assert_eq!(fs::read(scratch.path().join("b")).unwrap(), b"unchanged\n");
    assert!(!scratch.path().join("b.bak").exists());
}

struct FailingRead(io::Cursor<Vec<u8>>);

impl Read for FailingRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.0.position() == self.0.get_ref().len() as u64 {
            Err(io::Error::other("injected input read failure"))
        } else {
            self.0.read(bytes)
        }
    }
}

impl BufRead for FailingRead {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.0.position() == self.0.get_ref().len() as u64 {
            Err(io::Error::other("injected input read failure"))
        } else {
            self.0.fill_buf()
        }
    }
    fn consume(&mut self, amount: usize) {
        self.0.consume(amount)
    }
}

#[test]
fn initial_and_late_read_errors_are_never_successful_eof_or_lost_output() {
    for (prefix, null_data, expected) in [
        (b"".as_slice(), false, b"".as_slice()),
        (b"a\n".as_slice(), false, b"A\n".as_slice()),
        (b"a\0".as_slice(), true, b"A\0".as_slice()),
    ] {
        for script in ["s/a/A/", "s/a/A/;n"] {
            let options = sed_rs::Options {
                null_data,
                ..sed_rs::Options::default()
            };
            let engine =
                sed_rs::engine::Engine::new(sed_rs::command::parse(script).unwrap(), &options)
                    .unwrap();
            let mut output = Vec::new();
            let result =
                engine.process_stream(FailingRead(io::Cursor::new(prefix.to_vec())), &mut output);
            assert!(
                result.is_err(),
                "read failure was accepted as EOF: {result:?}"
            );
            assert_eq!(output, expected);
        }
    }
}

#[test]
fn quit_does_not_speculatively_read_more_input() {
    let engine = sed_rs::engine::Engine::new(
        sed_rs::command::parse("s/a/A/;q42").unwrap(),
        &sed_rs::Options::default(),
    )
    .unwrap();
    let mut output = Vec::new();
    let status = engine
        .process_stream(FailingRead(io::Cursor::new(b"a\n".to_vec())), &mut output)
        .unwrap();
    assert_eq!(status, 42);
    assert_eq!(output, b"A\n");
}

#[cfg(target_os = "linux")]
#[test]
fn in_place_input_read_errors_do_not_replace_links_or_create_backups() {
    let scratch = Scratch::new();
    let path = scratch.path().join("read-error");
    symlink("/proc/self/mem", &path).unwrap();
    let output = run(scratch.path(), &["-i.bak", "s/a/A/", "read-error"], b"");
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    assert_eq!(fs::read_link(&path).unwrap(), Path::new("/proc/self/mem"));
    assert!(!scratch.path().join("read-error.bak").exists());
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 1);
}

#[test]
fn failed_backup_creation_keeps_the_original_file_unchanged() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("target"), b"a\xff\r\n").unwrap();
    let output = run(scratch.path(), &["-imissing/*", "s/a/A/", "target"], b"");
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        fs::read(scratch.path().join("target")).unwrap(),
        b"a\xff\r\n"
    );
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 1);
}

#[test]
fn utf8_locale_preserves_character_matching_without_recoding_binary_input() {
    let scratch = Scratch::new();
    expect_output(
        run_locale(scratch.path(), &["s/./X/"], "é!\n".as_bytes(), "C.UTF-8"),
        0,
        b"X!\n",
    );
    expect_output(
        run(scratch.path(), &["s/./X/"], "é!\n".as_bytes()),
        0,
        b"X\xa9!\n",
    );
    expect_output(
        run_locale(scratch.path(), &[r"s/x*/-/g"], "é\n".as_bytes(), "C.UTF-8"),
        0,
        "-é-\n".as_bytes(),
    );
    expect_output(
        run_locale(
            scratch.path(),
            &[r"s/.*/\L&/"],
            "ÉΩ\n".as_bytes(),
            "C.UTF-8",
        ),
        0,
        "éω\n".as_bytes(),
    );
    expect_output(
        run_locale(scratch.path(), &["s/a/A/"], b"a\xff\r\n", "C.UTF-8"),
        0,
        b"A\xff\r\n",
    );
}

#[test]
fn a_default_symlink_backup_preserves_the_link_not_a_copy_of_its_target() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("target"), b"old\n").unwrap();
    symlink("target", scratch.path().join("link")).unwrap();
    expect_output(
        run(scratch.path(), &["-i*.bak", "s/old/new/", "link"], b""),
        0,
        b"",
    );
    assert_eq!(fs::read(scratch.path().join("link")).unwrap(), b"new\n");
    assert_eq!(
        fs::read_link(scratch.path().join("link.bak")).unwrap(),
        Path::new("target")
    );
    assert_eq!(fs::read(scratch.path().join("target")).unwrap(), b"old\n");
}

#[test]
fn utf8_matching_keeps_invalid_bytes_outside_character_matches() {
    let scratch = Scratch::new();
    let mixed = b"\xc3\xa9\xff\xce\xa9\n";
    for (script, input, expected) in [
        ("s/a/A/", b"a\n".as_slice(), b"A\n".as_slice()),
        ("/./d", b"a\n\xff\n", b"\xff\n"),
        ("/./d", b"\xe2\x82\n", b"\xe2\x82\n"),
        ("s/./X/g", mixed.as_slice(), b"X\xffX\n"),
        ("s/.*//", mixed.as_slice(), b"\xff\xce\xa9\n"),
        ("s/.*//g", mixed.as_slice(), b"\xff\n"),
        ("s/^a/A/", b"\xffa\n", b"\xffa\n"),
        ("s/a$/A/", b"a\xff\n", b"a\xff\n"),
        ("s/a$/A/", b"\xffa\n", b"\xffA\n"),
        ("s/^a$/A/", b"\xffa\xff\n", b"\xffa\xff\n"),
        ("s/a.*b/X/", b"a\xffb\n", b"a\xffb\n"),
    ] {
        expect_output(
            run_locale(scratch.path(), &[script], input, "C.UTF-8"),
            0,
            expected,
        );
    }
    let target = scratch.path().join("target");
    let original = b"a\n\xff\n\xc3\xa9\xff\xce\xa9\n\xe2\x82\n";
    fs::write(&target, original).unwrap();
    expect_output(
        run_locale(
            scratch.path(),
            &["-i.bak", "/./d", "target"],
            b"",
            "C.UTF-8",
        ),
        0,
        b"",
    );
    assert_eq!(fs::read(&target).unwrap(), b"\xff\n\xe2\x82\n");
    assert_eq!(
        fs::read(scratch.path().join("target.bak")).unwrap(),
        original
    );
    fs::write(&target, mixed).unwrap();
    expect_output(
        run_locale(scratch.path(), &["-i", "s/./X/g", "target"], b"", "C.UTF-8"),
        0,
        b"",
    );
    assert_eq!(fs::read(&target).unwrap(), b"X\xffX\n");
}

#[test]
fn next_commands_at_separate_file_eof_continue_with_later_operands() {
    let scratch = Scratch::new();
    for next in ["n", "N"] {
        for name in ["a", "b"] {
            fs::write(scratch.path().join(name), b"a\n").unwrap();
        }
        let script = format!("y/a/A/;{next}");
        expect_output(
            run(scratch.path(), &["-s", &script, "a", "b"], b""),
            0,
            b"A\nA\n",
        );
        expect_output(
            run(scratch.path(), &["-i.bak", &script, "a", "b"], b""),
            0,
            b"",
        );
        for name in ["a", "b"] {
            assert_eq!(
                fs::read(scratch.path().join(name)).unwrap(),
                b"A\n",
                "next={next}, file={name}"
            );
            assert_eq!(
                fs::read(scratch.path().join(format!("{name}.bak"))).unwrap(),
                b"a\n"
            );
        }
    }
}

#[test]
fn anchors_use_pattern_space_boundaries_not_a_trailing_lf_character() {
    let scratch = Scratch::new();
    for locale in ["C", "C.UTF-8"] {
        for args in [&["-z", "s/$/X/"][..], &["-z", "-E", "s/$/X/"][..]] {
            expect_output(
                run_locale(scratch.path(), args, b"a\n\0", locale),
                0,
                b"a\nX\0",
            );
        }
        expect_output(
            run_locale(scratch.path(), &["N;s/$/X/"], b"a\n\n", locale),
            0,
            b"a\nX\n",
        );
        expect_output(
            run_locale(scratch.path(), &["-z", "-n", "/^a$/p"], b"a\n\0", locale),
            0,
            b"",
        );
        expect_output(
            run_locale(scratch.path(), &[r"s/\$/X/"], b"$\n", locale),
            0,
            b"X\n",
        );
        expect_output(
            run_locale(scratch.path(), &[r"s/\A/X/"], b"A\n", locale),
            0,
            b"X\n",
        );
        expect_output(
            run_locale(scratch.path(), &[r"s/\z/X/"], b"z\n", locale),
            0,
            b"X\n",
        );
        expect_output(
            run_locale(scratch.path(), &["s/[$]/X/"], b"$\n", locale),
            0,
            b"X\n",
        );
    }
}

#[test]
fn clustered_in_place_options_preserve_flags_suffixes_and_operand_boundaries() {
    let scratch = Scratch::new();
    let target = scratch.path().join("target");
    let original = b"a\xffb\0";
    for (option, script, backup) in [
        ("-zi", "s/a/A/", None),
        ("-zsi", "s/a/A/", None),
        ("-nzi", "s/a/A/p", None),
        ("-zni*.bak", "s/a/A/p", Some("target.bak")),
        ("-iz", "s/a/A/", Some("targetz")),
    ] {
        fs::write(&target, original).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        expect_output(
            run(scratch.path(), &[option, script, "target"], b""),
            0,
            b"",
        );
        assert_eq!(fs::read(&target).unwrap(), b"A\xffb\0", "{option}");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        if let Some(backup) = backup {
            assert_eq!(
                fs::read(scratch.path().join(backup)).unwrap(),
                original,
                "{option}"
            );
        }
    }
    fs::write(&target, original).unwrap();
    let args = [
        OsString::from_vec(b"-zi*\xff.bak".to_vec()),
        OsString::from("s/a/A/"),
        OsString::from("target"),
    ];
    expect_output(run(scratch.path(), &args, b""), 0, b"");
    assert_eq!(fs::read(&target).unwrap(), b"A\xffb\0");
    assert_eq!(
        fs::read(
            scratch
                .path()
                .join(OsString::from_vec(b"target\xff.bak".to_vec()))
        )
        .unwrap(),
        original
    );

    fs::write(scratch.path().join("-i.script"), b"p\n").unwrap();
    fs::write(scratch.path().join("--"), b"p\n").unwrap();
    fs::write(scratch.path().join("-input"), b"a\n").unwrap();
    for args in [
        &["-nf", "-i.script", "--", "-input"][..],
        &["-n", "--file", "-i.script", "--", "-input"][..],
        &["-nf", "--", "--", "-input"][..],
        &["-nf-i.script", "--", "-input"][..],
        &["-nep", "--", "-input"][..],
    ] {
        expect_output(run(scratch.path(), args, b""), 0, b"a\n");
    }
    fs::write(scratch.path().join("-zi"), b"operand\n").unwrap();
    expect_output(
        run(scratch.path(), &["-e", "", "--", "-zi"], b""),
        0,
        b"operand\n",
    );
    assert_eq!(fs::read(scratch.path().join("-input")).unwrap(), b"a\n");
}

#[cfg(target_os = "linux")]
fn checked_full_device() -> fs::File {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};

    let expected = fs::symlink_metadata("/dev/full").expect("inspect /dev/full");
    assert!(
        expected.file_type().is_char_device(),
        "/dev/full must not be a link or regular file"
    );
    assert_eq!(
        expected.rdev(),
        libc::makedev(1, 7),
        "require the Linux full device"
    );
    let file = fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open("/dev/full")
        .expect("open verified full device without creating or truncating");
    let actual = file.metadata().unwrap();
    assert!(actual.file_type().is_char_device());
    assert_eq!(
        (actual.dev(), actual.ino(), actual.rdev()),
        (expected.dev(), expected.ino(), expected.rdev())
    );
    file
}

#[cfg(target_os = "linux")]
#[test]
fn stream_write_failures_have_exit_four_and_override_earlier_missing_inputs() {
    let scratch = Scratch::new();
    let input = scratch.path().join("input");
    fs::write(&input, b"a\nsecond\n").unwrap();
    let full = checked_full_device();
    for args in [
        &["s/a/A/"][..],
        &["q42"][..],
        &["s/a/A/", "missing", "input"][..],
    ] {
        let output = sed_command(scratch.path(), args, "C")
            .stdin(Stdio::from(fs::File::open(&input).unwrap()))
            .stdout(Stdio::from(full.try_clone().unwrap()))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(4), "{args:?}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("No space left on device"),
            "{output:?}"
        );
    }
    let output = sed_command(scratch.path(), &["", "missing"], "C")
        .stdin(Stdio::null())
        .stdout(Stdio::from(full))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stderr.starts_with(b"sed: can't read missing: "));
}

#[cfg(target_os = "linux")]
#[test]
fn in_place_write_failures_are_fatal_without_publishing_output_or_backups() {
    let scratch = Scratch::new();
    let original = b"a\xff\r\n";
    fs::write(scratch.path().join("target"), original).unwrap();
    fs::write(scratch.path().join("later"), b"a\n").unwrap();
    let full = checked_full_device();
    // The child reopens its verified inherited descriptor, not a mutable host path.
    let output = sed_command(
        scratch.path(),
        &[
            "-i.bak",
            "s/a/A/;w /proc/self/fd/1",
            "missing",
            "target",
            "later",
        ],
        "C",
    )
    .stdin(Stdio::null())
    .stdout(Stdio::from(full))
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert!(output.stderr.starts_with(b"sed: can't read missing: "));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("No space left on device"),
        "{output:?}"
    );
    assert_eq!(fs::read(scratch.path().join("target")).unwrap(), original);
    assert_eq!(fs::read(scratch.path().join("later")).unwrap(), b"a\n");
    assert!(!scratch.path().join("target.bak").exists());
    assert!(!scratch.path().join("later.bak").exists());
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 2);
}

#[cfg(target_os = "linux")]
#[test]
fn initial_and_late_stream_read_failures_have_exit_four_and_keep_prior_output() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("input"), b"a\xff\r\n").unwrap();
    symlink("/proc/self/mem", scratch.path().join("fault")).unwrap();
    for (files, expected) in [
        (&["fault"][..], b"".as_slice()),
        (&["input", "fault"][..], b"A\xff\r\n".as_slice()),
        (&["missing", "input", "fault"][..], b"A\xff\r\n".as_slice()),
    ] {
        let mut args = vec!["s/a/A/"];
        args.extend_from_slice(files);
        let output = run(scratch.path(), &args, b"");
        assert_eq!(output.status.code(), Some(4), "{output:?}");
        assert_eq!(output.stdout, expected);
        assert!(!output.stderr.is_empty());
    }
    let output = run(
        scratch.path(),
        &["-i.bak", "s/a/A/", "missing", "fault", "input"],
        b"",
    );
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert!(output.stderr.starts_with(b"sed: can't read missing: "));
    assert_eq!(
        fs::read_link(scratch.path().join("fault")).unwrap(),
        Path::new("/proc/self/mem")
    );
    assert_eq!(
        fs::read(scratch.path().join("input")).unwrap(),
        b"a\xff\r\n"
    );
    assert!(!scratch.path().join("fault.bak").exists());
    assert!(!scratch.path().join("input.bak").exists());
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 2);
}

#[test]
fn byte_scripts_from_e_and_f_preserve_invalid_bytes() {
    let scratch = Scratch::new();
    let script = OsString::from_vec(b"s/[\xff]/X/".to_vec());
    expect_output(run(scratch.path(), &[script], b"a\xffb\n"), 0, b"aXb\n");

    let script_file = scratch.path().join("script");
    fs::write(&script_file, b"s/[\xff]/Y/").unwrap();
    let file_args = [OsString::from("-f"), script_file.as_os_str().to_os_string()];
    expect_output(run(scratch.path(), &file_args, b"a\xffb\n"), 0, b"aYb\n");
}

#[test]
fn y_uses_resolved_locale_encoding_and_gnu_escapes() {
    let scratch = Scratch::new();
    expect_output(
        run_locale(scratch.path(), &[r"y/\xFF/X/"], b"a\xc3\xa9\xff\n", "C"),
        0,
        b"a\xc3\xa9X\n",
    );
    expect_output(
        run_locale(
            scratch.path(),
            &[r"y/\xFF/X/"],
            b"a\xc3\xa9\xff\n",
            "C.UTF-8",
        ),
        0,
        b"a\xc3\xa9X\n",
    );
    expect_output(
        run_locale(scratch.path(), &["y/é/É/"], "é\n".as_bytes(), "C.UTF-8"),
        0,
        "É\n".as_bytes(),
    );
    let output = run_locale(scratch.path(), &["y/é/XY/"], "é\n".as_bytes(), "C.UTF-8");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    expect_output(
        run_locale(
            scratch.path(),
            &["y/é/XY/"],
            "é\n".as_bytes(),
            "zz_ZZ.UTF-8",
        ),
        0,
        b"XY\n",
    );
}

#[test]
fn w_targets_are_truncated_and_created_once_before_processing() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("input"), b"a\nb\n").unwrap();
    fs::write(scratch.path().join("stale"), b"old content").unwrap();
    let script = "1d\nw stale\nw untouched\ns/a/A/w matched\n";
    expect_output(run(scratch.path(), &[script, "input"], b""), 0, b"b\n");
    assert_eq!(fs::read(scratch.path().join("stale")).unwrap(), b"b\n");
    assert_eq!(fs::read(scratch.path().join("untouched")).unwrap(), b"b\n");
    assert!(fs::read(scratch.path().join("matched")).unwrap().is_empty());

    fs::write(scratch.path().join("input"), b"a\n").unwrap();
    expect_output(
        run(scratch.path(), &["s/a/A/w matched\n", "input"], b""),
        0,
        b"A\n",
    );
    assert_eq!(fs::read(scratch.path().join("matched")).unwrap(), b"A\n");
}

#[test]
fn relative_address_ranges_match_gnu() {
    let scratch = Scratch::new();
    let input = b"1\n2\n3\n4\n5\n6\n7\n8\n";
    for (script, expected) in [
        ("2,+2p", b"2\n3\n4\n".as_slice()),
        ("2,+0p", b"2\n".as_slice()),
        ("2,~3p", b"2\n3\n".as_slice()),
        ("3,~3p", b"3\n4\n5\n6\n".as_slice()),
        ("2,~0p", b"2\n".as_slice()),
    ] {
        expect_output(run(scratch.path(), &["-n", script], input), 0, expected);
    }
}

#[test]
fn in_place_clears_set_id_but_preserves_sticky_mode() {
    let scratch = Scratch::new();
    for (index, mode) in [0o4751, 0o2751, 0o6751, 0o1751].into_iter().enumerate() {
        let path = scratch.path().join(format!("target-{index}"));
        fs::write(&path, b"x\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let name = path.file_name().unwrap();
        let args = [
            OsString::from("-i"),
            OsString::from("s/x/y/"),
            name.to_os_string(),
        ];
        expect_output(run(scratch.path(), &args, b""), 0, b"");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            mode & !0o6000
        );
    }
}
