use procfs::process::{Process, Stat, Status};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Output, Stdio},
    sync::mpsc::{self, SyncSender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const LEADER: &str = "axe_thr_main";
const BUSY: &str = "z_AXE_thread";
const IDLE: &str = "a_AXE_thread";

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct ThreadProcess {
    child: OwnedChild,
    pid: String,
    busy: i32,
    idle: i32,
}

impl ThreadProcess {
    fn new(frozen: bool) -> Self {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "thread_process_fixture",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(
                "AXE_PS_THREAD_FIXTURE",
                if frozen { "frozen" } else { "live" },
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start owned Rust thread fixture");
        let mut child = OwnedChild(child);
        let mut output = BufReader::new(child.0.stdout.take().unwrap());
        let mut line = String::new();
        let (pid, busy, idle) = loop {
            line.clear();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "fixture exited before readiness"
            );
            if let Some((_, ready)) = line.split_once("AXE_PS_THREADS_READY ") {
                let mut ids = ready
                    .split_whitespace()
                    .map(|id| id.parse::<i32>().unwrap());
                break (
                    ids.next().unwrap(),
                    ids.next().unwrap(),
                    ids.next().unwrap(),
                );
            }
        };
        assert_eq!(pid, child.0.id() as i32);
        if frozen {
            wait_for_stop(&child.0);
        }
        Self {
            child,
            pid: pid.to_string(),
            busy,
            idle,
        }
    }

    fn ps(&self, options: &[&str]) -> Output {
        ps_command()
            .args(["-p", &self.pid])
            .args(options)
            .output()
            .expect("run ps")
    }

    fn snapshot(&self) -> (Stat, Status, Vec<(Stat, Status)>) {
        let process = Process::new(self.child.0.id() as i32).unwrap();
        let mut tasks: Vec<_> = process
            .tasks()
            .unwrap()
            .map(|task| {
                let task = task.unwrap();
                (task.stat().unwrap(), task.status().unwrap())
            })
            .collect();
        tasks.sort_unstable_by_key(|(stat, _)| stat.pid);
        (process.stat().unwrap(), process.status().unwrap(), tasks)
    }
}

fn wait_for_stop(child: &Child) {
    let mut status = 0;
    // SAFETY: only the owned, unreaped child is waited on; status is a valid
    // writable pointer. WUNTRACED observes the stop without reaping it.
    assert_eq!(
        unsafe { libc::waitpid(child.id() as i32, &mut status, libc::WUNTRACED) },
        child.id() as i32
    );
    assert!(libc::WIFSTOPPED(status), "owned fixture did not freeze");
}

struct IdleWorker {
    stop: Option<SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for IdleWorker {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn tid() -> i32 {
    // SAFETY: gettid has no pointer arguments or additional preconditions.
    unsafe { libc::syscall(libc::SYS_gettid) as i32 }
}

fn set_task(name: &str, nice: i32) {
    fs::write(format!("/proc/self/task/{}/comm", tid()), name).unwrap();
    // SAFETY: the target is this fixture's current thread, with an allowed
    // nonnegative nice value. No other process or test thread is modified.
    assert_eq!(
        unsafe { libc::setpriority(libc::PRIO_PROCESS, tid() as u32, nice) },
        0
    );
}

fn thread_cpu_ns() -> u64 {
    let mut clock = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: CLOCK_THREAD_CPUTIME_ID writes this initialized, aligned value.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut clock) },
        0
    );
    clock.tv_sec as u64 * 1_000_000_000 + clock.tv_nsec as u64
}

#[test]
#[ignore = "managed Rust process fixture launched by thread behavior tests"]
fn thread_process_fixture() {
    let Ok(mode) = std::env::var("AXE_PS_THREAD_FIXTURE") else {
        return;
    };
    let pid = std::process::id() as i32;
    let busy = tid();
    // libtest has a sleeping leader and this test worker. Using its worker as
    // the busy task gives exactly three real tasks, not a hidden fourth one.
    assert_ne!(pid, busy);
    fs::write(format!("/proc/self/task/{pid}/comm"), LEADER).unwrap();
    set_task(BUSY, 7);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (stop_tx, stop_rx) = mpsc::sync_channel(0);
    let idle = thread::Builder::new()
        .name(IDLE.into())
        .spawn(move || {
            set_task(IDLE, 12);
            ready_tx.send(tid()).unwrap();
            let _ = stop_rx.recv();
        })
        .unwrap();
    let _idle = IdleWorker {
        stop: Some(stop_tx),
        thread: Some(idle),
    };
    let idle = ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(Process::myself().unwrap().tasks().unwrap().count(), 3);

    if mode == "frozen" {
        // CPU-clock work, not wall-clock sleeping, guarantees the busy task
        // has at least one whole CPU second while the leader and idle do not.
        let until = thread_cpu_ns() + 1_100_000_000;
        while thread_cpu_ns() < until {
            std::hint::black_box(123_u64.wrapping_mul(456));
        }
    }
    println!("AXE_PS_THREADS_READY {pid} {busy} {idle}");
    std::io::stdout().flush().unwrap();
    if mode == "frozen" {
        // SAFETY: SIGSTOP only freezes this owned fixture's thread group.
        assert_eq!(unsafe { libc::raise(libc::SIGSTOP) }, 0);
        let mut shutdown = String::new();
        std::io::stdin().read_to_string(&mut shutdown).unwrap();
    } else {
        // The busy task remains runnable; EOF is an owned shutdown channel
        // with no extra helper thread that would change the visible NLWP.
        // SAFETY: fd 0 is this child fixture's owned stdin pipe. F_GETFL and
        // F_SETFL take integer arguments and do not dereference any pointers.
        let flags = unsafe { libc::fcntl(0, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(0, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut byte = [0_u8; 1];
        loop {
            for _ in 0..10_000 {
                std::hint::black_box(123_u64.wrapping_mul(456));
            }
            // SAFETY: byte is valid writable storage of exactly one byte;
            // the owned stdin fd remains open for the fixture's lifetime.
            let read = unsafe { libc::read(0, byte.as_mut_ptr().cast(), byte.len()) };
            if read == 0 {
                break;
            }
            if read < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock
            {
                break;
            }
        }
    }
}

fn ps_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ps"));
    command
        .env_remove("COLUMNS")
        .env_remove("LINES")
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC");
    command
}

fn words(output: &Output) -> Vec<Vec<&str>> {
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| line.split_whitespace().collect())
        .collect()
}

fn successful(output: &Output) -> Vec<Vec<&str>> {
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    words(output)
}

fn native_command() -> Option<Command> {
    let program = [
        "/run/host-profiles/system/bin/ps",
        "/run/current-system/sw/bin/ps",
        "/usr/bin/ps",
        "/bin/ps",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).is_file())?;
    let mut command = Command::new(program);
    command
        .env_remove("COLUMNS")
        .env_remove("LINES")
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC");
    Some(command)
}

#[test]
fn flat_modes_report_real_task_identity_comm_nice_and_cpu_with_shared_memory() {
    let process = ThreadProcess::new(true);
    let (aggregate, status, tasks) = process.snapshot();
    assert_eq!(tasks.len(), 3);
    let tps = procfs::ticks_per_second();
    let expected: Vec<Vec<String>> = tasks
        .iter()
        .map(|(stat, _)| {
            vec![
                process.pid.clone(),
                process.pid.clone(),
                stat.pid.to_string(),
                stat.pid.to_string(),
                stat.pid.to_string(),
                "3".into(),
                stat.comm.clone(),
                stat.nice.to_string(),
                stat.state.to_string(),
                ((stat.utime + stat.stime) / tps).to_string(),
                aggregate.ppid.to_string(),
                status.vmrss.unwrap_or(0).to_string(),
                (aggregate.vsize / 1024).to_string(),
                status.euid.to_string(),
                status.egid.to_string(),
            ]
        })
        .collect();
    let format =
        "pid=,tgid=,tid=,lwp=,spid=,nlwp=,comm=,ni=,state=,cputimes=,ppid=,rss=,vsz=,uid=,gid=";
    for mode in ["-L", "-T", "H", "axH", "auxH"] {
        let output = process.ps(&[mode, "-o", format]);
        assert_eq!(successful(&output), expected, "{mode}");
        // AXE retains its existing custom-format override of BSD u. Native
        // procps rejects auxH combined with -o; fixed auxH is checked below.
        if let Some(mut native) = native_command().filter(|_| mode != "auxH") {
            let native = native
                .args(["-p", &process.pid, mode, "-o", format])
                .output()
                .unwrap();
            assert_eq!(successful(&output), successful(&native), "native {mode}");
        }
    }
    let main = tasks
        .iter()
        .find(|(stat, _)| stat.pid == aggregate.pid)
        .unwrap();
    let busy = tasks
        .iter()
        .find(|(stat, _)| stat.pid == process.busy)
        .unwrap();
    let idle = tasks
        .iter()
        .find(|(stat, _)| stat.pid == process.idle)
        .unwrap();
    assert_eq!(main.0.comm, LEADER);
    assert_eq!(busy.0.comm, BUSY);
    assert_eq!(busy.0.nice, 7);
    assert_eq!(idle.0.comm, IDLE);
    assert_eq!(idle.0.nice, 12);
    assert!(busy.0.utime + busy.0.stime >= tps);
    assert!(aggregate.utime + aggregate.stime > main.0.utime + main.0.stime);
    let output = process.ps(&["-o", "pid=,tid=,cputimes="]);
    assert_eq!(
        successful(&output),
        [vec![
            process.pid.as_str(),
            process.pid.as_str(),
            &((aggregate.utime + aggregate.stime) / tps).to_string()
        ]]
    );
}

#[test]
fn fixed_thread_formats_match_native_column_order_and_real_thread_count() {
    let process = ThreadProcess::new(true);
    let cases: &[(&[&str], &[&str], usize)] = &[
        (&["-L"], &["PID", "LWP", "TTY", "TIME", "CMD"], 3),
        (&["-T"], &["PID", "SPID", "TTY", "TIME", "CMD"], 3),
        (
            &["-Lf"],
            &[
                "UID", "PID", "PPID", "LWP", "C", "NLWP", "STIME", "TTY", "TIME", "CMD",
            ],
            3,
        ),
        (
            &["-Tf"],
            &[
                "UID", "PID", "SPID", "PPID", "C", "STIME", "TTY", "TIME", "CMD",
            ],
            3,
        ),
        (&["H"], &["PID", "TTY", "STAT", "TIME", "COMMAND"], 3),
        (&["axH"], &["PID", "TTY", "STAT", "TIME", "COMMAND"], 3),
        (
            &["auxH"],
            &[
                "USER", "PID", "%CPU", "%MEM", "VSZ", "RSS", "TTY", "STAT", "START", "TIME",
                "COMMAND",
            ],
            3,
        ),
        (
            &["u", "-L"],
            &[
                "USER", "PID", "LWP", "%CPU", "NLWP", "%MEM", "VSZ", "RSS", "TTY", "STAT", "START",
                "TIME", "COMMAND",
            ],
            3,
        ),
        (
            &["u", "-T"],
            &[
                "USER", "PID", "SPID", "%CPU", "%MEM", "VSZ", "RSS", "TTY", "STAT", "START",
                "TIME", "COMMAND",
            ],
            3,
        ),
        (&["-m"], &["PID", "TTY", "TIME", "CMD"], 4),
        (&["m"], &["PID", "TTY", "STAT", "TIME", "COMMAND"], 4),
        (&["axm"], &["PID", "TTY", "STAT", "TIME", "COMMAND"], 4),
        (&["-m", "-L"], &["PID", "LWP", "TTY", "TIME", "CMD"], 4),
        (&["-m", "-T"], &["PID", "SPID", "TTY", "TIME", "CMD"], 4),
        (
            &["H", "-L"],
            &["PID", "LWP", "TTY", "STAT", "TIME", "COMMAND"],
            3,
        ),
    ];
    for &(options, headers, count) in cases {
        let output = process.ps(options);
        let rows = successful(&output);
        assert_eq!(rows[0], headers, "{options:?}");
        assert_eq!(rows.len(), count + 1, "{options:?}");
        if let Some(mut native) = native_command() {
            let native = native
                .args(["-p", &process.pid])
                .args(options)
                .output()
                .unwrap();
            let native = successful(&native);
            assert_eq!(rows[0], native[0], "native {options:?}");
            assert_eq!(rows.len(), native.len(), "native {options:?}");
        }
    }
    let output = process.ps(&["-L", "-o", "comm=NAME,tid=TASK,pid=GROUP,nlwp=COUNT"]);
    let rows = successful(&output);
    assert_eq!(rows[0], ["NAME", "TASK", "GROUP", "COUNT"]);
    assert_eq!(
        rows[1],
        [LEADER, process.pid.as_str(), process.pid.as_str(), "3"]
    );
}

#[test]
fn mixed_modes_mask_field_applicability_and_keep_aggregate_cpu_distinct() {
    let process = ThreadProcess::new(true);
    let (aggregate, status, tasks) = process.snapshot();
    let tps = procfs::ticks_per_second();
    let format = "pid=,tgid=,tid=,lwp=,spid=,nlwp=,comm=,ni=,state=,stat=,pri=,psr=,wchan=,cls=,blocked=,ignored=,caught=,rss=,vsz=,ppid=,cputimes=,uid=,gid=";
    for mode in ["-m", "m", "axm"] {
        let output = process.ps(&[mode, "-o", format]);
        let rows = successful(&output);
        assert_eq!(rows.len(), 4, "{mode}");
        let summary = &rows[0];
        assert_eq!(
            &summary[..7],
            [
                process.pid.as_str(),
                process.pid.as_str(),
                "-",
                "-",
                "-",
                "3",
                LEADER
            ]
        );
        assert!(summary[7..17].iter().all(|cell| *cell == "-"));
        assert_eq!(summary[17], status.vmrss.unwrap_or(0).to_string());
        assert_eq!(summary[18], (aggregate.vsize / 1024).to_string());
        assert_eq!(summary[19], aggregate.ppid.to_string());
        assert_eq!(
            summary[20],
            ((aggregate.utime + aggregate.stime) / tps).to_string()
        );
        for (row, (task, _)) in rows[1..].iter().zip(&tasks) {
            assert_eq!(
                &row[..7],
                [
                    "-",
                    "-",
                    &task.pid.to_string(),
                    &task.pid.to_string(),
                    &task.pid.to_string(),
                    "-",
                    "-"
                ]
            );
            assert_eq!(row[7], task.nice.to_string());
            assert_eq!(row[8], "T");
            assert!(row[9].starts_with('T'));
            assert!(
                row[10..12]
                    .iter()
                    .chain(&row[13..17])
                    .all(|cell| *cell != "-")
            );
            assert!(row[17..20].iter().all(|cell| *cell == "-"));
            assert_eq!(row[20], ((task.utime + task.stime) / tps).to_string());
            assert_eq!(&row[21..], &summary[21..]);
        }
        if let Some(mut native) = native_command() {
            let native = native
                .args(["-p", &process.pid, mode, "-o", format])
                .output()
                .unwrap();
            let native = successful(&native);
            assert_eq!(rows.len(), native.len());
            // Keep the genuine aggregate summary CPU. WCHAN retains AXE's
            // dynamic width rather than native procps's clipped symbol.
            for (index, (row, native)) in rows.iter().zip(&native).enumerate() {
                assert_eq!(&row[..12], &native[..12], "native {mode}");
                assert!(row[12].starts_with(native[12]), "native {mode} wchan");
                assert_eq!(&row[13..20], &native[13..20], "native {mode}");
                assert_eq!(&row[21..], &native[21..], "native {mode}");
                if index > 0 {
                    assert_eq!(row[20], native[20], "native task CPU in {mode}");
                }
            }
        }
    }
}

#[test]
fn flat_sort_is_global_and_mixed_sort_keeps_every_group_summary_before_its_tasks() {
    let first = ThreadProcess::new(true);
    let second = ThreadProcess::new(true);
    let pids = format!("{},{}", first.pid, second.pid);
    let (_, _, first_tasks) = first.snapshot();
    let (_, _, second_tasks) = second.snapshot();
    let mut expected: Vec<_> = first_tasks.iter().chain(&second_tasks).collect();
    expected.sort_unstable_by(|(left, _), (right, _)| {
        left.comm
            .cmp(&right.comm)
            .then_with(|| right.pid.cmp(&left.pid))
    });
    for mode in ["-L", "-T", "H"] {
        let output = ps_command()
            .args([
                "-p",
                &pids,
                mode,
                "--sort",
                "comm,-tid",
                "-o",
                "pid=,tid=,comm=",
            ])
            .output()
            .unwrap();
        let rows = successful(&output);
        assert_eq!(rows.len(), 6, "{mode}");
        for (row, (task, status)) in rows.iter().zip(&expected) {
            assert_eq!(
                row,
                &vec![
                    status.tgid.to_string(),
                    task.pid.to_string(),
                    task.comm.clone()
                ]
            );
        }
        if let Some(mut native) = native_command() {
            let native = native
                .args([
                    "-p",
                    &pids,
                    "H",
                    "--sort",
                    "comm,-tid",
                    "-o",
                    "pid=,tid=,comm=",
                ])
                .output()
                .unwrap();
            assert_eq!(rows, successful(&native), "native H oracle for {mode}");
        }
    }
    for sort_options in [
        &["--sort", "comm"][..],
        &["k", "comm"],
        &["kcomm"],
        &["axkcomm"],
    ] {
        let output = ps_command()
            .args(["-p", &first.pid, "-L", "-o", "tid=,comm="])
            .args(sort_options)
            .output()
            .unwrap();
        let rows = successful(&output);
        assert_eq!(
            rows.iter().map(|row| row[1]).collect::<Vec<_>>(),
            [IDLE, LEADER, BUSY],
            "{sort_options:?}"
        );
    }
    let mut descending: Vec<_> = first_tasks
        .iter()
        .map(|(task, _)| task.pid.to_string())
        .collect();
    descending.sort_unstable_by_key(|tid| std::cmp::Reverse(tid.parse::<i32>().unwrap()));
    for options in [&["k", "-tid"][..], &["k-tid"], &["axk-tid"], &["-k-tid"]] {
        let output = ps_command()
            .args(["-p", &first.pid, "-L", "-o", "tid="])
            .args(options)
            .output()
            .unwrap();
        assert_eq!(
            successful(&output)
                .iter()
                .map(|row| row[0])
                .collect::<Vec<_>>(),
            descending,
            "{options:?}"
        );
    }
    let output = first.ps(&["-L", "--sort", "pri", "-o", "tid=,pri=,ni="]);
    let mut by_priority: Vec<_> = first_tasks.iter().map(|(task, _)| task).collect();
    by_priority.sort_unstable_by_key(|task| (task.priority, task.pid));
    for (row, task) in successful(&output).iter().zip(by_priority) {
        assert_eq!(
            row,
            &vec![
                task.pid.to_string(),
                (39 - task.priority).to_string(),
                task.nice.to_string()
            ]
        );
    }
    let mut groups = [(&first, &first_tasks), (&second, &second_tasks)];
    groups.sort_unstable_by_key(|(process, _)| std::cmp::Reverse(process.child.0.id()));
    for mode in ["-m", "m"] {
        let output = ps_command()
            .args([
                "-p",
                &pids,
                mode,
                "--sort",
                "comm,-pid",
                "-o",
                "pid=,tid=,comm=,ni=",
            ])
            .output()
            .unwrap();
        let rows = successful(&output);
        assert_eq!(rows.len(), 8, "{mode}");
        for (block, (process, tasks)) in rows.chunks_exact(4).zip(&groups) {
            assert_eq!(block[0], [process.pid.as_str(), "-", LEADER, "-"]);
            let mut tasks: Vec<_> = tasks.iter().collect();
            tasks.sort_unstable_by(|(left, _), (right, _)| left.comm.cmp(&right.comm));
            for (row, (task, _)) in block[1..].iter().zip(tasks) {
                assert_eq!(
                    row,
                    &vec![
                        "-".to_string(),
                        task.pid.to_string(),
                        "-".to_string(),
                        task.nice.to_string()
                    ]
                );
            }
        }
    }
}

#[test]
fn running_only_selects_task_state_while_mixed_mode_uses_process_state_and_terminates() {
    let process = ThreadProcess::new(false);
    for mode in ["-L", "-T", "H"] {
        let output = process.ps(&[mode, "r", "-o", "pid=,tid=,comm=,ni=,state="]);
        assert_eq!(
            successful(&output),
            [vec![
                process.pid.as_str(),
                &process.busy.to_string(),
                BUSY,
                "7",
                "R"
            ]],
            "{mode}"
        );
    }
    let mut command = ps_command();
    command
        .args(["-p", &process.pid, "-m", "r", "-o", "pid=,tid=,state="])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = OwnedChild(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "mixed running-only selection did not terminate"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let mut output = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert_eq!(child.0.wait().unwrap().code(), Some(1));
    assert!(output.is_empty());
}

#[test]
fn single_thread_and_missing_pid_rows_preserve_mode_headers_and_exit_status() {
    let child = OwnedChild(
        Command::new("sleep")
            .arg("90")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    // SAFETY: this is the owned, unreaped single-thread child; its PID cannot
    // be reused before our RAII guard reaps it.
    assert_eq!(unsafe { libc::kill(child.0.id() as i32, libc::SIGSTOP) }, 0);
    wait_for_stop(&child.0);
    let pid = child.0.id().to_string();
    for mode in ["-L", "-T", "H", "-m", "m"] {
        let output = ps_command()
            .args(["-p", &pid, mode, "-o", "pid=,tid=,nlwp="])
            .output()
            .unwrap();
        let rows = successful(&output);
        if matches!(mode, "-m" | "m") {
            assert_eq!(
                rows,
                [vec![pid.as_str(), "-", "1"], vec!["-", pid.as_str(), "-"]],
                "{mode}"
            );
        } else {
            assert_eq!(rows, [vec![pid.as_str(), pid.as_str(), "1"]], "{mode}");
        }
        let empty = ps_command()
            .args(["-p", "2147483647", mode, "-o", "pid,tid,nlwp"])
            .output()
            .unwrap();
        assert_eq!(empty.status.code(), Some(1), "{mode}");
        assert_eq!(words(&empty), [vec!["PID", "TID", "NLWP"]], "{mode}");
        assert!(empty.stderr.is_empty());
    }
}

#[test]
fn pid_selection_is_tgid_only_and_flag_or_value_conflicts_fail_without_rows() {
    let process = ThreadProcess::new(true);
    let tid = process.busy.to_string();
    for mode in ["-L", "-T", "H", "-m", "m"] {
        let output = ps_command()
            .args(["-p", &tid, mode, "-o", "pid=,tid="])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{mode}");
        assert!(output.stdout.is_empty(), "{mode}");
        assert!(output.stderr.is_empty(), "{mode}");
    }
    let conflicts: &[&[&str]] = &[
        &["-L", "-T"],
        &["H", "m"],
        &["H", "-m"],
        &["m", "-m"],
        &["-L", "H", "-o", "pid,tid"],
        &["-T", "-m", "-o", "pid,tid"],
        &["-Lf", "-o", "pid,tid"],
        &["-Tf", "-o", "pid,tid"],
        &["--sort", "pid", "--sort", "-tid"],
        &["--sort", "pid", "k-tid"],
    ];
    for &options in conflicts {
        let output = process.ps(options);
        assert_eq!(output.status.code(), Some(1), "{options:?}");
        assert!(output.stdout.is_empty(), "{options:?}");
        assert!(!output.stderr.is_empty(), "{options:?}");
    }
    for value in ["H", "m", "axH", "k-tid"] {
        let output = process.ps(&["-L", "--sort", value, "-o", "pid,tid"]);
        assert_eq!(output.status.code(), Some(1), "sort value {value}");
        assert!(output.stdout.is_empty());
    }
    let output = process.ps(&["-L", "-o", "comm=H,tid=m,pid=axH,nlwp=k"]);
    assert_eq!(successful(&output)[0], ["H", "m", "axH", "k"]);
    let output = process.ps(&["-H"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
