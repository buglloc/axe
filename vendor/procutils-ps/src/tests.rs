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
         FDSize:\t64\nGroups:\t\nThreads:\t1\nSigQ:\t0/0\nSigPnd:\t0\nShdPnd:\t0\n\
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
    let table = table_with_format(&specs);
    let mut rendered = Vec::new();
    cols::print_table(&table, &mut rendered).unwrap();
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
        total_mem_kb: 1024,
        page_size: 4096,
    };
    let mut table = table_with_format(specs);
    let line = table.new_line(None);
    for (index, spec) in specs.iter().enumerate() {
        let cell = (spec.field.compute)(&mut process);
        table.line_mut(line).data_set(index, &cell);
    }
    let mut output = Vec::new();
    cols::print_table(&table, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

#[test]
fn newborn_process_cpu_usage_saturates_at_snapshot() {
    let (mut stat, status) = process_fixture(42, 42, TTY, OWN, OWN);
    for starttime in [9_999, 10_000, 10_001] {
        stat.starttime = starttime;
        let specs = parse_format_spec(&["pid=,%cpu=,c=".into()]).unwrap();
        let output = render_process(&specs, &stat, &status, 100.0);
        let expected = if starttime < 10_000 {
            ["42", "20000.0", "99"]
        } else {
            ["42", "0.0", "0"]
        };
        assert_eq!(output.split_whitespace().collect::<Vec<_>>(), expected);
    }
}

#[test]
fn full_format_renders_effective_user_order_and_three_part_time() {
    let (stat, status) = process_fixture(42, 42, TTY, u32::MAX - 1, u32::MAX);
    let parsed = args(&["-f"]);
    let selection = Selection::new(&parsed, u32::MAX, TTY).unwrap();
    let output = render_process(&fixed_format(&selection, true), &stat, &status, 100.0);
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
        "0",
        &start,
        "pts/0",
        "00:00:02",
        "program",
        "--flag",
    ];
    assert_eq!(
        lines.next().unwrap().split_whitespace().collect::<Vec<_>>(),
        expected
    );
    assert!(lines.next().is_none());
}
