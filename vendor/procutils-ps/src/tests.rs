use super::*;
use procfs::process::{Stat, Status};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

const OWN: u32 = 1000;
const OTHER: u32 = 2000;
const TTY: i32 = 0x8800;

fn args(options: &[&str]) -> Args {
    let argv = std::iter::once("ps")
        .chain(options.iter().copied())
        .map(OsString::from)
        .collect();
    Args::try_parse_from(preprocess_argv(argv)).expect("valid ps arguments")
}

fn process_fixture(pid: i32, session: i32, tty: i32, ruid: u32, euid: u32) -> (Stat, Status) {
    // Linux stat fields 4 through 52, with the mandatory fields populated.
    let mut values = [0_u64; 49];
    values[0] = 1; // ppid
    values[1] = pid as u64; // pgrp
    values[2] = session as u64;
    values[3] = tty as u64;
    values[4] = pid as u64; // tpgid
    values[10] = 123; // utime
    values[11] = 77; // stime
    values[14] = 20; // priority
    values[16] = 1; // num_threads
    values[18] = 10_001; // starttime: newer than the test uptime snapshot
    values[19] = 1_048_576; // vsize
    values[20] = 32; // rss
    let tail = values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let stat = Stat::from_read(format!("{pid} (process-{pid}) S {tail}").as_bytes()).unwrap();
    let raw_status = format!(
        "Name:\tprocess-{pid}\nState:\tS (sleeping)\nTgid:\t{pid}\nPid:\t{pid}\n\
         PPid:\t1\nTracerPid:\t0\nUid:\t{ruid} {euid} {euid} {euid}\nGid:\t0 0 0 0\n\
         FDSize:\t64\nGroups:\t\nVmRSS:\t128 kB\nThreads:\t1\nSigQ:\t0/0\nSigPnd:\t0\nShdPnd:\t0\n\
         SigBlk:\t0\nSigIgn:\t0\nSigCgt:\t0\nCapInh:\t0\nCapPrm:\t0\nCapEff:\t0\n"
    );
    let status = Status::from_buf_read(raw_status.as_bytes()).unwrap();
    (stat, status)
}

fn system_fixture() -> Vec<(Stat, Status)> {
    vec![
        process_fixture(101, 101, TTY, OWN, OWN),
        process_fixture(102, 101, TTY, OWN, OWN),
        process_fixture(103, 101, TTY + 1, OWN, OWN),
        process_fixture(104, 104, 0, OWN, OWN),
        process_fixture(105, 101, TTY, OTHER, OTHER),
        process_fixture(106, 106, TTY + 1, OTHER, OTHER),
        process_fixture(107, 107, 0, OTHER, OTHER),
        process_fixture(108, 101, TTY, OWN, OTHER),
        process_fixture(109, 109, 0, OTHER, OWN),
    ]
}

fn selected(options: &[&str], my_tty: i32, processes: &[(Stat, Status)]) -> Vec<i32> {
    let selection = Selection::new(&args(options), OWN, my_tty).unwrap();
    processes
        .iter()
        .filter(|(stat, status)| selection.includes(stat, status))
        .map(|(stat, _)| stat.pid)
        .collect()
}

#[test]
fn unix_and_bsd_defaults_and_terminal_selectors() {
    let processes = system_fixture();
    let cases: &[(&[&str], &[i32])] = &[
        (&[], &[101, 102]),
        (&["-w"], &[101, 102]),
        (&["-ww"], &[101, 102]),
        (&["-w", "-w"], &[101, 102]),
        (&["u"], &[101, 102, 103]),
        (&["w"], &[101, 102, 103]),
        (&["ww"], &[101, 102, 103]),
        (&["a"], &[101, 102, 103, 105, 106, 108]),
        (&["x"], &[101, 102, 103, 104, 109]),
        (&["ax"], &[101, 102, 103, 104, 105, 106, 107, 108, 109]),
        (&["aux"], &[101, 102, 103, 104, 105, 106, 107, 108, 109]),
        (&["auxww"], &[101, 102, 103, 104, 105, 106, 107, 108, 109]),
        (&["-ax"], &[101, 102, 103, 104, 105, 106, 107, 108, 109]),
        (&["-au"], &[101, 102, 103, 105, 106, 108]),
        (&["-a"], &[102, 103, 105, 108]),
        (&["-x"], &[101, 102, 103, 104, 109]),
        (&["-e"], &[101, 102, 103, 104, 105, 106, 107, 108, 109]),
        (&["-A"], &[101, 102, 103, 104, 105, 106, 107, 108, 109]),
        (&["-ef"], &[101, 102, 103, 104, 105, 106, 107, 108, 109]),
        (&["-au", "1000"], &[101, 102, 103, 104, 105, 108, 109]),
    ];
    for &(options, expected) in cases {
        assert_eq!(selected(options, TTY, &processes), expected, "{options:?}");
    }
    assert_eq!(selected(&[], 0, &processes), [104, 109]);
    assert_eq!(selected(&["-w"], 0, &processes), [104, 109]);
    assert_eq!(selected(&["u"], 0, &processes), [101, 102, 103]);
}

#[test]
fn bsd_t_adds_current_terminal_without_narrowing_ax() {
    let processes = system_fixture();
    assert_eq!(selected(&["T"], TTY, &processes), [101, 102, 105, 108]);
    assert_eq!(selected(&["T"], 0, &processes), [104, 107, 109]);
    assert_eq!(
        selected(&["T", "-p", "107"], TTY, &processes),
        [101, 102, 105, 107, 108]
    );
    assert_eq!(
        selected(&["T", "-p", "101"], 0, &processes),
        [101, 104, 107, 109]
    );
    assert_eq!(selected(&["-u", "-p", "103"], TTY, &processes), [103]);
    for tty in [TTY, 0] {
        assert_eq!(
            selected(&["axT"], tty, &processes),
            [101, 102, 103, 104, 105, 106, 107, 108, 109]
        );
    }
}

#[test]
fn real_and_effective_uid_lists_are_distinct_and_additive() {
    let processes = system_fixture();
    assert_eq!(
        selected(&["-U", "1000"], TTY, &processes),
        [101, 102, 103, 104, 108]
    );
    assert_eq!(
        selected(&["-u", "1000"], TTY, &processes),
        [101, 102, 103, 104, 109]
    );
    assert_eq!(
        selected(&["--user=1000"], TTY, &processes),
        [101, 102, 103, 104, 109]
    );
    assert_eq!(
        selected(&["-U", "1000", "-u", "2000"], TTY, &processes),
        [101, 102, 103, 104, 105, 106, 107, 108]
    );
    for options in [["-U", "1000, 2000"], ["-u", "1000 2000"]] {
        assert_eq!(
            selected(&options, TTY, &processes),
            [101, 102, 103, 104, 105, 106, 107, 108, 109]
        );
    }
}

#[test]
fn user_names_and_numeric_ids_share_list_syntax() {
    let name = procutils_common::uid::uid_to_name(0).expect("system UID 0 has a name");
    let values = format!("{name}, 1000");
    let processes = vec![
        process_fixture(1, 1, 0, 0, OTHER),
        process_fixture(2, 2, 0, OTHER, 0),
        process_fixture(3, 3, 0, OWN, OWN),
    ];
    assert_eq!(selected(&["-U", &values], 0, &processes), [1, 3]);
    assert_eq!(selected(&["-u", &values], 0, &processes), [2, 3]);
}

#[test]
fn pid_lists_override_broad_selectors_but_union_with_users() {
    let processes = system_fixture();
    for option in ["-e", "-A", "a", "ax", "aux"] {
        assert_eq!(
            selected(&[option, "-p", "106"], TTY, &processes),
            [106],
            "{option}"
        );
    }
    assert_eq!(
        selected(&["-p", "106", "-U", "1000"], TTY, &processes),
        [101, 102, 103, 104, 106, 108]
    );
    assert_eq!(
        selected(&["-p", "106", "-u", "1000"], TTY, &processes),
        [101, 102, 103, 104, 106, 109]
    );
    assert_eq!(
        selected(&["-p", "106", "-p", "107 109"], TTY, &processes),
        [106, 107, 109]
    );
    assert_eq!(
        selected(&["-e", "-U", "1000"], TTY, &processes),
        [101, 102, 103, 104, 105, 106, 107, 108, 109]
    );
}

#[test]
fn running_only_applies_after_pid_and_user_selection() {
    let mut processes = system_fixture();
    processes[1].0.state = 'R';
    processes[2].0.state = 'D';
    processes[5].0.state = 'R';
    assert_eq!(selected(&["r"], TTY, &processes), [102, 103]);
    assert_eq!(selected(&["-p", "104,106", "r"], TTY, &processes), [106]);
    assert_eq!(
        selected(&["-p", "104", "-U", "1000", "r"], TTY, &processes),
        [102, 103]
    );
    assert!(selected(&["-p", "104", "r"], TTY, &processes).is_empty());
}

#[test]
fn option_like_values_are_not_reinterpreted_as_bsd_flags() {
    for options in [["-U", "ax"], ["-u", "aux"], ["-p", "ww"]] {
        let parsed = args(&options);
        assert!(Selection::new(&parsed, OWN, TTY).is_err(), "{options:?}");
    }
    let parsed = args(&["-o", "pid=aux,comm=w"]);
    let specs = parse_format_spec(&parsed.format).unwrap();
    let mut rendered = Vec::new();
    output::write_table(&specs, &[], None, &mut rendered).unwrap();
    assert_eq!(
        String::from_utf8(rendered)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["aux", "w"]
    );
    let raw_value = OsString::from_vec(vec![b'a', b'x', 0xff]);
    let input = vec![OsString::from("ps"), OsString::from("-o"), raw_value];
    let normalized = preprocess_argv(input);
    assert_eq!(normalized[2].as_bytes(), [b'a', b'x', 0xff]);
    assert!(Args::try_parse_from(normalized).is_err());
}

fn render_process(specs: &[FieldSpec], stat: &Stat, status: &Status, uptime: f64) -> String {
    render_process_with_memory(specs, stat, status, uptime, 1024)
}

fn render_process_with_memory(
    specs: &[FieldSpec],
    stat: &Stat,
    status: &Status,
    uptime: f64,
    total_mem_kb: u64,
) -> String {
    let mut uid_cache = procutils_common::uid::UidCache::new();
    let mut gid_cache = HashMap::new();
    let mut process = ProcessContext {
        stat,
        status,
        cmdline: "program --flag",
        uid_cache: &mut uid_cache,
        gid_cache: &mut gid_cache,
        boot_time: 0,
        uptime_secs: uptime,
        tps: 100,
        total_mem_kb,
    };
    let cells = specs
        .iter()
        .map(|spec| (spec.field.compute)(&mut process))
        .collect();
    let mut output = Vec::new();
    output::write_table(specs, &[(stat.pid, cells)], None, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

#[test]
fn newborn_process_cpu_usage_uses_snapshot_ticks_without_underflow() {
    let (mut stat, status) = process_fixture(42, 42, TTY, OWN, OWN);
    stat.utime = 1;
    stat.stime = 0;
    let specs = parse_format_spec(&["pid=,%cpu=,c=,etime=,etimes=".into()]).unwrap();
    for starttime in [9_999, 10_000, 10_001] {
        stat.starttime = starttime;
        // The fractional boot tick is discarded by procps before subtraction.
        let output = render_process(&specs, &stat, &status, 100.009);
        let expected = if starttime < 10_000 {
            ["42", "100", "99", "00:00", "0"]
        } else {
            ["42", "0.0", "0", "00:00", "0"]
        };
        assert_eq!(output.split_whitespace().collect::<Vec<_>>(), expected);
    }
}

#[test]
fn cpu_columns_truncate_quantization_boundaries_without_capping_multicore_usage() {
    let (mut stat, status) = process_fixture(42, 42, TTY, OWN, OWN);
    stat.starttime = 10_000;
    stat.stime = 0;
    let specs = parse_format_spec(&["pid=,%cpu=,c=".into()]).unwrap();
    for (ticks, percent, integer_percent) in [
        (5, "0.0", "0"),
        (10, "0.1", "0"),
        (99, "0.9", "0"),
        (100, "1.0", "1"),
        (1235, "12.3", "12"),
        (1240, "12.4", "12"),
        (1299, "12.9", "12"),
        (1300, "13.0", "13"),
        (9999, "99.9", "99"),
        (10_000, "100", "99"),
        (12_345, "123", "99"),
        (20_000, "200", "99"),
    ] {
        stat.utime = ticks;
        let output = render_process(&specs, &stat, &status, 200.0);
        assert_eq!(
            output.split_whitespace().collect::<Vec<_>>(),
            ["42", percent, integer_percent],
            "CPU ticks: {ticks}",
        );
    }
}

#[test]
fn long_lived_cpu_usage_matches_procps_single_precision_quantization() {
    let (mut stat, status) = process_fixture(42, 42, TTY, OWN, OWN);
    stat.starttime = 0;
    stat.utime = 16_777_217;
    stat.stime = 0;
    let specs = parse_format_spec(&["pid=,%cpu=,c=".into()]).unwrap();
    // A double-only ratio is exactly 0.1%; libproc2's float multiplication
    // puts this just below the tenth-percent boundary.
    let output = render_process(&specs, &stat, &status, 167_772_170.0);
    assert_eq!(
        output.split_whitespace().collect::<Vec<_>>(),
        ["42", "0.0", "0"],
    );
}

#[test]
fn memory_columns_use_status_kib_and_truncate_percentages() {
    let (mut stat, mut status) = process_fixture(42, 42, TTY, OWN, OWN);
    stat.rss = 4096;
    let specs = parse_format_spec(&["pid=,rss=,rsz=,%mem=,pmem=".into()]).unwrap();
    for (rss_kb, percent) in [
        (1, "0.0"),
        (2, "0.1"),
        (128, "12.5"),
        (129, "12.5"),
        (1023, "99.9"),
        (1024, "99.9"),
        (2048, "99.9"),
    ] {
        status.vmrss = Some(rss_kb);
        let output = render_process(&specs, &stat, &status, 100.0);
        let rss = rss_kb.to_string();
        assert_eq!(
            output.split_whitespace().collect::<Vec<_>>(),
            ["42", rss.as_str(), rss.as_str(), percent, percent],
            "VmRSS: {rss_kb} KiB",
        );
    }

    status.vmrss = Some(32 * 1024 * 1024);
    let output = render_process_with_memory(&specs, &stat, &status, 100.0, 64 * 1024 * 1024);
    assert_eq!(
        output.split_whitespace().collect::<Vec<_>>(),
        ["42", "33554432", "33554432", "50.0", "50.0"],
    );
}

#[test]
fn memory_columns_report_zero_for_missing_status_memory_without_stat_fallback() {
    let (mut stat, mut status) = process_fixture(42, 42, TTY, OWN, OWN);
    stat.rss = 4096;
    status.vmrss = None;
    let specs = parse_format_spec(&["pid=,rss=,%mem=".into()]).unwrap();
    for state in ['S', 'Z', 'I'] {
        stat.state = state;
        let output = render_process(&specs, &stat, &status, 100.0);
        assert_eq!(
            output.split_whitespace().collect::<Vec<_>>(),
            ["42", "0", "0.0"],
            "process state: {state}",
        );
    }

    status.vmrss = Some(128);
    let output = render_process_with_memory(&specs, &stat, &status, 100.0, 0);
    assert_eq!(
        output.split_whitespace().collect::<Vec<_>>(),
        ["42", "128", "0.0"],
    );
}

#[test]
fn priority_columns_match_procps_for_positive_negative_nice_and_realtime() {
    let (mut stat, status) = process_fixture(42, 42, TTY, OWN, OWN);
    let specs = parse_format_spec(&["pid=,pri=,ni=".into()]).unwrap();
    for (nice, kernel_priority, priority) in [
        (-20, 0, "39"),
        (-5, 15, "24"),
        (0, 20, "19"),
        (5, 25, "14"),
        (19, 39, "0"),
        (0, -51, "90"),
    ] {
        stat.nice = nice;
        stat.priority = kernel_priority;
        let output = render_process(&specs, &stat, &status, 100.0);
        let nice = nice.to_string();
        assert_eq!(
            output.split_whitespace().collect::<Vec<_>>(),
            ["42", priority, nice.as_str()],
        );
    }
}

#[test]
fn flag_columns_mask_unrelated_kernel_bits() {
    let (mut stat, status) = process_fixture(42, 42, TTY, OWN, OWN);
    let specs = parse_format_spec(&["pid=,flags=,f=".into()]).unwrap();
    for legacy_bits in 0..=7 {
        stat.flags = (legacy_bits << 6) | 0x8000_023f;
        let output = render_process(&specs, &stat, &status, 100.0);
        let flags = legacy_bits.to_string();
        assert_eq!(
            output.split_whitespace().collect::<Vec<_>>(),
            ["42", flags.as_str(), flags.as_str()],
        );
    }
}

#[test]
fn elapsed_columns_subtract_ticks_before_truncating_seconds() {
    let (mut stat, status) = process_fixture(42, 42, TTY, OWN, OWN);
    let specs = parse_format_spec(&["pid=,etime=,etimes=".into()]).unwrap();
    for (starttime, uptime, elapsed, seconds) in [
        (9990, 100.01, "00:00", "0"),
        (9990, 100.89, "00:00", "0"),
        (9990, 100.90, "00:01", "1"),
        (9991, 100.90, "00:00", "0"),
        (9990, 101.0, "00:01", "1"),
        (50, 60.49, "00:59", "59"),
        (50, 60.50, "01:00", "60"),
        (50, 3600.49, "59:59", "3599"),
        (50, 3600.50, "01:00:00", "3600"),
        (50, 86400.49, "23:59:59", "86399"),
        (50, 86400.50, "1-00:00:00", "86400"),
    ] {
        stat.starttime = starttime;
        let output = render_process(&specs, &stat, &status, uptime);
        assert_eq!(
            output.split_whitespace().collect::<Vec<_>>(),
            ["42", elapsed, seconds],
            "start tick: {starttime}, uptime: {uptime}",
        );
    }
}

#[test]
fn fixed_user_format_uses_truncated_percentages_and_status_rss() {
    let (mut stat, mut status) = process_fixture(42, 42, TTY, u32::MAX, u32::MAX);
    stat.starttime = 10_000;
    stat.utime = 1235;
    stat.stime = 0;
    status.vmrss = Some(129);
    let selection = Selection::new(&args(&["u"]), u32::MAX, TTY).unwrap();
    let output = render_process(&fixed_format(&selection, false), &stat, &status, 200.0);
    let start = format_helpers::format_start_compact(stat.starttime, 0, 100);
    assert_eq!(
        output
            .lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        [
            "4294967295",
            "42",
            "12.3",
            "12.5",
            "1024",
            "129",
            "pts/0",
            "Ss+",
            &start,
            "0:12",
            "program",
            "--flag",
        ],
    );
}

#[test]
fn full_format_renders_effective_user_order_and_three_part_time() {
    let (mut stat, status) = process_fixture(42, 42, TTY, u32::MAX - 1, u32::MAX);
    stat.starttime = 10_000;
    stat.utime = 1299;
    stat.stime = 0;
    let parsed = args(&["-f"]);
    let selection = Selection::new(&parsed, u32::MAX, TTY).unwrap();
    let output = render_process(&fixed_format(&selection, true), &stat, &status, 200.0);
    let mut lines = output.lines();
    assert_eq!(
        lines.next().unwrap().split_whitespace().collect::<Vec<_>>(),
        ["UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD"]
    );
    let start = format_helpers::format_start_compact(stat.starttime, 0, 100);
    let expected = [
        "4294967295",
        "42",
        "1",
        "12",
        &start,
        "pts/0",
        "00:00:12",
        "program",
        "--flag",
    ];
    assert_eq!(
        lines.next().unwrap().split_whitespace().collect::<Vec<_>>(),
        expected
    );
    assert!(lines.next().is_none());
}
