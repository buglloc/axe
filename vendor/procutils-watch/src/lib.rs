use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use procutils_common::{MAX_TERM_WIDTH, man::ManContent};
use std::{
    io::{self, IsTerminal, Read, Write},
    os::unix::process::CommandExt,
    process::{Child, Command, ExitCode, ExitStatus, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const MAN: ManContent = ManContent {
    description: Some(include_str!("../man/description.man")),
    extra_sections: &[
        ("EXAMPLES", include_str!("../man/examples.man")),
        ("NOTES", include_str!("../man/notes.man")),
        ("SEE ALSO", include_str!("../man/see_also.man")),
    ],
};

/// Execute a program periodically, showing output fullscreen.
#[derive(Parser)]
#[command(name = "watch", version, about, max_term_width = MAX_TERM_WIDTH)]
pub struct Args {
    /// Beep if command has a non-zero exit.
    #[arg(short, long)]
    beep: bool,

    /// Interpret ANSI color and style sequences.
    #[arg(short, long)]
    color: bool,

    /// Do not interpret ANSI color and style sequences.
    #[arg(short = 'C', long)]
    no_color: bool,

    /// Freeze updates on command error, and exit after a key press.
    #[arg(short, long)]
    errexit: bool,

    /// Exit when the output of command changes.
    #[arg(short = 'g', long)]
    chgexit: bool,

    /// Specify update interval in seconds.
    #[arg(short = 'n', long, default_value = "2.0")]
    interval: f64,

    /// Make watch attempt to run command every --interval seconds precisely.
    #[arg(short, long)]
    precise: bool,

    /// Exit when output does not change for the given number of cycles.
    #[arg(short = 'q', long)]
    equexit: Option<u64>,

    /// Turn off the header.
    #[arg(short = 't', long)]
    no_title: bool,

    /// Turn off line wrapping. Long lines will be truncated.
    #[arg(short = 'w', long)]
    no_wrap: bool,

    /// Pass command to exec instead of sh -c.
    #[arg(short = 'x', long)]
    exec: bool,

    /// Command and arguments to execute.
    #[arg(required = true, trailing_var_arg = true)]
    command: Vec<String>,
}

fn terminal_size() -> (u16, u16) {
    use std::os::fd::AsFd;
    rustix::termios::tcgetwinsize(std::io::stdout().as_fd())
        .map(|ws| (ws.ws_col, ws.ws_row))
        .unwrap_or((80, 24))
}

fn format_time() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let secs_of_day = now % 86400;
    let h = secs_of_day / 3600;
    let m = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;

    // Approximate local time using UTC offset heuristic
    format!("{h:02}:{m:02}:{s:02}")
}

fn run_command(args: &Args, terminal: &TerminalState) -> io::Result<Option<(String, Option<i32>)>> {
    let mut command = if args.exec {
        let mut command = Command::new(&args.command[0]);
        command.args(&args.command[1..]);
        command
    } else {
        let mut command = Command::new("sh");
        command.arg("-c").arg(args.command.join(" "));
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Ok(Some((format!("watch: command error: {error}\n"), Some(4))));
        }
    };
    let Some(output) =
        collect_command(child, || terminal.wait_for_quit(Duration::from_millis(20)))?
    else {
        return Ok(None);
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(Some((text, output.status.code())))
}

fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn cancel_command(child: &mut Child) -> io::Result<ExitStatus> {
    // Only the collecting thread may reap this child. Its live or zombie
    // PID still anchors both numeric signal targets until wait below.
    let pid = rustix::process::Pid::from_child(child);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    let _ = child.kill();
    child.wait()
}

fn collect_command(
    mut child: Child,
    mut interrupted: impl FnMut() -> io::Result<bool>,
) -> io::Result<Option<Output>> {
    let stdout = child.stdout.take().expect("piped command stdout");
    let stderr = child.stderr.take().expect("piped command stderr");
    std::thread::scope(|scope| {
        let stdout =
            match std::thread::Builder::new().spawn_scoped(scope, move || read_pipe(stdout)) {
                Ok(reader) => reader,
                Err(error) => {
                    let _ = cancel_command(&mut child);
                    return Err(error);
                }
            };
        let stderr =
            match std::thread::Builder::new().spawn_scoped(scope, move || read_pipe(stderr)) {
                Ok(reader) => reader,
                Err(error) => {
                    let _ = cancel_command(&mut child);
                    return Err(error);
                }
            };
        let completed = loop {
            // Readers never wait on the process. Reap only when both pipes
            // are drained, then leave the cancellation loop immediately.
            if stdout.is_finished() && stderr.is_finished() {
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(Some(status)),
                    Ok(None) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    // No numeric signalling after an errored wait: ECHILD
                    // would mean that the PID is no longer ours to signal.
                    Err(error) => return Err(error),
                }
            }
            match interrupted() {
                Ok(true) => break Ok(None),
                Ok(false) => {}
                Err(error) => break Err(error),
            }
        };
        let completed = match completed {
            Ok(Some(status)) => Ok(Some(status)),
            Ok(None) => cancel_command(&mut child).map(|_| None),
            Err(error) => {
                let _ = cancel_command(&mut child);
                Err(error)
            }
        };
        let stdout = stdout
            .join()
            .map_err(|_| io::Error::other("stdout reader panicked"))?;
        let stderr = stderr
            .join()
            .map_err(|_| io::Error::other("stderr reader panicked"))?;
        let Some(status) = completed? else {
            return Ok(None);
        };
        Ok(Some(Output {
            status,
            stdout: stdout?,
            stderr: stderr?,
        }))
    })
}

fn print_header(out: &mut impl Write, args: &Args, cols: u16, newline: &str) -> io::Result<()> {
    let time = format_time();
    let left = format!("Every {:.1}s: {}", args.interval, args.command.join(" "));
    let left = truncate_line(&left, cols.saturating_sub(time.len() as u16 + 1));
    let padding = (cols as usize).saturating_sub(left.width() + time.len());
    write!(
        out,
        "{left}{time:>width$}{newline}{newline}",
        width = padding + time.len(),
    )
}

fn truncate_line(line: &str, cols: u16) -> &str {
    let mut width = 0;
    for (offset, grapheme) in line.grapheme_indices(true) {
        let cells = if grapheme == "\t" {
            8 - width % 8
        } else {
            grapheme.width()
        };
        if width + cells > cols as usize {
            return &line[..offset];
        }
        width += cells;
    }
    line
}

struct TerminationSignals {
    requested: Arc<AtomicBool>,
    registrations: [Option<signal_hook::SigId>; 3],
}

impl TerminationSignals {
    fn new() -> io::Result<Self> {
        let mut signals = Self {
            requested: Arc::new(AtomicBool::new(false)),
            registrations: [None, None, None],
        };
        for (registration, signal) in signals.registrations.iter_mut().zip([
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
        ]) {
            *registration = Some(signal_hook::flag::register(
                signal,
                Arc::clone(&signals.requested),
            )?);
        }
        Ok(signals)
    }

    fn requested(&self) -> bool {
        self.requested.load(Ordering::Relaxed)
    }
}

impl Drop for TerminationSignals {
    fn drop(&mut self) {
        for registration in self.registrations.iter_mut().filter_map(Option::take) {
            signal_hook::low_level::unregister(registration);
        }
    }
}

struct TerminalState {
    interactive: bool,
    no_wrap: bool,
    // Fields drop after TerminalState::drop restores the terminal.
    signals: TerminationSignals,
}

impl TerminalState {
    fn new(no_wrap: bool) -> io::Result<Self> {
        let signals = TerminationSignals::new()?;
        let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
        if interactive {
            crossterm::terminal::enable_raw_mode()?;
        }
        let state = Self {
            interactive,
            no_wrap,
            signals,
        };
        if no_wrap {
            let mut out = io::stdout().lock();
            out.write_all(b"\x1b[?7l")?;
            out.flush()?;
        }
        Ok(state)
    }

    fn wait_for_quit(&self, timeout: Duration) -> io::Result<bool> {
        let start = Instant::now();
        loop {
            if self.signals.requested() {
                return Ok(true);
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            let step = remaining.min(Duration::from_millis(20));
            if self.interactive {
                match event::poll(step) {
                    Ok(true) => {
                        if let Event::Key(key) = event::read()?
                            && key.kind == KeyEventKind::Press
                            && (matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
                                || (key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL)))
                        {
                            return Ok(true);
                        }
                    }
                    Ok(false) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            } else {
                std::thread::sleep(step);
            }
            if start.elapsed() >= timeout {
                return Ok(self.signals.requested());
            }
        }
    }

    fn wait_for_any_key(&self) -> io::Result<()> {
        let mut input = io::stdin();
        loop {
            if self.signals.requested() {
                return Ok(());
            }
            if self.interactive {
                match event::poll(Duration::from_millis(20)) {
                    Ok(true) => {
                        if let Event::Key(key) = event::read()?
                            && key.kind == KeyEventKind::Press
                        {
                            return Ok(());
                        }
                    }
                    Ok(false) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            } else {
                let ready = {
                    let mut fds = [rustix::event::PollFd::new(
                        &input,
                        rustix::event::PollFlags::IN,
                    )];
                    rustix::event::poll(
                        &mut fds,
                        Some(&rustix::event::Timespec {
                            tv_sec: 0,
                            tv_nsec: 20_000_000,
                        }),
                    )
                };
                match ready {
                    Ok(0) => {}
                    Ok(_) => {
                        input.read(&mut [0])?;
                        return Ok(());
                    }
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
}

impl Drop for TerminalState {
    fn drop(&mut self) {
        if self.no_wrap {
            let mut out = io::stdout().lock();
            let _ = out.write_all(b"\x1b[?7h");
            let _ = out.flush();
        }
        if self.interactive {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

pub fn run(args: Args) -> ExitCode {
    let terminal = match TerminalState::new(args.no_wrap) {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("watch: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = run_loop(&args, &terminal);
    if terminal.signals.requested() {
        return ExitCode::SUCCESS;
    }
    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("watch: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_loop(args: &Args, terminal: &TerminalState) -> io::Result<ExitCode> {
    let interval = Duration::from_secs_f64(args.interval.max(0.1));
    let mut prev_output: Option<String> = None;
    let mut unchanged_count: u64 = 0;
    let newline = if terminal.interactive { "\r\n" } else { "\n" };

    loop {
        if terminal.wait_for_quit(Duration::ZERO)? {
            return Ok(ExitCode::SUCCESS);
        }
        let start = Instant::now();
        let (cols, rows) = terminal_size();
        let Some((output, exit_code)) = run_command(args, terminal)? else {
            return Ok(ExitCode::SUCCESS);
        };

        {
            let mut out = io::stdout().lock();
            out.write_all(b"\x1b[2J\x1b[H")?;
            let mut available_rows = rows as usize;
            if !args.no_title {
                print_header(&mut out, args, cols, newline)?;
                available_rows = available_rows.saturating_sub(2);
            }
            for line in output.lines().take(available_rows) {
                let line = if args.no_wrap && !args.color {
                    truncate_line(line, cols)
                } else {
                    line
                };
                write!(out, "{line}{newline}")?;
            }
            if args.beep && exit_code != Some(0) {
                out.write_all(b"\x07")?;
            }
            out.flush()?;
        }

        if args.errexit && exit_code != Some(0) {
            write!(
                io::stderr().lock(),
                "watch: command exit with a non-zero status, press a key to exit{newline}"
            )?;
            terminal.wait_for_any_key()?;
            return Ok(if terminal.signals.requested() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        if args.chgexit
            && let Some(prev) = &prev_output
            && *prev != output
        {
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(cycles) = args.equexit
            && let Some(prev) = &prev_output
        {
            if *prev == output {
                unchanged_count += 1;
                if unchanged_count >= cycles {
                    return Ok(ExitCode::SUCCESS);
                }
            } else {
                unchanged_count = 0;
            }
        }
        prev_output = Some(output);
        let delay = if args.precise {
            interval.saturating_sub(start.elapsed())
        } else {
            interval
        };
        if terminal.wait_for_quit(delay)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn command_fixture_child() {
        if std::env::var_os("WATCH_CAPTURE_FIXTURE").is_none() {
            return;
        }
        io::stdin().read_exact(&mut [0]).unwrap();
        io::stdout().write_all(b"captured stdout\n").unwrap();
        io::stderr().write_all(b"captured stderr\n").unwrap();
        std::process::exit(7);
    }

    #[test]
    fn cancellation_keeps_the_exited_child_unreaped_until_signalling_is_finished() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::command_fixture_child", "--nocapture"])
            .env("WATCH_CAPTURE_FIXTURE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut release = child.stdin.take();
        let output = collect_command(child, || {
            release.take().unwrap().write_all(b"x")?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
                if status.lines().any(|line| {
                    line.starts_with("State:") && line.split_whitespace().nth(1) == Some("Z")
                }) {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "fixture did not exit",
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(true)
        })
        .unwrap();
        assert!(output.is_none());
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
}
