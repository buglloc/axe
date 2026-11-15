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
            &[
                r#"{ print "<"$0">", length(RT); if (NR==1) { getline x; print "<"x">", length(RT) } }"#,
            ],
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
            &[
                r#"BEGIN { RS="a+"; while ((getline x < "/dev/stdin") > 0) print NR, "<"x">", "<"RT">" }"#,
            ],
            b"aaXaaaY",
            "C",
        ),
        0,
        b"0 <> <aa>\n0 <X> <aaa>\n0 <Y> <>\n",
    );
}

#[test]
fn awk_arithmetic_and_fields_recognize_signed_infinity() {
    assert_output(
        run(
            "awk",
            &[r#"{ print ($0<2), ($1<2), ($0+0), ($1+0) }"#],
            b"+inf\n-InF\ninf\n",
            "C",
        ),
        0,
        b"0 0 +inf +inf\n1 1 -inf -inf\n0 0 0 0\n",
    );
    assert_output(
        run(
            "awk",
            &[
                r#"BEGIN { print ("+inf"+0), sprintf("%.6g","+inf"), strtonum("+inf"); print sprintf("%.6g","-0"); print ("+NaN"+0), ("-NaN"+0) }"#,
            ],
            b"",
            "C",
        ),
        0,
        b"+inf +inf 0\n-0\n+nan -nan\n",
    );
}

#[test]
fn grep_c_locale_classes_and_words_are_byte_oriented() {
    assert_output(
        run(
            "grep",
            &["-a", "-o", "[[:alpha:]]"],
            "éa\n猫a\ne\u{301}\n".as_bytes(),
            "C",
        ),
        0,
        b"a\na\ne\n",
    );
    assert_output(
        run("grep", &["-a", "-o", r"\<a"], b"\xffa\xfe\n", "C"),
        0,
        b"a\n",
    );
    assert_output(
        run(
            "grep",
            &["-a", "-w", "-o", "a"],
            "éa猫 a_ a\n".as_bytes(),
            "C",
        ),
        0,
        b"a\na\n",
    );
    assert_output(
        run(
            "grep",
            &["-a", "-o", "[[:alpha:]]"],
            "é猫a\n".as_bytes(),
            "C.UTF-8",
        ),
        0,
        "é\n猫\na\n".as_bytes(),
    );
    assert_output(
        run(
            "grep",
            &["-a", "-w", "-o", "a"],
            "éa猫 a_ a\n".as_bytes(),
            "C.UTF-8",
        ),
        0,
        b"a\n",
    );
}

#[test]
fn grep_c_locale_does_not_treat_high_bytes_as_binary() {
    for args in [&[""][..], &["a"][..]] {
        assert_output(run("grep", args, b"\xffa\xfe\n", "C"), 0, b"\xffa\xfe\n");
    }
    let output = run("grep", &[""], b"\xffa\xfe\n", "C.UTF-8");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(!output.stderr.is_empty(), "{output:?}");
}

#[test]
fn printf_unicode_escapes_use_the_output_codeset() {
    assert_output(
        run("printf", &["%b", r"\u732b\U0001f600\u0061"], b"", "C"),
        0,
        b"\\u732B\\U0001F600a",
    );
    assert_output(
        run("printf", &[r"\u732b\U0001f600\u0061"], b"", "C"),
        0,
        b"\\u732B\\U0001F600a",
    );
    assert_output(
        run("printf", &["%b", r"\u732b\U0001f600\u0061"], b"", "C.UTF-8"),
        0,
        "猫😀a".as_bytes(),
    );
    let invalid = run("printf", &["%b", r"\uD800"], b"", "C");
    assert_eq!(invalid.status.code(), Some(1), "{invalid:?}");
    assert!(invalid.stdout.is_empty(), "{invalid:?}");
    assert!(!invalid.stderr.is_empty(), "{invalid:?}");
}

#[test]
fn sed_empty_matches_preserve_utf8_character_boundaries() {
    assert_output(
        run("sed", &["s/x*/X/g"], "é猫\n".as_bytes(), "C.UTF-8"),
        0,
        "XéX猫X\n".as_bytes(),
    );
    assert_output(
        run("sed", &["s/x*/X/g"], "é\n".as_bytes(), "C"),
        0,
        b"X\xc3X\xa9X\n",
    );
}

#[test]
fn grep_utf8_regexes_do_not_consume_invalid_characters() {
    assert_output(
        run("grep", &["-a", "-o", "."], b"\xffa\xfe\n", "C.UTF-8"),
        0,
        b"a\n",
    );
    assert_output(
        run("grep", &["-a", "-o", "[^a]"], b"\xffa\xfe\n", "C.UTF-8"),
        1,
        b"",
    );
    assert_output(
        run(
            "grep",
            &["-a", "-o", r"^a\|a$\|\`a\|a\'"],
            b"\xffa\xfe\n",
            "C.UTF-8",
        ),
        1,
        b"",
    );
    assert_output(
        run("grep", &["-a", "-w", "-o", "a"], b"\xffa\xfe\n", "C.UTF-8"),
        0,
        b"a\n",
    );
}

#[test]
fn grep_only_matching_does_not_emit_unmatched_invalid_bytes() {
    for pattern in [".", "a*", r"a\|ab", "[[:alpha:]]"] {
        assert_output(
            run("grep", &["-o", pattern], b"\xffa\xfe\n", "C.UTF-8"),
            0,
            b"a\n",
        );
    }
}

#[test]
fn grep_binary_nul_boundaries_affect_counts_and_anchors() {
    for locale in ["C", "C.UTF-8"] {
        assert_output(run("grep", &["-c", ""], b"a\0b\0\0a", locale), 0, b"4\n");
        assert_output(run("grep", &["-c", "a"], b"a\0b\0\0a", locale), 0, b"2\n");
        assert_output(
            run("grep", &["-c", "a$"], b"a\0b\na\xffb\n", locale),
            0,
            b"1\n",
        );
        assert_output(
            run("grep", &["-a", "-c", ""], b"a\0b\0\0a", locale),
            0,
            b"1\n",
        );
        assert_output(
            run("grep", &["-z", "-c", ""], b"a\0b\0\0a", locale),
            0,
            b"4\n",
        );
        let inverted = run("grep", &["-v", "."], b"a\0b\0\0a", locale);
        assert_eq!(inverted.status.code(), Some(0), "{inverted:?}");
        assert!(inverted.stdout.is_empty(), "{inverted:?}");
        assert!(!inverted.stderr.is_empty(), "{inverted:?}");
    }
}

#[test]
fn printf_rejects_malformed_argument_escapes_without_losing_printed_prefixes() {
    for locale in ["C", "C.UTF-8"] {
        for escape in [r"\x", r"\uZZZZ", r"\u123", r"\uD800", r"\U0000DFFF"] {
            let operand = format!("before{escape}");
            let argument = run("printf", &["%bafter", &operand], b"", locale);
            assert_eq!(argument.status.code(), Some(1), "{argument:?}");
            assert_eq!(argument.stdout, b"before", "{argument:?}");
            assert!(!argument.stderr.is_empty(), "{argument:?}");

            let format = format!("{operand}|after");
            let literal = run("printf", &[&format], b"", locale);
            assert_eq!(literal.status.code(), Some(1), "{literal:?}");
            assert_eq!(literal.stdout, b"before", "{literal:?}");
            assert!(!literal.stderr.is_empty(), "{literal:?}");
        }
        assert_output(
            run("printf", &["%bafter", r"before\c\uD800"], b"", locale),
            0,
            b"before",
        );
        assert_output(run("printf", &["%b", r"\q"], b"", locale), 0, br"\q");
    }
}

#[test]
fn echo_keeps_permissive_escapes_while_printf_rejects_them() {
    for locale in ["C", "C.UTF-8"] {
        assert_output(
            run("echo", &["-e", r"\x|\xZ|\uD800|\U0000DFFF"], b"", locale),
            0,
            b"\\x|\\xZ|\\uD800|\\U0000DFFF\n",
        );
        let strict = run("printf", &["%b", r"\x"], b"", locale);
        assert_eq!(strict.status.code(), Some(1), "{strict:?}");
        assert!(strict.stdout.is_empty(), "{strict:?}");
        assert!(!strict.stderr.is_empty(), "{strict:?}");
    }
}

#[test]
fn grep_invalid_utf8_does_not_create_a_regex_word_start() {
    assert_output(
        run("grep", &["-a", "-o", r"\<a"], b"\xffa\xfe\n", "C.UTF-8"),
        1,
        b"",
    );
    assert_output(
        run("grep", &["-c", r"\<a"], b"\xffa\xfe\n", "C.UTF-8"),
        1,
        b"0\n",
    );
    assert_output(
        run("grep", &["-q", r"\<a"], b"\xffa\xfe\n", "C.UTF-8"),
        1,
        b"",
    );
    assert_output(
        run("grep", &["-a", "-v", r"\<a"], b"\xffa\xfe\n", "C.UTF-8"),
        0,
        b"\xffa\xfe\n",
    );
}
