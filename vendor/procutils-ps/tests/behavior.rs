use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::fd::FromRawFd,
    process::{Child, Command, Output, Stdio},
};
use unicode_width::UnicodeWidthChar;

fn ps_command(args: &[&str], columns: Option<&str>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ps"));
    command
        .args(args)
        .env_remove("COLUMNS")
        .env_remove("LINES")
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC");
    if let Some(columns) = columns {
        command.env("COLUMNS", columns);
    }
    command
}

fn ps(args: &[&str]) -> Output {
    ps_command(args, None).output().expect("run ps")
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

struct WidthProcess {
    child: Child,
    pid: String,
    command_line: String,
}

impl WidthProcess {
    fn new(payload: &str, name: &str) -> Self {
        let child = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "width_process_fixture",
                "--ignored",
                "--nocapture",
                "--skip",
                payload,
            ])
            .env("AXE_PS_WIDTH_FIXTURE", name)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start managed process");
        let mut process = Self {
            pid: child.id().to_string(),
            child,
            command_line: String::new(),
        };
        let mut output = BufReader::new(process.child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "fixture exited before readiness"
            );
            if line.contains("AXE_PS_WIDTH_READY") {
                break;
            }
        }
        let mut status = 0;
        // SAFETY: this is our unreaped child and status is a valid output
        // pointer. WUNTRACED observes its stop without reaping the process.
        let waited =
            unsafe { libc::waitpid(process.child.id() as i32, &mut status, libc::WUNTRACED) };
        assert_eq!(waited, process.child.id() as i32);
        assert!(libc::WIFSTOPPED(status), "fixture did not freeze");
        process.command_line = procfs::process::Process::new(process.child.id() as i32)
            .unwrap()
            .cmdline()
            .unwrap()
            .join(" ");
        process
    }

    fn args_output(&self, options: &[&str], columns: Option<&str>) -> Output {
        let mut args = vec!["-p", self.pid.as_str(), "-o", "args="];
        args.extend_from_slice(options);
        ps_command(&args, columns).output().expect("run ps")
    }
}

impl Drop for WidthProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "managed process fixture, launched by the output behavior tests"]
fn width_process_fixture() {
    let Ok(name) = std::env::var("AXE_PS_WIDTH_FIXTURE") else {
        return;
    };
    std::fs::write("/proc/self/comm", name).unwrap();
    println!("AXE_PS_WIDTH_READY");
    std::io::stdout().flush().unwrap();
    // SAFETY: SIGSTOP freezes the entire fixture process until its owner kills
    // it, keeping stat/status values stable across output comparisons.
    unsafe { libc::raise(libc::SIGSTOP) };
    loop {
        std::thread::park();
    }
}

fn only_row(output: &Output) -> &str {
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    stdout(output)
        .strip_suffix('\n')
        .expect("one newline-terminated row")
}

fn display_columns(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(1)).sum()
}

#[test]
fn bsd_and_unix_wide_options_clip_commands_and_combine_counts() {
    let payload = format!("WIDTH_PAYLOAD_{}", "x".repeat(400));
    let process = WidthProcess::new(&payload, "width_fixture");
    let cases: &[(&[&str], Option<usize>)] = &[
        (&[], Some(60)),
        (&["w"], Some(132)),
        (&["-w"], Some(132)),
        (&["ww"], None),
        (&["-ww"], None),
        (&["w", "-w"], None),
        (&["-w", "w"], None),
        (&["w", "w"], None),
        (&["-w", "-w"], None),
    ];
    for &(options, width) in cases {
        let output = process.args_output(options, Some("60"));
        let expected = width.map_or(process.command_line.as_str(), |width| {
            &process.command_line[..width]
        });
        assert_eq!(only_row(&output), expected, "{options:?}");
    }
    // Piping remains unlimited when COLUMNS is absent, even with just one w.
    for options in [&[][..], &["w"][..], &["-w"][..]] {
        let output = process.args_output(options, None);
        assert_eq!(only_row(&output), process.command_line, "{options:?}");
    }
}

#[test]
fn width_aliases_use_last_value_then_apply_aggregate_wide_count() {
    let payload = format!("WIDTH_PAYLOAD_{}", "x".repeat(400));
    let process = WidthProcess::new(&payload, "width_fixture");
    let cases: &[(&[&str], Option<usize>)] = &[
        (&["--cols", "50"], Some(50)),
        (&["--columns", "50"], Some(50)),
        (&["--width", "50"], Some(50)),
        (&["--cols=50"], Some(50)),
        (&["--columns=50"], Some(50)),
        (&["--width=50"], Some(50)),
        (&["--width", "+50"], Some(50)),
        (&["--width", "0x32"], Some(50)),
        (&["--width", "062"], Some(50)),
        (&["--width", " 50"], Some(50)),
        (&["--cols", "50", "w"], Some(132)),
        (&["w", "--cols", "50"], Some(132)),
        (&["--width", "50", "-w"], Some(132)),
        (&["-w", "--width", "50"], Some(132)),
        (&["--columns", "50", "ww"], None),
        (&["ww", "--columns", "50"], None),
        (&["--cols", "50", "-ww"], None),
        (&["-ww", "--cols", "50"], None),
        (&["--cols", "200", "w"], Some(200)),
        (&["--width", "50", "--cols", "70"], Some(70)),
        (&["--cols", "70", "--width", "50"], Some(50)),
    ];
    for &(options, width) in cases {
        let output = process.args_output(options, Some("60"));
        let expected = width.map_or(process.command_line.as_str(), |width| {
            &process.command_line[..width]
        });
        assert_eq!(only_row(&output), expected, "{options:?}");
    }
    for value in ["0", "-1", "invalid", "131072"] {
        let output = process.args_output(&[], Some(value));
        assert_eq!(only_row(&output), process.command_line, "COLUMNS={value}");
    }
    for value in ["+50", "0x32", "062"] {
        let output = process.args_output(&[], Some(value));
        assert_eq!(
            only_row(&output),
            &process.command_line[..50],
            "COLUMNS={value}"
        );
    }
}

#[test]
fn invalid_width_values_fail_without_printing_process_rows() {
    for alias in ["--cols", "--columns", "--width"] {
        for value in ["0", "-1", "invalid", "50 ", "2000000000", "ww", "aux"] {
            let output = ps(&[alias, value, "-p", "2147483647"]);
            assert_eq!(output.status.code(), Some(1), "{alias} {value}");
            assert!(output.stdout.is_empty(), "{alias} {value}");
            assert!(!output.stderr.is_empty(), "{alias} {value}");
        }
        let overridden_invalid = ps(&[alias, "invalid", "--width", "50"]);
        assert_eq!(overridden_invalid.status.code(), Some(1));
        assert!(overridden_invalid.stdout.is_empty());
        let output = ps(&[alias]);
        assert!(!output.status.success(), "{alias}");
        assert!(output.stdout.is_empty(), "{alias}");
    }
}

#[test]
fn width_values_do_not_consume_following_bare_bsd_clusters() {
    let payload = format!("WIDTH_PAYLOAD_{}", "x".repeat(400));
    let process = WidthProcess::new(&payload, "width_fixture");
    for alias in ["--cols", "--columns", "--width"] {
        let output = ps_command(&[alias, "50", "auxww", "-p", &process.pid], Some("60"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{alias}: {:?}", output.stderr);
        let mut lines = stdout(&output).lines();
        assert_eq!(
            lines.next().unwrap().split_whitespace().collect::<Vec<_>>(),
            [
                "USER", "PID", "%CPU", "%MEM", "VSZ", "RSS", "TTY", "STAT", "START", "TIME",
                "COMMAND"
            ],
            "{alias}"
        );
        assert!(
            lines.next().unwrap().ends_with(&process.command_line),
            "{alias}"
        );
        assert!(lines.next().is_none(), "{alias}");
    }
}

#[test]
fn fixed_formats_share_width_controls_without_making_unix_w_bsd() {
    let payload = format!("WIDTH_PAYLOAD_{}", "x".repeat(400));
    let process = WidthProcess::new(&payload, "width_fixture");
    let native = ps(&["-p", &process.pid, "-ww"]);
    let mut lines = stdout(&native).lines();
    let header = lines.next().unwrap();
    assert_eq!(
        header.split_whitespace().collect::<Vec<_>>(),
        ["PID", "TTY", "TIME", "CMD"]
    );
    let row = lines.next().unwrap();
    let prefix = row.strip_suffix("width_fixture").unwrap();
    let width = (prefix.len() + 5).to_string();
    for alias in ["--cols", "--columns", "--width"] {
        let output = ps(&["-p", &process.pid, alias, &width]);
        let mut lines = stdout(&output).lines();
        assert!(output.status.success(), "{alias}: {:?}", output.stderr);
        assert_eq!(lines.next().unwrap(), header);
        assert_eq!(lines.next().unwrap(), format!("{prefix}width"));
    }
    for wide in ["-w", "-ww"] {
        let output = ps_command(&["-p", &process.pid, wide], Some("60"))
            .output()
            .unwrap();
        assert_eq!(stdout(&output), stdout(&native), "{wide}");
    }
    let cases: &[(&[&str], usize)] = &[
        (&["-f"], 60),
        (&["-f", "-w"], 132),
        (&["-f", "w"], 132),
        (&["x"], 60),
        (&["x", "w"], 132),
    ];
    for &(options, width) in cases {
        let mut args = vec!["-p", process.pid.as_str()];
        args.extend_from_slice(options);
        let output = ps_command(&args, Some("60")).output().unwrap();
        assert!(output.status.success(), "{options:?}: {:?}", output.stderr);
        assert_eq!(
            display_columns(stdout(&output).lines().nth(1).unwrap()),
            width,
            "{options:?}"
        );
    }
    for options in [&["-f", "-ww"][..], &["-f", "ww"][..], &["auxww"][..]] {
        let mut args = vec!["-p", process.pid.as_str()];
        args.extend_from_slice(options);
        let output = ps_command(&args, Some("60")).output().unwrap();
        assert!(output.status.success(), "{options:?}: {:?}", output.stderr);
        assert!(
            stdout(&output)
                .lines()
                .nth(1)
                .unwrap()
                .ends_with(&process.command_line),
            "{options:?}"
        );
    }
}

#[test]
fn narrow_width_preserves_fixed_numeric_columns_and_headers() {
    let payload = format!("WIDTH_PAYLOAD_{}", "x".repeat(400));
    let process = WidthProcess::new(&payload, "width_fixture");
    let format = "pid=,ppid=,vsz=,pri=,ni=,flags=";
    let unlimited = ps(&["-p", &process.pid, "-o", format, "-ww"]);
    let narrow = ps_command(&["-p", &process.pid, "-o", format], Some("1"))
        .output()
        .unwrap();
    assert!(narrow.status.success());
    assert_eq!(stdout(&narrow), stdout(&unlimited));
    assert!(only_row(&narrow).len() > 1);

    let output = ps_command(&["-p", &process.pid, "-o", "pid=,args=,ppid="], Some("50"))
        .output()
        .unwrap();
    let row = only_row(&output);
    assert_eq!(row.split_whitespace().next(), Some(process.pid.as_str()));
    let parent = std::process::id().to_string();
    assert_eq!(row.split_whitespace().last(), Some(parent.as_str()));
    assert_eq!(display_columns(row), 50);
    assert!(!row.contains(&payload));

    for options in [&["--cols", "1"][..], &["-ww"][..]] {
        let mut args = vec!["-p", "2147483647", "-o", "pid=,comm="];
        args.extend_from_slice(options);
        let empty = ps(&args);
        assert_eq!(empty.status.code(), Some(1), "{options:?}");
        assert!(empty.stdout.is_empty(), "{options:?}");
        let mut args = vec!["-p", "2147483647", "-f"];
        args.extend_from_slice(options);
        let header = ps(&args);
        assert_eq!(header.status.code(), Some(1), "{options:?}");
        assert_eq!(
            stdout(&header).split_whitespace().collect::<Vec<_>>(),
            ["UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD"]
        );
    }
}

#[test]
fn unicode_commands_clip_on_display_columns_without_splitting_utf8() {
    let process = WidthProcess::new("UNICODE:界🙂e\u{301}tail", "width_fixture");
    let marker = process.command_line.find("界🙂").unwrap();
    let prefix = &process.command_line[..marker];
    let prefix_columns = display_columns(prefix);
    for (extra, suffix) in [
        (1, ""),
        (2, "界"),
        (3, "界"),
        (4, "界🙂"),
        (6, "界🙂e\u{301}t"),
    ] {
        let width = (prefix_columns + extra).to_string();
        let output = process.args_output(&["--cols", &width], None);
        assert_eq!(
            only_row(&output),
            format!("{prefix}{suffix}"),
            "extra={extra}"
        );
        assert!(display_columns(only_row(&output)) <= prefix_columns + extra);
    }
    let unlimited = process.args_output(&["-ww"], Some("60"));
    assert_eq!(only_row(&unlimited), process.command_line);
    let ascii = ps_command(&["-p", &process.pid, "-o", "args=", "-ww"], None)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let expected: String = process
        .command_line
        .bytes()
        .map(|byte| {
            if (b' '..=b'~').contains(&byte) {
                byte as char
            } else {
                '?'
            }
        })
        .collect();
    assert_eq!(only_row(&ascii), expected);
}

#[test]
fn command_controls_render_as_procps_text_instead_of_hex_or_ansi_escapes() {
    let process = WidthProcess::new(
        "CTRL:a\tb\nc\rd\u{b}e\u{c}f\u{1b}[31mg\u{7}h\u{7f}i:end",
        "comm\n\tname",
    );
    let expected: String = process
        .command_line
        .chars()
        .map(|ch| {
            if ch == '\n' {
                ' '
            } else if ch.is_control() {
                '?'
            } else {
                ch
            }
        })
        .collect();
    for options in [&["-ww"][..], &["--cols", "10000"][..]] {
        let output = process.args_output(options, Some("60"));
        assert_eq!(only_row(&output), expected, "{options:?}");
        assert!(!stdout(&output).contains('\u{1b}'));
        assert!(!stdout(&output).contains("\\x0a"));
    }
    let comm = ps(&["-p", &process.pid, "-o", "comm=", "-ww"]);
    assert_eq!(only_row(&comm), "comm??name");
}

fn ps_in_terminal(
    args: &[&str],
    columns: Option<&str>,
    terminal_width: u16,
    piped: bool,
) -> Output {
    let mut master = -1;
    let mut slave = -1;
    let size = libc::winsize {
        ws_row: 24,
        ws_col: terminal_width,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes both fd outputs on success; the winsize is
    // initialized and null optional name/termios arguments are permitted.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    };
    assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: successful openpty returned two distinct, owned file descriptors.
    let mut master = unsafe { File::from_raw_fd(master) };
    // SAFETY: slave is the second owned fd and is transferred exactly once.
    let slave = unsafe { File::from_raw_fd(slave) };
    let mut command = ps_command(args, columns);
    command
        .stdin(slave.try_clone().unwrap())
        .stderr(Stdio::piped());
    if piped {
        command.stdout(Stdio::piped());
    } else {
        command.stdout(slave.try_clone().unwrap());
    }
    let child = command.spawn().expect("run ps in a PTY");
    drop(command);
    drop(slave);
    let mut output = child.wait_with_output().unwrap();
    if !piped {
        let mut bytes = [0_u8; 4096];
        loop {
            match master.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => output.stdout.extend_from_slice(&bytes[..count]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
        output.stdout.retain(|&byte| byte != b'\r');
    }
    output
}

#[test]
fn actual_terminal_width_env_and_options_share_the_same_policy() {
    let payload = format!("WIDTH_PAYLOAD_{}", "x".repeat(400));
    let process = WidthProcess::new(&payload, "width_fixture");
    let cases: &[(&[&str], Option<&str>, bool, Option<usize>)] = &[
        (&[], None, false, Some(53)),
        (&[], Some("60"), false, Some(60)),
        (&["w"], None, false, Some(132)),
        (&["-w"], None, false, Some(132)),
        (&["ww"], None, false, None),
        (&["-ww"], None, false, None),
        (&["--cols", "50"], None, false, Some(50)),
        (&["--columns", "50"], Some("60"), false, Some(50)),
        (&["--width", "50", "w"], Some("60"), false, Some(132)),
        (&["ww", "--width", "50"], Some("60"), false, None),
        (&[], None, true, None),
        (&["-w"], None, true, None),
        (&[], Some("60"), true, Some(60)),
    ];
    for &(options, columns, piped, width) in cases {
        let mut args = vec!["-p", process.pid.as_str(), "-o", "args="];
        args.extend_from_slice(options);
        let output = ps_in_terminal(&args, columns, 53, piped);
        let expected = width.map_or(process.command_line.as_str(), |width| {
            &process.command_line[..width]
        });
        assert_eq!(
            only_row(&output),
            expected,
            "{options:?} COLUMNS={columns:?} piped={piped}"
        );
    }
    for mode in [&["-f"][..], &["x"][..]] {
        let mut args = vec!["-p", process.pid.as_str()];
        args.extend_from_slice(mode);
        let output = ps_in_terminal(&args, None, 53, false);
        assert!(output.status.success(), "{mode:?}: {:?}", output.stderr);
        assert_eq!(display_columns(stdout(&output).lines().nth(1).unwrap()), 53);
    }
}
