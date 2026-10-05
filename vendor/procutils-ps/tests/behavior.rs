use std::process::{Command, Output};

fn ps(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ps"))
        .args(args)
        .output()
        .expect("run ps")
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("ps output is UTF-8")
}

#[test]
fn empty_native_full_and_custom_selections_print_headers_and_exit_one() {
    // Linux pid_max is smaller than i32::MAX, so this PID cannot exist.
    let cases: &[(&[&str], &[&str])] = &[
        (&["-p", "2147483647"], &["PID", "TTY", "TIME", "CMD"]),
        (
            &["-p", "2147483647", "-f"],
            &["UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD"],
        ),
        (&["-p", "2147483647", "-o", "pid,comm"], &["PID", "COMMAND"]),
    ];
    for &(args, expected_headers) in cases {
        let output = ps(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}: {:?}", output.stderr);
        assert_eq!(stdout(&output).lines().count(), 1, "{args:?}");
        assert_eq!(
            stdout(&output).split_whitespace().collect::<Vec<_>>(),
            expected_headers,
            "{args:?}"
        );
    }
}

#[test]
fn only_all_empty_custom_headers_suppress_the_header_row() {
    let pid = std::process::id().to_string();
    let all_empty = ps(&["-p", &pid, "-o", "pid=,comm="]);
    assert!(all_empty.status.success());
    assert_eq!(stdout(&all_empty).lines().count(), 1);
    assert_eq!(
        stdout(&all_empty).split_whitespace().next(),
        Some(pid.as_str())
    );

    let mixed = ps(&["-p", &pid, "-o", "pid=,comm"]);
    assert!(mixed.status.success());
    let mut lines = stdout(&mixed).lines();
    assert_eq!(lines.next().unwrap().trim(), "COMMAND");
    assert_eq!(
        lines.next().unwrap().split_whitespace().next(),
        Some(pid.as_str())
    );
    assert!(lines.next().is_none());

    let no_rows = ps(&["-p", "2147483647", "-o", "pid=,comm="]);
    assert_eq!(no_rows.status.code(), Some(1));
    assert!(no_rows.stdout.is_empty());
}

#[test]
fn invalid_user_is_a_diagnostic_failure_not_an_empty_success() {
    let name = format!("__axe_ps_nonexistent_user_{}__", std::process::id());
    for option in ["-U", "-u", "--user"] {
        let output = ps(&[option, &name]);
        assert!(!output.status.success(), "{option}");
        assert!(output.stdout.is_empty(), "{option}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&name),
            "{option}: {:?}",
            output.stderr
        );
    }
}

#[test]
fn full_format_has_ordered_columns_and_hh_mm_ss_cpu_time() {
    let pid = std::process::id().to_string();
    let output = ps(&["-p", &pid, "-f"]);
    assert!(output.status.success());
    let mut lines = stdout(&output).lines();
    assert_eq!(
        lines.next().unwrap().split_whitespace().collect::<Vec<_>>(),
        ["UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD"]
    );
    let row = lines.next().unwrap().split_whitespace().collect::<Vec<_>>();
    assert_eq!(row[1], pid);
    let time = row[6].split(':').collect::<Vec<_>>();
    assert_eq!(time.len(), 3);
    assert!(
        time.iter()
            .all(|part| part.len() >= 2 && part.bytes().all(|byte| byte.is_ascii_digit()))
    );
    assert!(lines.next().is_none());
}
