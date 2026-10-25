#![cfg(unix)]

use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static SCRATCH_ID: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-awk-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock is after Unix epoch")
                .as_nanos(),
            SCRATCH_ID.fetch_add(1, Ordering::Relaxed),
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("create owned awk scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, name: impl AsRef<Path>, bytes: &[u8]) {
        fs::write(self.0.join(name), bytes).expect("write awk fixture");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run<S: AsRef<OsStr>>(scratch: &Scratch, args: &[S], input: &[u8]) -> Output {
    run_locale(scratch, args, input, "C")
}

fn run_locale<S: AsRef<OsStr>>(
    scratch: &Scratch,
    args: &[S],
    input: &[u8],
    locale: &str,
) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "awk", "--"])
        .args(args)
        .current_dir(scratch.path())
        .env("LC_ALL", locale)
        .env("LANG", locale)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn awk applet");
    child
        .stdin
        .take()
        .expect("awk stdin is piped")
        .write_all(input)
        .expect("write awk input");
    child.wait_with_output().expect("wait for awk applet")
}

fn assert_output(output: Output, status: i32, stdout: &[u8]) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_eq!(output.stdout, stdout, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

struct OwnedChild(Option<Child>);

impl OwnedChild {
    fn spawn(scratch: &Scratch, program: &str, locale: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", "awk", "--", program])
            .current_dir(scratch.path())
            .env("LC_ALL", locale)
            .env("LANG", locale)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn owned awk process");
        Self(Some(child))
    }

    fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("owned child has not been collected")
    }

    fn stdin(&mut self) -> ChildStdin {
        self.child()
            .stdin
            .take()
            .expect("owned child stdin is piped")
    }

    fn stop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn output_bounded(mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self
            .child()
            .try_wait()
            .expect("inspect owned awk exit")
            .is_none()
        {
            assert!(
                Instant::now() < deadline,
                "awk blocked after its record/control result was determined"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        self.0
            .take()
            .expect("owned child is present")
            .wait_with_output()
            .expect("collect owned awk output")
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn operand_assignments_keep_their_positions_and_argv_entries() {
    let scratch = Scratch::new();
    scratch.file("a", b"A\n");
    scratch.file("b", b"B\n");
    assert_output(
        run(
            &scratch,
            &[
                "-v",
                "x=pre",
                r#"BEGIN {print "begin",x,ARGC; for(i=1;i<ARGC;i++) print i,ARGV[i]} {print FILENAME,x,FNR,$0} END {print "end",x,NR}"#,
                "x=one",
                "a",
                "x=two",
                "b",
                "x=last",
            ],
            b"",
        ),
        0,
        b"begin pre 6\n1 x=one\n2 a\n3 x=two\n4 b\n5 x=last\na one 1 A\nb two 1 B\nend last 2\n",
    );
}

#[test]
fn begin_and_record_argv_edits_control_which_files_are_opened() {
    let scratch = Scratch::new();
    scratch.file("a", b"A\n");
    scratch.file("b", b"B\n");
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {ARGV[1]="a"; delete ARGV[2]; ARGV[3]="x=between"; ARGV[4]="missing"; ARGC=5} {print FILENAME,x,FNR,$0; if(NR==1) ARGV[4]="b"} END {print "end",NR}"#,
                "missing-one",
                "missing-two",
            ],
            b"",
        ),
        0,
        b"a  1 A\nb between 1 B\nend 2\n",
    );
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {delete ARGV[1]; ARGV[2]=""} {print FILENAME,$0}"#,
                "missing-one",
                "missing-two",
            ],
            b"stdin\n",
        ),
        0,
        b"- stdin\n",
    );
}

#[test]
fn extreme_argc_and_field_indices_terminate_with_gawk_statuses() {
    let scratch = Scratch::new();
    let mut child = OwnedChild::spawn(&scratch, "BEGIN { ARGC=1e300 } { print }", "C");
    child
        .stdin()
        .write_all(b"stdin\n")
        .expect("write huge ARGC input");
    assert_output(child.output_bounded(), 0, b"stdin\n");

    for program in [
        "BEGIN { NF=-1 }",
        "BEGIN { NF=1e300 }",
        "BEGIN { $(-1)=\"x\" }",
        "BEGIN { $1e300=\"x\" }",
    ] {
        let child = OwnedChild::spawn(&scratch, program, "C");
        let output = child.output_bounded();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(!output.stderr.is_empty(), "{output:?}");
    }

    let child = OwnedChild::spawn(&scratch, "BEGIN { $1000000=\"x\"; print NF }", "C");
    assert_output(child.output_bounded(), 0, b"1000000\n");
}

#[test]
fn assignments_update_special_variables_when_their_operand_is_reached() {
    let scratch = Scratch::new();
    scratch.file("a", b"one:two\n");
    scratch.file("b", b"three:four\n");
    assert_output(
        run(
            &scratch,
            &[
                "-v",
                "FS=:",
                r#"{print NF,$2} END {print NR}"#,
                "a",
                "NR=10",
                "b",
            ],
            b"",
        ),
        0,
        b"2 two\n2 four\n11\n",
    );
}

#[test]
fn repeated_program_files_are_joined_with_a_source_boundary() {
    let scratch = Scratch::new();
    scratch.file("one.awk", b"BEGIN {print \"one\"} # no final newline");
    scratch.file("two.awk", b"BEGIN {print \"two\"}\n");
    assert_output(
        run(&scratch, &["-f", "one.awk", "-ftwo.awk"], b""),
        0,
        b"one\ntwo\n",
    );
}

#[test]
fn begin_exit_skips_operand_opening_and_still_runs_end() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {print "begin"; exit 7; print "unreachable"} END {print "end"}"#,
                "missing",
            ],
            b"",
        ),
        7,
        b"begin\nend\n",
    );
    let failure = run(
        &scratch,
        &[r#"BEGIN {print "begin"} {print}"#, "missing"],
        b"",
    );
    assert_eq!(failure.status.code(), Some(2), "{failure:?}");
    assert_eq!(failure.stdout, b"begin\n", "{failure:?}");
    assert!(!failure.stderr.is_empty(), "{failure:?}");
}

#[test]
fn exit_unwinds_blocks_and_end_controls_final_status() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[r#"{print; exit 3; print "unreachable"} END {print "end"}"#],
            b"first\nsecond\n",
        ),
        3,
        b"first\nend\n",
    );
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {for(i=0;i<3;i++) {if(i==1) exit 7; print i}} END {print "end"; exit} END {print "unreachable"}"#,
            ],
            b"",
        ),
        7,
        b"0\nend\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {exit 7} END {print "end"; exit 5}"#],
            b"",
        ),
        5,
        b"end\n",
    );
}

#[test]
fn single_byte_record_separators_and_changes_take_effect_on_the_next_read() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS=":"} {print NR,$0}"#],
            b"one:two:three",
        ),
        0,
        b"1 one\n2 two\n3 three\n",
    );
    assert_output(
        run(&scratch, &[r#"{print; RS=":"}"#], b"first\nsecond:third"),
        0,
        b"first\nsecond\nthird\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS="\0"} {print NR,$0}"#],
            b"one\0two\0",
        ),
        0,
        b"1 one\n2 two\n",
    );
}

#[test]
fn regex_record_separators_are_longest_and_cross_buffer_boundaries() {
    let scratch = Scratch::new();
    let mut input = vec![b'x'; 8191];
    input.extend_from_slice(b":::tail::");
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS=":+"} {print NR,length($0)}"#],
            &input,
        ),
        0,
        b"1 8191\n2 4\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS="a.*z|:"} {print NR,"[" $0 "]"}"#],
            b"prea:middlez:tail",
        ),
        0,
        b"1 [pre]\n2 []\n3 [tail]\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS="x*"} {print "[" $0 "]"}"#],
            b"abxxcd",
        ),
        0,
        b"[ab]\n[cd]\n",
    );
}

#[test]
fn paragraph_mode_only_treats_empty_lines_as_boundaries() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS=""} {print NR,length($0),NF}"#],
            b"\n\none\n two\n\n \n\nlast\n\n",
        ),
        0,
        b"1 8 2\n2 1 0\n3 4 1\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS=""; FS=":"} {print NF,$1,$2,$3,$4}"#],
            b"a:b\nc:d\n\n",
        ),
        0,
        b"4 a b c d\n",
    );
}

#[test]
fn crlf_records_preserve_the_carriage_return() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &["{print length($0),length($1),$0}"],
            b"one\r\ntwo\r\n",
        ),
        0,
        b"4 4 one\r\n4 4 two\r\n",
    );
}

#[test]
fn records_fields_substrings_printf_and_array_keys_preserve_arbitrary_bytes() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[r#"{printf "%d:%s:%s\n",length($0),substr($0,2,1),$0; a[$0]++; print a[$0]}"#],
            b"a\xffb\0c\n",
        ),
        0,
        b"5:\xff:a\xffb\0c\n1\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {printf "%s|%c|%s","\377",255,"\xff"}"#],
            b"",
        ),
        0,
        b"\xff|\xff|\xff",
    );
    assert_output(
        run(
            &scratch,
            &[
                "-v",
                r"pattern=\377",
                "{match($0,pattern); print RSTART,RLENGTH; sub(pattern,\"X\"); print}",
            ],
            b"a\xffb\n",
        ),
        0,
        b"2 1\naXb\n",
    );
}

#[test]
fn unix_filenames_program_files_and_assignment_values_need_not_be_utf8() {
    let scratch = Scratch::new();
    let filename = OsString::from_vec(b"input-\xff".to_vec());
    let script = OsString::from_vec(b"program-\xfe.awk".to_vec());
    scratch.file(Path::new(&filename), b"content\n");
    scratch.file(Path::new(&script), b"{print FILENAME,x,$0}\n");
    let args = vec![
        OsString::from("-f"),
        script,
        OsString::from_vec(b"x=raw-\xfd".to_vec()),
        filename,
    ];
    assert_output(
        run(&scratch, &args, b""),
        0,
        b"input-\xff raw-\xfd content\n",
    );
}

#[test]
fn getline_uses_the_same_record_reader_and_operand_cursor() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {RS=":"; getline first; print "begin",NR,FNR,first} {print NR,FNR,$0} END {print "end",NR}"#,
            ],
            b"a:b:c",
        ),
        0,
        b"begin 1 1 a\n2 2 b\n3 3 c\nend 3\n",
    );
    scratch.file("a", b"A\n");
    scratch.file("b", b"B\n");
    assert_output(
        run(
            &scratch,
            &[
                "{getline second; print $0,second,FILENAME,NR,FNR}",
                "a",
                "b",
            ],
            b"",
        ),
        0,
        b"A B b 2 1\n",
    );
    scratch.file("records", b"one:two:three");
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS=":"; while((getline value < "records")>0) print value}"#],
            b"",
        ),
        0,
        b"one\ntwo\nthree\n",
    );
}

#[test]
fn regex_search_substitution_and_splitting_use_leftmost_longest_matches() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[
                r#"{match($0,/a|aa/); print RSTART,RLENGTH; value=$0; print gsub(/a|aa/,"X",value),value; print gensub(/(a|aa)/,"<\\1>","g")}"#,
            ],
            b"aa\n",
        ),
        0,
        b"1 2\n1 X\n<aa>\n",
    );
    assert_output(
        run(
            &scratch,
            &["{match($0,/b|aaa/); print RSTART,RLENGTH}"],
            b"baaa\n",
        ),
        0,
        b"1 1\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {print split("aaX",a,/a|aa/),a[1],a[2]}"#],
            b"",
        ),
        0,
        b"2  X\n",
    );
}

#[test]
fn gensub_accepts_regex_literals_without_changing_boolean_regex_expressions() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[
                r#"{print gensub(/([a-z]+):([0-9]+)/,"\\2-\\1","g"); print gensub("([a-z]+):([0-9]+)","\\2-\\1",1); print (/name/)}"#,
            ],
            b"name:42\n",
        ),
        0,
        b"42-name\n42-name\n1\n",
    );
}

#[test]
fn strtonum_converts_decimal_hexadecimal_and_octal_inputs() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {print strtonum("0x20"),strtonum("010"),strtonum("018"),strtonum("-0x10"),strtonum("12.5e1"),strtonum("junk"),strtonum("-010"),strtonum(" 010"); printf "%.9f\n",strtonum(1.23456789)}"#,
            ],
            b"",
        ),
        0,
        b"32 8 18 0 125 0 -10 10\n1.234567890\n",
    );
}

#[test]
fn utf8_locale_keeps_character_operations_and_regexes_character_aware() {
    let scratch = Scratch::new();
    assert_output(
        run_locale(
            &scratch,
            &[
                r#"{print length($0),substr($0,2,1); printf "%.1s|%c\n",$0,233; match($0,/é|éé/); print RSTART,RLENGTH; print gensub(/./,"X",1)}"#,
            ],
            "éé\n".as_bytes(),
            "C.UTF-8",
        ),
        0,
        "2 é\né|é\n1 2\nXé\n".as_bytes(),
    );
}

#[test]
fn begin_can_create_an_operand_before_the_runtime_opens_it() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {print "created" > "late"; close("late")} {print FILENAME,$0}"#,
                "late",
            ],
            b"",
        ),
        0,
        b"late created\n",
    );
}

#[test]
fn regex_anchors_mean_record_boundaries_and_unknown_letter_escapes_stay_literal() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {RS=":"} {match($0,/$/); print RSTART,RLENGTH; print gsub(/a$/,"X"),$0}"#],
            b"a\n:",
        ),
        0,
        b"3 0\n0 a\n\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {print ("AzZ" ~ /\A\z\Z/), ("d" ~ /\d/), ("x\n" ~ /[^^]$/)}"#],
            b"",
        ),
        0,
        b"1 1 1\n",
    );
}

#[test]
fn utf8_regexes_skip_invalid_bytes_without_losing_them_or_inventing_anchors() {
    let scratch = Scratch::new();
    assert_output(
        run_locale(
            &scratch,
            &[
                r#"BEGIN {FS=""} {print NF,length($0); value=$0; gsub(/./,"X",value); print value; print ($0 ~ /^b/),($0 ~ /é$/),($0 ~ /b$/)}"#,
            ],
            b"\xc3\xa9\xffb\n",
            "C.UTF-8",
        ),
        0,
        b"3 3\nX\xffX\n0 0 1\n",
    );
    assert_output(
        run_locale(
            &scratch,
            &[r#"BEGIN {RS="é+"} {print NR,$0}"#],
            "aéébé".as_bytes(),
            "C.UTF-8",
        ),
        0,
        b"1 a\n2 b\n",
    );
}

#[test]
fn field_separator_options_preserve_order_and_assignment_escapes() {
    let scratch = Scratch::new();
    assert_output(
        run(&scratch, &["-v", "FS=:", "-F,", "{print NF,$2}"], b"a,b\n"),
        0,
        b"2 b\n",
    );
    assert_output(
        run(&scratch, &["-F,", "-v", "FS=:", "{print NF,$2}"], b"a:b\n"),
        0,
        b"2 b\n",
    );
    assert_output(
        run(&scratch, &[r"-F\t", "{print NF,$2}"], b"a\tb\n"),
        0,
        b"2 b\n",
    );
}

#[test]
fn strict_modes_use_only_the_first_record_separator_character() {
    let scratch = Scratch::new();
    for mode in ["--posix", "--traditional"] {
        assert_output(
            run(
                &scratch,
                &[mode, r#"BEGIN {RS="::"} {print NR,$0}"#],
                b"a:b::c",
            ),
            0,
            b"1 a\n2 b\n3 \n4 c\n",
        );
    }
}

#[test]
fn regex_separator_stops_reading_when_its_longest_candidate_is_disproved() {
    let scratch = Scratch::new();
    let mut child = OwnedChild::spawn(
        &scratch,
        r#"BEGIN {RS="a|ab"} {print "[" $0 "]"; exit}"#,
        "C",
    );
    let mut input = child.stdin();
    input
        .write_all(b"aX")
        .expect("write a completed separator and disproof byte");
    let output = child.output_bounded();
    drop(input);
    assert_output(output, 0, b"[]\n");
}

#[test]
fn unicode_record_boundary_assertions_wait_for_complete_refilled_characters() {
    let scratch = Scratch::new();
    let mut input = vec![b' '; 8190];
    input.extend_from_slice("xé".as_bytes());
    let mut expected = b"[".to_vec();
    expected.extend_from_slice(&input);
    expected.extend_from_slice(b"]\n");
    assert_output(
        run_locale(
            &scratch,
            &[r#"BEGIN {RS="x\\y"} {print "[" $0 "]"}"#],
            &input,
            "C.UTF-8",
        ),
        0,
        &expected,
    );
}

#[test]
fn function_exit_unwinds_pending_assignments_and_record_patterns() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[r#"function stop(){exit 7} BEGIN {x=0; y=0; x=stop()+(y=1)} END {print x,y}"#],
            b"",
        ),
        7,
        b"0 0\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"function stop(){exit 3} {x=7; x=stop()+1} END {print x}"#],
            b"record\n",
        ),
        3,
        b"7\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"function stop(){exit 3} stop() {print "unreachable"} END {print "end"}"#],
            b"record\n",
        ),
        3,
        b"end\n",
    );
}

#[test]
fn function_exit_prevents_a_later_getline_from_opening_an_owned_fifo() {
    let scratch = Scratch::new();
    let metadata = fs::symlink_metadata(scratch.path()).expect("inspect owned scratch directory");
    assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
    // SAFETY: geteuid takes no arguments and has no side effects.
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    let path = scratch.path().join("blocked");
    let c_path = CString::new(path.as_os_str().as_bytes()).expect("owned FIFO path has no NUL");
    // SAFETY: c_path names a new entry beneath the verified owned directory.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert!(
        fs::symlink_metadata(&path)
            .expect("inspect owned FIFO")
            .file_type()
            .is_fifo()
    );
    let child = OwnedChild::spawn(
        &scratch,
        r#"function stop(){exit 7} BEGIN {x=5; x=stop()+(getline value < "blocked")} END {print x}"#,
        "C",
    );
    assert_output(child.output_bounded(), 7, b"5\n");
}

#[cfg(target_os = "linux")]
fn resident_bytes(child: &mut OwnedChild) -> u64 {
    assert!(
        child
            .child()
            .try_wait()
            .expect("inspect owned awk process")
            .is_none()
    );
    let pid = child.child().id();
    let status =
        fs::read_to_string(format!("/proc/{pid}/status")).expect("read owned awk process status");
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .expect("process status field is present")
    };
    assert_eq!(field("Pid:"), u64::from(pid));
    field("VmRSS:") * 1024
}

#[cfg(target_os = "linux")]
fn feed_blank_run(
    child: &mut OwnedChild,
    mut input: ChildStdin,
    prefix: &'static [u8],
) -> ChildStdin {
    let (sender, receiver) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = (|| -> std::io::Result<ChildStdin> {
            input.write_all(prefix)?;
            let chunk = [b'\n'; 64 * 1024];
            for _ in 0..1024 {
                input.write_all(&chunk)?;
            }
            Ok(input)
        })();
        let _ = sender.send(result);
    });
    let result = match receiver.recv_timeout(Duration::from_secs(20)) {
        Ok(result) => result,
        Err(error) => {
            child.stop();
            let _ = writer.join();
            panic!("awk stopped consuming a blank paragraph delimiter run: {error}");
        }
    };
    writer.join().expect("join blank input writer");
    result.expect("write blank paragraph input")
}

#[cfg(target_os = "linux")]
#[test]
fn paragraph_delimiter_runs_keep_bounded_memory_and_do_not_wait_for_another_record() {
    for trailing in [false, true] {
        let scratch = Scratch::new();
        let program = if trailing {
            r#"BEGIN {RS=""; print "ready" > "ready"; close("ready")} {print; if(NR==2) exit}"#
        } else {
            r#"BEGIN {RS=""; print "ready" > "ready"; close("ready")} {print; exit}"#
        };
        let mut child = OwnedChild::spawn(&scratch, program, "C");
        let deadline = Instant::now() + Duration::from_secs(10);
        while fs::read(scratch.path().join("ready")).ok().as_deref() != Some(b"ready\n".as_slice())
        {
            assert!(
                child
                    .child()
                    .try_wait()
                    .expect("inspect paragraph process")
                    .is_none()
            );
            assert!(
                Instant::now() < deadline,
                "paragraph process did not execute BEGIN"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let baseline = resident_bytes(&mut child);
        let input = child.stdin();
        let mut input = feed_blank_run(&mut child, input, if trailing { b"x\n\n" } else { b"" });
        let resident = resident_bytes(&mut child);
        input
            .write_all(if trailing { b"y\n\n" } else { b"x\n\n" })
            .expect("finish a paragraph without closing stdin");
        let output = child.output_bounded();
        drop(input);
        assert_output(output, 0, if trailing { b"x\ny\n" } else { b"x\n" });
        assert!(
            resident.saturating_sub(baseline) < 16 * 1024 * 1024,
            "blank delimiter memory scaled with the 64 MiB run: baseline={baseline}, resident={resident}"
        );
    }
}

#[test]
fn escaped_utf8_pattern_bytes_preserve_character_matching_and_literal_metacharacters() {
    let scratch = Scratch::new();
    let output = run_locale(
        &scratch,
        &[
            r#"BEGIN {print gensub(/é.|\xC3\xA9/,"X",1,"éé"); print gensub(/é.|\303\251/,"X",1,"éé"); pattern="é.|\\xC3\\xA9"; print gensub(pattern,"X",1,"éé"); match("*",/\052/); print RSTART,RLENGTH; match("[",/\x5B/); print RSTART,RLENGTH}"#,
        ],
        b"",
        "C.UTF-8",
    );
    assert!(std::str::from_utf8(&output.stdout).is_ok(), "{output:?}");
    assert_output(output, 0, b"X\nX\nX\n1 1\n1 1\n");
    assert_output(
        run_locale(
            &scratch,
            &[r#"BEGIN {RS="\\xC3\\xA9+"} {print NR,$0}"#],
            "aééb".as_bytes(),
            "C.UTF-8",
        ),
        0,
        b"1 a\n2 b\n",
    );
}

#[test]
fn fpat_and_patsplit_do_not_add_an_empty_field_after_a_nonempty_match() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {print patsplit("a",a,/a*/),a[1]; print patsplit("a,b",a,/[^,]*/,seps),a[1],a[2],"[" seps[1] "]"}"#,
            ],
            b"",
        ),
        0,
        b"1 a\n2 a b [,]\n",
    );
    assert_output(
        run(
            &scratch,
            &[r#"BEGIN {FPAT="[^,]*"} {print NF; for(i=1;i<=NF;i++) print "[" $i "]"}"#],
            b"a,b\n",
        ),
        0,
        b"2\n[a]\n[b]\n",
    );
}

#[test]
fn strtonum_keeps_numeric_strings_decimal_but_interprets_pure_string_literals() {
    let scratch = Scratch::new();
    assert_output(
        run(
            &scratch,
            &[r#"{print strtonum($1),strtonum("010")}"#],
            b"010\n",
        ),
        0,
        b"10 8\n",
    );
    assert_output(
        run(
            &scratch,
            &[
                "-v",
                "value=010",
                r#"BEGIN {print strtonum(value),strtonum("010")}"#,
            ],
            b"",
        ),
        0,
        b"10 8\n",
    );
}

#[test]
fn exit_raised_during_getline_file_lifecycle_also_aborts_its_expression() {
    let scratch = Scratch::new();
    scratch.file("input", b"unread\n");
    assert_output(
        run(
            &scratch,
            &[
                r#"BEGIN {x=5; x=(getline value)+1} BEGINFILE {exit 7} END {print x}"#,
                "input",
            ],
            b"",
        ),
        7,
        b"5\n",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn stdout_write_and_flush_failures_are_distinct_from_input_read_errors() {
    for program in [
        r#"BEGIN {print "payload"}"#,
        r#"BEGIN {printf "%s", "payload"}"#,
    ] {
        let full = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("open output failure device");
        let metadata = full.metadata().unwrap();
        assert!(metadata.file_type().is_char_device());
        assert_eq!(
            (libc::major(metadata.rdev()), libc::minor(metadata.rdev())),
            (1, 7)
        );
        let output = Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", "awk", "--", program])
            .env("PATH", "/nonexistent")
            .env("LC_ALL", "C")
            .env("AXE_STORE_MODE", "off")
            .stdin(Stdio::null())
            .stdout(full)
            .output()
            .expect("run awk with failing stdout");
        assert_eq!(output.status.code(), Some(1), "{program}: {output:?}");
        assert!(!output.stderr.is_empty(), "{program}: {output:?}");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "awk", "--", "{print}", "/proc/self/mem"])
        .env("PATH", "/nonexistent")
        .env("LC_ALL", "C")
        .env("AXE_STORE_MODE", "off")
        .stdin(Stdio::null())
        .output()
        .expect("run awk with an unreadable input address");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}
