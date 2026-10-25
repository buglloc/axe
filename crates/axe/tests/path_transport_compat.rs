#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "axe-path-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private fixture: {error}"),
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

fn command(directory: &Path, applet: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", applet, "--"])
        .current_dir(directory)
        .env_clear()
        .env("HOME", directory)
        .env("PATH", "/nonexistent")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    command
}

fn output(command: &mut Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn applet");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .expect("write fixture input");
    child.wait_with_output().expect("wait for applet")
}

fn run(directory: &Path, applet: &str, args: &[&OsStr], input: &[u8]) -> Output {
    output(command(directory, applet).args(args), input)
}

fn text_args<'a>(args: &'a [&'a str]) -> Vec<&'a OsStr> {
    args.iter().map(OsStr::new).collect()
}

fn success(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn find_print0_selects_and_removes_only_the_original_byte_path() {
    let scratch = Scratch::new("find-consumer");
    fs::create_dir(scratch.path().join("only")).unwrap();
    let selected = OsStr::from_bytes(b"only/raw-\xff");
    let unselected = "only/raw-\u{fffd}";
    fs::write(scratch.path().join(selected), b"selected\n").unwrap();
    fs::write(
        scratch.path().join(unselected),
        b"UNSELECTED FILE MUST SURVIVE\n",
    )
    .unwrap();
    let inode = fs::metadata(scratch.path().join(selected))
        .unwrap()
        .ino()
        .to_string();
    let found = run(
        scratch.path(),
        "find",
        &[
            OsStr::new("only"),
            OsStr::new("-inum"),
            OsStr::new(&inode),
            OsStr::new("-print0"),
        ],
        b"",
    );
    success(&found);
    assert_eq!(found.stdout, b"only/raw-\xff\0");
    let removed = run(
        scratch.path(),
        "xargs",
        &text_args(&["-0", "rm", "--"]),
        &found.stdout,
    );
    success(&removed);
    assert!(!scratch.path().join(selected).exists());
    assert_eq!(
        fs::read(scratch.path().join(unselected)).unwrap(),
        b"UNSELECTED FILE MUST SURVIVE\n"
    );
}

#[test]
fn find_accepts_byte_roots_patterns_and_files0_lists() {
    let scratch = Scratch::new("find-argv");
    let root = OsStr::from_bytes(b"root-\xff");
    fs::create_dir(scratch.path().join(root)).unwrap();
    fs::write(
        scratch
            .path()
            .join(root)
            .join(OsStr::from_bytes(b"raw-\xff")),
        b"yes\n",
    )
    .unwrap();
    fs::write(scratch.path().join(root).join("raw-\u{fffd}"), b"no\n").unwrap();
    for predicate in ["-name", "-iname"] {
        let found = run(
            scratch.path(),
            "find",
            &[
                root,
                OsStr::new(predicate),
                OsStr::from_bytes(b"raw-\xff"),
                OsStr::new("-print"),
            ],
            b"",
        );
        success(&found);
        assert_eq!(found.stdout, b"root-\xff/raw-\xff\n");
    }
    let found = run(
        scratch.path(),
        "find",
        &[
            root,
            OsStr::new("-path"),
            OsStr::from_bytes(b"root-\xff/raw-\xff"),
            OsStr::new("-print0"),
        ],
        b"",
    );
    success(&found);
    assert_eq!(found.stdout, b"root-\xff/raw-\xff\0");
    let list = OsStr::from_bytes(b"list-\xff");
    let printed = OsStr::from_bytes(b"printed-\xff");
    fs::write(scratch.path().join(list), b"root-\xff/raw-\xff\0").unwrap();
    let found = run(
        scratch.path(),
        "find",
        &[
            OsStr::new("-files0-from"),
            list,
            OsStr::new("-fprint0"),
            printed,
        ],
        b"",
    );
    success(&found);
    assert!(found.stdout.is_empty());
    assert_eq!(
        fs::read(scratch.path().join(printed)).unwrap(),
        b"root-\xff/raw-\xff\0"
    );
}

#[test]
fn find_printf_and_regex_preserve_raw_path_bytes() {
    let scratch = Scratch::new("find-printf-regex");
    let raw = OsStr::from_bytes(b"raw-\xff");
    let replacement = OsStr::new("raw-\u{fffd}");
    fs::write(scratch.path().join(raw), b"raw\n").unwrap();
    fs::write(scratch.path().join(replacement), b"replacement\n").unwrap();

    for (directive, expected) in [
        (b"%p\\0".as_slice(), b"raw-\xff\0".as_slice()),
        (b"%f\\0".as_slice(), b"raw-\xff\0".as_slice()),
    ] {
        for (path, expected) in [(raw, expected), (replacement, b"raw-\xef\xbf\xbd\0")] {
            let result = run(
                scratch.path(),
                "find",
                &[
                    path,
                    OsStr::new("-maxdepth"),
                    OsStr::new("0"),
                    OsStr::new("-printf"),
                    OsStr::from_bytes(directive),
                ],
                b"",
            );
            success(&result);
            assert_eq!(result.stdout, expected);
        }
    }
    let precision = run(
        scratch.path(),
        "find",
        &[
            raw,
            OsStr::new("-maxdepth"),
            OsStr::new("0"),
            OsStr::new("-printf"),
            OsStr::new("%8.3p\\0"),
        ],
        b"",
    );
    success(&precision);
    assert_eq!(precision.stdout, b"     raw\0");

    let fprintf_output = scratch.path().join("fprintf-output");
    let fprintf = run(
        scratch.path(),
        "find",
        &[
            raw,
            OsStr::new("-maxdepth"),
            OsStr::new("0"),
            OsStr::new("-fprintf"),
            fprintf_output.as_os_str(),
            OsStr::new("%p\\0"),
        ],
        b"",
    );
    success(&fprintf);
    assert!(fprintf.stdout.is_empty());
    assert_eq!(fs::read(fprintf_output).unwrap(), b"raw-\xff\0");

    let raw_parent = OsStr::from_bytes(b"parent-\xff");
    let replacement_parent = OsStr::new("parent-\u{fffd}");
    let raw_child = scratch.path().join(raw_parent).join("leaf");
    let replacement_child = scratch.path().join(replacement_parent).join("leaf");
    fs::create_dir(scratch.path().join(raw_parent)).unwrap();
    fs::create_dir(scratch.path().join(replacement_parent)).unwrap();
    fs::write(&raw_child, b"raw parent\n").unwrap();
    fs::write(&replacement_child, b"replacement parent\n").unwrap();
    let raw_child_arg = Path::new(raw_parent).join("leaf");
    let replacement_child_arg = Path::new(replacement_parent).join("leaf");
    for (path, expected) in [
        (raw_child_arg.as_os_str(), b"parent-\xff\0".as_slice()),
        (
            replacement_child_arg.as_os_str(),
            b"parent-\xef\xbf\xbd\0".as_slice(),
        ),
    ] {
        let result = run(
            scratch.path(),
            "find",
            &[
                path,
                OsStr::new("-maxdepth"),
                OsStr::new("0"),
                OsStr::new("-printf"),
                OsStr::new("%h\\0"),
            ],
            b"",
        );
        success(&result);
        assert_eq!(result.stdout, expected);
    }

    for (predicate, pattern) in [
        ("-regex", OsString::from_vec(b".*/raw-\xff".to_vec())),
        ("-iregex", OsString::from_vec(b".*/RAW-\xff".to_vec())),
    ] {
        let result = run(
            scratch.path(),
            "find",
            &[
                OsStr::new("."),
                OsStr::new(predicate),
                pattern.as_os_str(),
                OsStr::new("-printf"),
                OsStr::new("%p\\0"),
            ],
            b"",
        );
        success(&result);
        assert_eq!(result.stdout, b"./raw-\xff\0", "{predicate}");
    }
}

#[test]
fn find_exec_preserves_byte_literals_and_embedded_path_replacement() {
    let scratch = Scratch::new("find-exec");
    let name = OsStr::from_bytes(b"raw-\xff");
    fs::write(scratch.path().join(name), b"yes\n").unwrap();
    let executable = OsStr::from_bytes(b"command-\xff");
    symlink(env!("CARGO_BIN_EXE_axe"), scratch.path().join(executable)).unwrap();
    let executable_path = Path::new(".").join(executable);
    let found = run(
        scratch.path(),
        "find",
        &[
            name,
            OsStr::new("-exec"),
            executable_path.as_os_str(),
            OsStr::new("--applet"),
            OsStr::new("printf"),
            OsStr::new("--"),
            OsStr::new("%s\\0"),
            OsStr::from_bytes(b"literal-\xfe"),
            OsStr::from_bytes(b"before-\xfe{}-after"),
            OsStr::new(";"),
        ],
        b"",
    );
    success(&found);
    assert_eq!(found.stdout, b"literal-\xfe\0before-\xferaw-\xff-after\0");
}

#[cfg(target_os = "linux")]
#[test]
fn find_printf_reports_output_write_failures() {
    let scratch = Scratch::new("find-printf-output");
    let result = command(scratch.path(), "find")
        .args([
            OsStr::new("."),
            OsStr::new("-maxdepth"),
            OsStr::new("0"),
            OsStr::new("-printf"),
            OsStr::new("%p\\n"),
        ])
        .stdin(Stdio::null())
        .stdout(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(!result.stderr.is_empty(), "{result:?}");
}

#[test]
fn grep_and_aliases_emit_reopenable_filename_bytes_in_all_filename_modes() {
    let scratch = Scratch::new("grep-filenames");
    let name = OsStr::from_bytes(b"name-\xff");
    fs::write(scratch.path().join(name), b"yes\n").unwrap();
    fs::write(scratch.path().join("name-\u{fffd}"), b"wrong file\n").unwrap();
    for applet in ["grep", "egrep", "fgrep", "rgrep"] {
        for (flags, pattern, expected) in [
            ("-lZ", "yes", b"name-\xff\0".as_slice()),
            ("-LZ", "absent", b"name-\xff\0".as_slice()),
            ("-H", "yes", b"name-\xff:yes\n".as_slice()),
            ("-Hc", "yes", b"name-\xff:1\n".as_slice()),
        ] {
            let matched = run(
                scratch.path(),
                applet,
                &[OsStr::new(flags), OsStr::new(pattern), name],
                b"",
            );
            if flags == "-LZ" {
                assert_eq!(matched.status.code(), Some(1), "{matched:?}");
                assert!(matched.stderr.is_empty(), "{matched:?}");
            } else {
                success(&matched);
            }
            assert_eq!(matched.stdout, expected, "{applet} {flags}");
        }
    }
    fs::create_dir(scratch.path().join("tree")).unwrap();
    fs::write(scratch.path().join("tree").join(name), b"yes\n").unwrap();
    let matched = run(
        scratch.path(),
        "rgrep",
        &text_args(&["-lZ", "yes", "tree"]),
        b"",
    );
    success(&matched);
    assert_eq!(matched.stdout, b"tree/name-\xff\0");
}

#[test]
fn grep_quiet_preserves_missing_file_diagnostics_and_exit_classes() {
    let scratch = Scratch::new("grep-quiet");
    fs::write(scratch.path().join("present"), b"yes\n").unwrap();
    for flag in ["-q", "-s", "-qs"] {
        let matched = run(
            scratch.path(),
            "grep",
            &text_args(&[flag, "yes", "missing"]),
            b"",
        );
        assert_eq!(matched.status.code(), Some(2), "{matched:?}");
        assert!(matched.stdout.is_empty());
        if flag == "-q" {
            assert!(
                matched
                    .stderr
                    .windows(b"missing".len())
                    .any(|part| part == b"missing"),
                "{matched:?}"
            );
        } else {
            assert!(matched.stderr.is_empty(), "{matched:?}");
        }
    }
    let matched = run(
        scratch.path(),
        "grep",
        &text_args(&["-q", "yes", "missing", "present"]),
        b"",
    );
    assert_eq!(matched.status.code(), Some(0), "{matched:?}");
    assert!(matched.stdout.is_empty());
    assert!(!matched.stderr.is_empty(), "{matched:?}");
    assert!(
        matched
            .stderr
            .windows(b"missing".len())
            .any(|part| part == b"missing"),
        "{matched:?}"
    );
    let unmatched = run(scratch.path(), "grep", &text_args(&["no", "present"]), b"");
    assert_eq!(unmatched.status.code(), Some(1), "{unmatched:?}");
    assert!(unmatched.stdout.is_empty());
    assert!(unmatched.stderr.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn grep_stdout_failure_is_an_error_not_no_match() {
    let scratch = Scratch::new("grep-output");
    let mut child = command(scratch.path(), "grep")
        .arg("yes")
        .stdin(Stdio::piped())
        .stdout(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"yes\n").unwrap();
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(2), "{result:?}");
    assert!(!result.stderr.is_empty());
}

#[test]
fn xargs_replacement_preserves_internal_blanks_tabs_and_bytes() {
    let scratch = Scratch::new("xargs-replace");
    for flags in [&["-I{}"][..], &["--replace={}"][..], &["--replace"][..]] {
        let mut args = flags.to_vec();
        args.extend(["printf", "<%s>\\n", "prefix{}suffix"]);
        let result = run(
            scratch.path(),
            "xargs",
            &text_args(&args),
            b"  a  b\tc-\xff \n\"quoted value\"\n",
        );
        success(&result);
        assert_eq!(
            result.stdout,
            b"<prefixa  b\tc-\xff suffix>\n<prefixquoted valuesuffix>\n"
        );
        let empty = run(scratch.path(), "xargs", &text_args(&args), b"");
        success(&empty);
        assert!(empty.stdout.is_empty());
    }
}

#[test]
fn xargs_eof_options_follow_gnu_record_boundaries() {
    let scratch = Scratch::new("xargs-eof");
    let cases: &[(&[&str], &[u8], &[u8])] = &[
        (
            &["-E", "STOP", "printf", "<%s>\\n"],
            b"a\nSTOP\nb\n",
            b"<a>\n",
        ),
        (&["-eSTOP", "printf", "<%s>\\n"], b"a\nSTOP\nb\n", b"<a>\n"),
        (
            &["--eof=STOP", "printf", "<%s>\\n"],
            b"a\nSTOP\nb\n",
            b"<a>\n",
        ),
        (
            &["-E", "STOP", "-0", "printf", "<%s>\\n"],
            b"a\0STOP\0b\0",
            b"<a>\n<STOP>\n<b>\n",
        ),
        (
            &["-E", "STOP", "-d", ":", "printf", "<%s>\\n"],
            b"a:STOP:b:",
            b"<a>\n<STOP>\n<b>\n",
        ),
        (
            &["-E", "STOP", "-L1", "printf", "<%s>\\n"],
            b"a STOP\nb\n",
            b"<a>\n",
        ),
        (
            &["-E", "STOP", "-I{}", "printf", "<%s>\\n", "{}"],
            b"a STOP\nSTOP\nb\n",
            b"<a STOP>\n",
        ),
    ];
    for (args, input, expected) in cases {
        let result = run(scratch.path(), "xargs", &text_args(args), input);
        success(&result);
        assert_eq!(result.stdout, *expected, "{args:?}");
    }
}

#[test]
fn xargs_logical_lines_and_control_byte_separators_preserve_arguments() {
    let scratch = Scratch::new("xargs-lines");
    for flag in ["-L1", "--max-lines=1", "--max-lines"] {
        let result = run(
            scratch.path(),
            "xargs",
            &text_args(&[flag, "printf", "<%s><%s>\\n"]),
            b"a \n\nb\nc\t\nd\n'a '\nb\na\\ \nb\n",
        );
        success(&result);
        assert_eq!(result.stdout, b"<a><b>\n<c><d>\n<a ><>\n<b><>\n<a ><b>\n");
    }
    let result = run(
        scratch.path(),
        "xargs",
        &text_args(&["printf", "<%s>\\n"]),
        b"\x0b\x0c\r a\x0bb\x0cc\rd\n",
    );
    success(&result);
    assert_eq!(result.stdout, b"<a\x0bb\x0cc\rd>\n");
    let quoted = run(
        scratch.path(),
        "xargs",
        &text_args(&["printf", "<%s>\\n"]),
        b"\"a\\b\" a\\\nb\n",
    );
    success(&quoted);
    assert_eq!(quoted.stdout, b"<a\\b>\n<a\nb>\n");
}

#[test]
fn xargs_long_values_and_size_limits_preserve_every_argument() {
    let scratch = Scratch::new("xargs-options");
    let result = run(
        scratch.path(),
        "xargs",
        &text_args(&[
            "--max-args=1",
            "--max-procs=1",
            "--delimiter=:",
            "printf",
            "<%s>\\n",
        ]),
        b"a b:c\t d:",
    );
    success(&result);
    assert_eq!(result.stdout, b"<a b>\n<c\t d>\n");
    // printf\0 plus the format uses 18 bytes; each input argument needs another 6.
    for flag in ["-s24", "--max-chars=24"] {
        let result = run(
            scratch.path(),
            "xargs",
            &text_args(&[flag, "printf", "<%s><%s>\\n"]),
            b"aaaaa bbbbb ccccc\n",
        );
        success(&result);
        assert_eq!(result.stdout, b"<aaaaa><>\n<bbbbb><>\n<ccccc><>\n");
    }
    let oversized = run(
        scratch.path(),
        "xargs",
        &text_args(&["-s5", "printf", "<%s>\\n"]),
        b"a\n",
    );
    assert_eq!(oversized.status.code(), Some(1), "{oversized:?}");
    assert!(oversized.stdout.is_empty());
}

#[test]
fn tree_shows_targets_escaped_names_raw_roots_and_missing_directory_status() {
    let scratch = Scratch::new("tree-paths");
    let root = OsStr::from_bytes(b"root-\xff");
    fs::create_dir(scratch.path().join(root)).unwrap();
    for name in [b"a space".as_slice(), b"line\nbreak", b"raw-\xff"] {
        fs::write(
            scratch.path().join(root).join(OsStr::from_bytes(name)),
            b"yes\n",
        )
        .unwrap();
    }
    symlink("a space", scratch.path().join(root).join("link")).unwrap();
    symlink("missing", scratch.path().join(root).join("dangling")).unwrap();
    fs::create_dir(scratch.path().join("other")).unwrap();
    fs::write(scratch.path().join("other/child"), b"not traversed\n").unwrap();
    symlink("../other", scratch.path().join(root).join("dirlink")).unwrap();
    let result = run(
        scratch.path(),
        "tree",
        &[OsStr::new("-i"), OsStr::new("--noreport"), root],
        b"",
    );
    success(&result);
    assert_eq!(result.stdout, b"root-\\377\na\\ space\ndangling -> missing\ndirlink -> ../other\nline\\nbreak\nlink -> a\\ space\nraw-\\377\n");
    let result = run(scratch.path(), "tree", &text_args(&["missing"]), b"");
    assert_eq!(result.status.code(), Some(2), "{result:?}");
    assert_eq!(
        result.stdout,
        b"missing  [error opening dir]\n\n0 directories, 0 files\n"
    );
    assert!(result.stderr.is_empty());
}

#[test]
fn tree_filters_unfollowed_directory_links_without_losing_directory_only_reporting() {
    let scratch = Scratch::new("tree-pattern-prune");
    fs::create_dir_all(scratch.path().join("root/nested")).unwrap();
    fs::create_dir(scratch.path().join("other")).unwrap();
    fs::write(scratch.path().join("root/nested/leaf.txt"), b"leaf\n").unwrap();
    fs::write(scratch.path().join("other/other.txt"), b"outside\n").unwrap();
    symlink("../other", scratch.path().join("root/dirlink")).unwrap();

    let filtered = run(
        scratch.path(),
        "tree",
        &text_args(&["-i", "-P", "*.txt", "--prune", "--noreport", "root"]),
        b"",
    );
    success(&filtered);
    assert_eq!(filtered.stdout, b"root\nnested\nleaf.txt\n");

    let directories = run(
        scratch.path(),
        "tree",
        &text_args(&["-di", "--noreport", "root"]),
        b"",
    );
    success(&directories);
    assert_eq!(directories.stdout, b"root\ndirlink -> ../other\nnested\n");
}

#[test]
fn which_outputs_exact_path_bytes_and_actionable_missing_diagnostics() {
    let scratch = Scratch::new("which-paths");
    let bin = scratch.path().join(OsStr::from_bytes(b"bin-\xff"));
    fs::create_dir(&bin).unwrap();
    let name = OsString::from_vec(b"compat-\xff".to_vec());
    let executable = bin.join(&name);
    fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let result = output(
        command(scratch.path(), "which")
            .env("PATH", &bin)
            .arg(&name),
        b"",
    );
    success(&result);
    let mut expected = executable.as_os_str().as_bytes().to_vec();
    expected.push(b'\n');
    assert_eq!(result.stdout, expected);
    let result = output(
        command(scratch.path(), "which")
            .env("PATH", &bin)
            .arg("absent"),
        b"",
    );
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(result.stdout.is_empty());
    assert!(!result.stderr.is_empty(), "{result:?}");
    assert!(
        result
            .stderr
            .windows(b"absent".len())
            .any(|part| part == b"absent"),
        "{result:?}"
    );
    let result = run(scratch.path(), "which", &text_args(&["dir/absent"]), b"");
    assert_eq!(result.status.code(), Some(1));
    assert!(!result.stderr.is_empty());
    assert!(
        result
            .stderr
            .windows(b"absent".len())
            .any(|part| part == b"absent")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn tree_and_which_do_not_hide_output_write_failures() {
    let scratch = Scratch::new("output-errors");
    fs::write(scratch.path().join("executable"), b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        scratch.path().join("executable"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    for (applet, args, expected) in [
        ("tree", &["--noreport", "."][..], 2),
        ("which", &["./executable"][..], 1),
    ] {
        let result = command(scratch.path(), applet)
            .args(args)
            .stdin(Stdio::null())
            .stdout(
                fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/full")
                    .unwrap(),
            )
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(expected), "{applet}: {result:?}");
        assert!(!result.stderr.is_empty(), "{applet}: {result:?}");
    }
}

#[test]
fn find_parenthesized_byte_operands_remain_selective_for_real_consumers() {
    let scratch = Scratch::new("find-parentheses");
    fs::create_dir(scratch.path().join("only")).unwrap();
    let selected = OsStr::from_bytes(b"only/raw-\xff");
    let unselected = "only/raw-\u{fffd}";
    fs::write(scratch.path().join(selected), b"selected\n").unwrap();
    fs::write(
        scratch.path().join(unselected),
        b"UNSELECTED FILE MUST SURVIVE\n",
    )
    .unwrap();
    for (predicate, operand) in [
        ("-name", OsStr::from_bytes(b"raw-\xff")),
        ("-path", selected),
        ("-samefile", selected),
    ] {
        let result = run(
            scratch.path(),
            "find",
            &[
                OsStr::new("only"),
                OsStr::new("("),
                OsStr::new(predicate),
                operand,
                OsStr::new(")"),
                OsStr::new("-print0"),
            ],
            b"",
        );
        success(&result);
        assert_eq!(result.stdout, b"only/raw-\xff\0", "{predicate}");
    }
    let printed = OsStr::from_bytes(b"printed-\xff");
    let result = run(
        scratch.path(),
        "find",
        &[
            OsStr::new("only"),
            OsStr::new("("),
            OsStr::new("-name"),
            OsStr::from_bytes(b"raw-\xff"),
            OsStr::new("-fprint0"),
            printed,
            OsStr::new(")"),
        ],
        b"",
    );
    success(&result);
    assert!(result.stdout.is_empty());
    let paths = fs::read(scratch.path().join(printed)).unwrap();
    assert_eq!(paths, b"only/raw-\xff\0");
    let consumed = run(
        scratch.path(),
        "xargs",
        &text_args(&["-0", "rm", "--"]),
        &paths,
    );
    success(&consumed);
    assert!(!scratch.path().join(selected).exists());
    assert_eq!(
        fs::read(scratch.path().join(unselected)).unwrap(),
        b"UNSELECTED FILE MUST SURVIVE\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn find_uses_requested_utf8_locale_without_losing_raw_filename_bytes() {
    let scratch = Scratch::new("find-locale");
    for directory in ["wildcard", "casefold", "bytes", "mixed"] {
        fs::create_dir(scratch.path().join(directory)).unwrap();
    }
    fs::write(scratch.path().join("wildcard/é"), b"unicode wildcard\n").unwrap();
    fs::write(scratch.path().join("casefold/É"), b"unicode casefold\n").unwrap();
    fs::write(scratch.path().join("mixed/É"), b"unicode mixed\n").unwrap();
    fs::write(
        scratch.path().join(OsStr::from_bytes(b"mixed/raw-\xff")),
        b"raw mixed\n",
    )
    .unwrap();
    fs::write(
        scratch.path().join(OsStr::from_bytes(b"bytes/raw-\xff")),
        b"raw filename\n",
    )
    .unwrap();
    fs::write(
        scratch.path().join("bytes/raw-\u{fffd}"),
        b"WRONG UNICODE DOUBLE\n",
    )
    .unwrap();
    for (args, paths, contents) in [
        (
            &["wildcard", "-name", "?", "-print0"][..],
            "wildcard/é\0".as_bytes(),
            b"unicode wildcard\n".as_slice(),
        ),
        (
            &["casefold", "-iname", "é", "-print0"][..],
            "casefold/É\0".as_bytes(),
            b"unicode casefold\n".as_slice(),
        ),
        (
            &["casefold", "-ipath", "casefold/é", "-print0"][..],
            "casefold/É\0".as_bytes(),
            b"unicode casefold\n".as_slice(),
        ),
    ] {
        let result = output(
            command(scratch.path(), "find")
                .env("LC_ALL", "C.UTF-8")
                .args(args),
            b"",
        );
        success(&result);
        assert_eq!(result.stdout, paths, "{args:?}");
        let consumed = run(
            scratch.path(),
            "xargs",
            &text_args(&["-0", "cat"]),
            &result.stdout,
        );
        success(&consumed);
        assert_eq!(consumed.stdout, contents, "{args:?}");
    }
    let result = run(
        scratch.path(),
        "find",
        &text_args(&["wildcard", "-name", "?", "-print0"]),
        b"",
    );
    success(&result);
    assert!(
        result.stdout.is_empty(),
        "C locale is byte-oriented: {result:?}"
    );
    let result = output(
        command(scratch.path(), "find")
            .env("LC_ALL", "C.UTF-8")
            .args([
                OsStr::new("bytes"),
                OsStr::new("("),
                OsStr::new("-name"),
                OsStr::from_bytes(b"raw-\xff"),
                OsStr::new(")"),
                OsStr::new("-print0"),
            ]),
        b"",
    );
    success(&result);
    assert_eq!(result.stdout, b"bytes/raw-\xff\0");
    let consumed = run(
        scratch.path(),
        "xargs",
        &text_args(&["-0", "cat"]),
        &result.stdout,
    );
    success(&consumed);
    assert_eq!(consumed.stdout, b"raw filename\n");
    let mixed = output(
        command(scratch.path(), "find")
            .env("LC_ALL", "C.UTF-8")
            .args([
                OsStr::new("mixed"),
                OsStr::new("-sorted"),
                OsStr::new("("),
                OsStr::new("-name"),
                OsStr::from_bytes(b"raw-\xff"),
                OsStr::new("-o"),
                OsStr::new("-iname"),
                OsStr::new("é"),
                OsStr::new(")"),
                OsStr::new("-print0"),
            ]),
        b"",
    );
    success(&mixed);
    let mut expected = b"mixed/raw-\xff\0".to_vec();
    expected.extend_from_slice("mixed/É\0".as_bytes());
    assert_eq!(mixed.stdout, expected);
    let consumed = run(
        scratch.path(),
        "xargs",
        &text_args(&["-0", "cat"]),
        &mixed.stdout,
    );
    success(&consumed);
    assert_eq!(consumed.stdout, b"raw mixed\nunicode mixed\n");
}

fn system_argument_ceiling(invocation: &Command) -> usize {
    // SAFETY: sysconf takes no pointers and _SC_ARG_MAX is a valid selector.
    let configured = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
    assert!(configured > 0, "ARG_MAX is unavailable: {configured}");
    let mut environment_bytes = 0_usize;
    let mut environment_count = 0_usize;
    for (key, value) in invocation.get_envs() {
        let value = value.expect("fixture commands set all listed environment variables");
        environment_bytes += key.as_bytes().len() + value.as_bytes().len() + 2;
        environment_count += 1;
    }
    usize::try_from(configured)
        .unwrap()
        .saturating_sub(environment_bytes)
        .saturating_sub((environment_count + 1) * std::mem::size_of::<usize>())
        .saturating_sub(2048)
}

#[test]
fn xargs_explicit_size_can_exceed_default_while_preserving_each_expanded_argument() {
    let scratch = Scratch::new("xargs-larger-size");
    let payload = vec![b'a'; 80_000];
    let consumer = [
        env!("CARGO_BIN_EXE_axe"),
        "--applet",
        "printf",
        "--",
        "<%s>\\n",
        "{}",
        "{}",
    ];
    let initial_bytes = consumer[..consumer.len() - 2]
        .iter()
        .map(|argument| argument.len() + 1)
        .sum::<usize>();
    let required = initial_bytes
        + 2 * (payload.len() + 1)
        + (consumer.len() + 1) * std::mem::size_of::<usize>();
    let invocation = command(scratch.path(), "xargs");
    let ceiling = system_argument_ceiling(&invocation);
    if required > ceiling {
        eprintln!("ARG_MAX headroom {ceiling} is below the required {required} bytes");
        return;
    }
    let mut input = payload.clone();
    input.push(b'\n');
    let mut expected = Vec::new();
    for _ in 0..2 {
        expected.push(b'<');
        expected.extend_from_slice(&payload);
        expected.extend_from_slice(b">\n");
    }
    for size in ["-s262144", "--max-chars=262144"] {
        let mut args = vec![size, "-I{}"];
        args.extend(consumer);
        let result = run(scratch.path(), "xargs", &text_args(&args), &input);
        success(&result);
        assert_eq!(result.stdout, expected, "{size}");
    }
    let mut default_args = vec!["-I{}"];
    default_args.extend(consumer);
    let result = run(scratch.path(), "xargs", &text_args(&default_args), &input);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(result.stdout.is_empty());
}
