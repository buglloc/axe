#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn run(applet: &str, args: &[&str], input: &[u8], locale: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", applet, "--"])
        .args(args)
        .env("PATH", "/nonexistent")
        .env("LC_ALL", locale)
        .env("LANG", locale)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn language applet");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write input");
    child.wait_with_output().expect("collect language applet")
}

fn assert_output(output: Output, status: i32, stdout: &[u8]) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_eq!(output.stdout, stdout, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn awk_power_binds_above_signs_and_below_increments() {
    assert_output(
        run(
            "awk",
            &["BEGIN { x=2; print -2^2, (-2)^2, 2^-2, 2^3^2, -2^-2, ++x^2, x++^2, x }"],
            b"",
            "C",
        ),
        0,
        b"-4 4 0.25 512 -0.25 9 9 4\n",
    );
}

#[test]
fn awk_rt_tracks_regex_string_and_paragraph_terminators() {
    assert_output(
        run(
            "awk",
            &[r#"BEGIN { RS="a+" } { print NR, "<"$0">", "<"RT">" }"#],
            b"aaaXaaY",
            "C",
        ),
        0,
        b"1 <> <aaa>\n2 <X> <aa>\n3 <Y> <>\n",
    );
    assert_output(
        run(
            "awk",
            &[r#"{ print "<"$0">", length(RT); if (NR==1) { getline x; print "<"x">", length(RT) } }"#],
            b"one\ntwo\nthree",
            "C",
        ),
        0,
        b"<one> 1\n<two> 1\n<three> 0\n",
    );
    assert_output(
        run(
            "awk",
            &[r#"BEGIN { RS="" } { print "<"$0">", length(RT) }"#],
            b"\n\none\n\n\n\ntwo\n",
            "C",
        ),
        0,
        b"<one> 4\n<two> 1\n",
    );
}

#[test]
fn awk_redirected_getline_sets_rt_without_changing_main_counters() {
    assert_output(
        run(
            "awk",
            &[r#"BEGIN { RS="a+"; while ((getline x < "/dev/stdin") > 0) print NR, "<"x">", "<"RT">" }"#],
            b"aaXaaaY",
            "C",
        ),
        0,
        b"0 <> <aa>\n0 <X> <aaa>\n0 <Y> <>\n",
    );
}
