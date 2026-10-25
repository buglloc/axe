#![cfg(target_os = "linux")]

use std::process::{Command, Output};

fn ps(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "ps", "--"])
        .args(args)
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_MODE", "off")
        .env("LC_ALL", "C")
        .output()
        .expect("run ps")
}

#[test]
fn identifier_columns_keep_procps_width_with_empty_headers_and_small_values() {
    let pid = std::process::id().to_string();
    let pid_max: u32 = std::fs::read_to_string("/proc/sys/kernel/pid_max")
        .expect("read pid limit")
        .trim()
        .parse()
        .expect("numeric pid limit");
    let width = pid_max.saturating_sub(1).to_string().len().max(5);
    let output = ps(&["-p", &pid, "-o", "pid=,ppid=,nlwp=,comm="]);
    assert!(output.status.success(), "{:?}", output.stderr);
    let row = std::str::from_utf8(&output.stdout).expect("ps text output");
    assert!(row.starts_with(&format!("{pid:>width$} ")), "{row:?}");
    let fields: Vec<_> = row.split_whitespace().collect();
    assert_eq!(fields[0], pid);
    let nlwp = fields[2];
    let prefix_length = 2 * width + 2;
    assert!(
        row[prefix_length..].starts_with(&format!("{nlwp:>4} ")),
        "{row:?}"
    );

    let empty = ps(&["-p", "2147483647", "-o", "pid,comm"]);
    assert_eq!(empty.status.code(), Some(1));
    assert_eq!(
        empty.stdout,
        format!("{:>width$} COMMAND\n", "PID").as_bytes()
    );
}

#[test]
fn unknown_output_field_is_a_usage_failure_not_an_empty_selection() {
    let output = ps(&["-o", "axe_unknown_field"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("axe_unknown_field"));
}
