use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Output, Stdio},
};

fn ps(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ps"))
        .args(args)
        .env_remove("COLUMNS")
        .env_remove("LINES")
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC")
        .output()
        .expect("run ps")
}

fn rows(output: &Output) -> Vec<i32> {
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| line.trim().parse().expect("a PID row"))
        .collect()
}

struct FrozenProcess {
    child: Child,
    pid: i32,
}

impl FrozenProcess {
    fn new(
        name: &str,
        payload: &str,
        nice: i32,
        memory_mib: usize,
        extra_fds: usize,
        policy: i32,
    ) -> Self {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sorting_process_fixture",
                "--ignored",
                "--nocapture",
                "--skip",
                payload,
            ])
            .env("AXE_PS_SORT_NAME", name)
            .env("AXE_PS_SORT_NICE", nice.to_string())
            .env("AXE_PS_SORT_MEMORY", memory_mib.to_string())
            .env("AXE_PS_SORT_FDS", extra_fds.to_string())
            .env("AXE_PS_SORT_POLICY", policy.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start managed sorting process");
        let mut process = Self {
            pid: child.id() as i32,
            child,
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
            if line.contains("AXE_PS_SORT_READY") {
                break;
            }
        }
        let mut status = 0;
        // SAFETY: this is our unreaped child, and status is initialized/aligned.
        // WUNTRACED observes the process-wide freeze without reaping the child.
        let waited = unsafe { libc::waitpid(process.pid, &mut status, libc::WUNTRACED) };
        assert_eq!(waited, process.pid);
        assert!(libc::WIFSTOPPED(status), "fixture did not freeze");
        process
    }
}

impl Drop for FrozenProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "managed process fixture, launched by sorting behavior tests"]
fn sorting_process_fixture() {
    let Ok(name) = std::env::var("AXE_PS_SORT_NAME") else {
        return;
    };
    let nice: i32 = std::env::var("AXE_PS_SORT_NICE").unwrap().parse().unwrap();
    let memory_mib: usize = std::env::var("AXE_PS_SORT_MEMORY")
        .unwrap()
        .parse()
        .unwrap();
    let extra_fds: usize = std::env::var("AXE_PS_SORT_FDS").unwrap().parse().unwrap();
    let policy: i32 = std::env::var("AXE_PS_SORT_POLICY")
        .unwrap()
        .parse()
        .unwrap();
    let pid = std::process::id();
    std::fs::write("/proc/self/comm", name).unwrap();
    // SAFETY: setpriority targets only this fixture's group-leader PID. The
    // nonnegative priorities do not require privilege and do not affect tests.
    assert_eq!(
        unsafe { libc::setpriority(libc::PRIO_PROCESS, pid, nice) },
        0
    );
    let parameters = libc::sched_param { sched_priority: 0 };
    // SAFETY: the initialized sched_param is borrowed only during the call;
    // OTHER/BATCH/IDLE modify only this fixture's leader scheduling policy.
    assert_eq!(
        unsafe { libc::sched_setscheduler(pid as i32, policy, &parameters) },
        0
    );
    let mut memory = vec![0_u8; memory_mib * 1024 * 1024];
    memory.fill(0x5a);
    let descriptors: Vec<File> = (0..extra_fds)
        .map(|_| File::open("/dev/null").unwrap())
        .collect();
    std::hint::black_box(&memory);
    std::hint::black_box(&descriptors);
    println!("AXE_PS_SORT_READY");
    std::io::stdout().flush().unwrap();
    // SAFETY: SIGSTOP freezes every thread in this managed child, keeping all
    // raw memory/CPU/thread snapshots stable until its RAII owner kills it.
    unsafe { libc::raise(libc::SIGSTOP) };
    loop {
        std::thread::park();
        std::hint::black_box(&memory);
        std::hint::black_box(&descriptors);
    }
}

fn fixture_processes() -> [FrozenProcess; 3] {
    // Creation order, command names and full argv deliberately disagree.
    [
        FrozenProcess::new("z_axe_sort", "a_sort_payload", 0, 3, 0, libc::SCHED_OTHER),
        FrozenProcess::new("a_axe_sort", "z_sort_payload", 12, 25, 5, libc::SCHED_BATCH),
        FrozenProcess::new("m_axe_sort", "m_sort_payload", 6, 11, 2, libc::SCHED_IDLE),
    ]
}

fn pid_list(processes: &[FrozenProcess]) -> String {
    processes
        .iter()
        .map(|process| process.pid.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn sorted_pids(processes: &[FrozenProcess], spec: &str) -> Vec<i32> {
    rows(&ps(&[
        "-p",
        &pid_list(processes),
        "-o",
        "pid=",
        "--sort",
        spec,
    ]))
}

#[test]
fn numeric_sorting_uses_raw_memory_nice_priority_and_direction() {
    let processes = fixture_processes();
    let [z, a, m] = processes.each_ref().map(|process| process.pid);
    for spec in ["pid", "+pid", "tgid", "tid", "spid", "lwp"] {
        assert_eq!(sorted_pids(&processes, spec), [z, a, m], "{spec}");
    }
    assert_eq!(sorted_pids(&processes, "-pid"), [m, a, z]);
    for spec in [
        "rss", "rsz", "%mem", "pmem", "vsz", "vsize", "nice", "ni", "pri", "fds",
    ] {
        assert_eq!(sorted_pids(&processes, spec), [z, m, a], "{spec}");
        assert_eq!(
            sorted_pids(&processes, &format!("-{spec}")),
            [a, m, z],
            "-{spec}"
        );
    }
    for spec in [
        "uid,-pid",
        "euid,-pid",
        "user,-pid",
        "uname,-pid",
        "euser,-pid",
        "gid,-pid",
        "egid,-pid",
        "group,-pid",
        "nlwp,-pid",
    ] {
        assert_eq!(sorted_pids(&processes, spec), [m, a, z], "{spec}");
    }
}

#[test]
fn command_arguments_and_scheduler_classes_follow_native_keys() {
    let processes = fixture_processes();
    let [z, a, m] = processes.each_ref().map(|process| process.pid);
    for spec in ["comm", "ucmd", "ucomm", "cls", "class", "policy"] {
        assert_eq!(sorted_pids(&processes, spec), [a, m, z], "{spec}");
    }
    assert_eq!(sorted_pids(&processes, "-comm"), [z, m, a]);
    for spec in ["args", "cmd", "command"] {
        assert_eq!(sorted_pids(&processes, spec), [z, m, a], "{spec}");
        assert_eq!(
            sorted_pids(&processes, &format!("-{spec}")),
            [a, m, z],
            "-{spec}"
        );
    }
}

#[test]
fn unsorted_and_true_key_ties_keep_pid_order() {
    let processes = fixture_processes();
    let mut expected: Vec<_> = processes.iter().map(|process| process.pid).collect();
    expected.sort_unstable();
    let reversed = processes
        .iter()
        .rev()
        .map(|process| process.pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(rows(&ps(&["-p", &reversed, "-o", "pid="])), expected);
    assert_eq!(
        rows(&ps(&["-p", &reversed, "-o", "pid=", "--sort", "uid"])),
        expected
    );
}

#[test]
fn bsd_k_attached_and_separate_values_share_sorting_grammar() {
    let processes = fixture_processes();
    let [z, a, m] = processes.each_ref().map(|process| process.pid);
    let pids = pid_list(&processes);
    for options in [
        &["k", "-rss,pid"][..],
        &["k-rss,pid"][..],
        &["axk-rss,pid"][..],
        &["-k", "-rss,pid"][..],
        &["--sort=-rss,pid"][..],
    ] {
        let mut args = vec!["-p", pids.as_str(), "-o", "pid="];
        args.extend_from_slice(options);
        // ax selects extra processes too: their global positions do not change
        // the relative ordering of the three controlled fixture processes.
        let controlled: Vec<_> = rows(&ps(&args))
            .into_iter()
            .filter(|pid| [z, a, m].contains(pid))
            .collect();
        assert_eq!(controlled, [a, m, z], "{options:?}");
    }
    for spec in [
        "pid,",
        "pid ",
        "pid\t",
        "pid\n",
        "pid rss",
        "pid\trss",
        "pid\nrss",
        "+pid,-rss",
        "pid,pid",
    ] {
        assert_eq!(sorted_pids(&processes, spec), [z, a, m], "{spec:?}");
    }
}

#[test]
fn malformed_unknown_and_repeated_sort_options_fail_before_any_cells() {
    let pid = std::process::id().to_string();
    for spec in [
        "",
        ",pid",
        "pid,,rss",
        "pid,,",
        "+",
        "-",
        "--pid",
        " pid",
        "pid  rss",
        "pid, rss",
        "PID",
        "unknown",
        "addr",
        "processor",
        "pid=label",
    ] {
        let output = ps(&["-p", &pid, "-o", "pid=", "--sort", spec]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{spec:?}: {:?}",
            output.stderr
        );
        assert!(output.stdout.is_empty(), "{spec:?}: {:?}", output.stdout);
        assert!(!output.stderr.is_empty(), "{spec:?}");
    }
    for options in [
        &["--sort", "pid", "--sort", "-rss"][..],
        &["--sort", "pid", "k-rss"][..],
        &["kpid", "k-pid"][..],
    ] {
        let mut args = vec!["-p", pid.as_str(), "-o", "pid="];
        args.extend_from_slice(options);
        let output = ps(&args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{options:?}: {:?}",
            output.stderr
        );
        assert!(output.stdout.is_empty(), "{options:?}");
        assert!(!output.stderr.is_empty(), "{options:?}");
    }
    for option in ["--sort", "k", "-k"] {
        let output = ps(&["-p", &pid, "-o", "pid=", option]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{option}: {:?}",
            output.stderr
        );
        assert!(output.stdout.is_empty(), "{option}");
        assert!(!output.stderr.is_empty(), "{option}");
    }
}

#[test]
fn explicit_flat_thread_sort_retains_every_real_task_globally() {
    let processes = fixture_processes();
    let pids = pid_list(&processes);
    let mut expected = Vec::new();
    for process in &processes {
        for task in procfs::process::Process::new(process.pid)
            .unwrap()
            .tasks()
            .unwrap()
        {
            expected.push((process.pid, task.unwrap().stat().unwrap().pid));
        }
    }
    assert!(
        expected.len() > processes.len(),
        "fixture must have real test-harness threads"
    );
    expected.sort_by_key(|&(_, tid)| std::cmp::Reverse(tid));
    for mode in ["-L", "-T", "H"] {
        let output = ps(&["-p", &pids, mode, "-o", "pid=,tid=", "--sort", "-tid"]);
        assert!(output.status.success(), "{mode}: {:?}", output.stderr);
        assert!(output.stderr.is_empty(), "{mode}: {:?}", output.stderr);
        let actual: Vec<(i32, i32)> = std::str::from_utf8(&output.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let mut cells = line.split_whitespace();
                let pid = cells.next().unwrap().parse().unwrap();
                let tid = cells.next().unwrap().parse().unwrap();
                assert!(cells.next().is_none());
                (pid, tid)
            })
            .collect();
        assert_eq!(actual, expected, "{mode}");
    }
}
