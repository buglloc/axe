//! AXE additions: keep BSD and Unix selection distinct in one backend selector.

use crate::Args;
use procfs::process::{Stat, Status};
use std::{
    collections::HashSet,
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
};

#[derive(Clone, Debug, Default)]
pub(super) struct BsdOptions {
    a: bool,
    x: bool,
    pub user_format: bool,
    running: bool,
    current_tty: bool,
    pub wide: u8,
    pub threads: bool,
    pub mixed_threads: bool,
}

fn is_bsd_option(byte: u8) -> bool {
    matches!(
        byte,
        b'a' | b'x' | b'u' | b'w' | b'r' | b'T' | b'H' | b'm' | b'k'
    )
}

pub(super) fn parse_bsd_options(value: &str) -> Result<BsdOptions, String> {
    let mut options = BsdOptions::default();
    if value.is_empty() {
        return Err("empty BSD option list".into());
    }
    for byte in value.bytes() {
        match byte {
            b'a' => options.a = true,
            b'x' => options.x = true,
            b'u' => options.user_format = true,
            b'r' => options.running = true,
            b'T' => options.current_tty = true,
            b'w' => options.wide = options.wide.saturating_add(1).min(2),
            b'H' => options.threads = true,
            b'm' => options.mixed_threads = true,
            b'k' => {}
            _ => return Err(format!("unsupported BSD option: {}", char::from(byte))),
        }
    }
    Ok(options)
}

fn bsd_argument(options: &[u8]) -> OsString {
    let mut bytes = Vec::with_capacity(b"--bsd-options=".len() + options.len());
    bytes.extend_from_slice(b"--bsd-options=");
    bytes.extend_from_slice(options);
    OsString::from_vec(bytes)
}

fn short_takes_next_value(options: &[u8]) -> bool {
    for (index, byte) in options.iter().enumerate() {
        if matches!(*byte, b'p' | b'o' | b'U' | b'u' | b'k') {
            return index + 1 == options.len();
        }
    }
    false
}

/// Normalize conventional bare BSD options before parsing [`Args`].
///
/// The vector includes argv[0]. Option values and non-UTF-8 arguments are
/// preserved byte-for-byte. The hidden `--bsd-options` argument records only
/// option style; process enumeration and selection happen entirely in `run`.
pub fn preprocess_argv(args: Vec<OsString>) -> Vec<OsString> {
    let mut output = Vec::with_capacity(args.len());
    let mut input = args.into_iter().peekable();
    let Some(program) = input.next() else {
        return output;
    };
    output.push(program);

    let mut value_next = false;
    let mut end_options = false;
    while let Some(arg) = input.next() {
        let bytes = arg.as_bytes();
        if value_next || end_options {
            value_next = false;
            output.push(arg);
            continue;
        }
        if bytes == b"--" {
            end_options = true;
            output.push(arg);
            continue;
        }
        if !bytes.starts_with(b"-")
            && let Some(index) = bytes.iter().position(|&byte| byte == b'k')
            && bytes[..index].iter().copied().all(is_bsd_option)
        {
            output.push(bsd_argument(&bytes[..=index]));
            output.push(OsString::from("--sort"));
            if index + 1 == bytes.len() {
                value_next = true;
            } else {
                output.push(OsString::from_vec(bytes[index + 1..].to_vec()));
            }
            continue;
        }
        if !bytes.is_empty() && bytes.iter().copied().all(is_bsd_option) {
            output.push(bsd_argument(bytes));
            continue;
        }
        if bytes.starts_with(b"--") {
            value_next = matches!(
                bytes,
                b"--pid"
                    | b"--user"
                    | b"--ruser"
                    | b"--format"
                    | b"--sort"
                    | b"--bsd-options"
                    | b"--cols"
                    | b"--columns"
                    | b"--width"
            );
            output.push(arg);
            continue;
        }
        if let Some(short) = bytes.strip_prefix(b"-") {
            // procps accepts -ax as its BSD ax fallback. -au falls back
            // only when no Unix -u value follows; -au USER remains Unix.
            let bsd_cluster = (short == b"u" || short.first() == Some(&b'a'))
                && short.iter().copied().all(is_bsd_option);
            let fallback = if bsd_cluster {
                if let Some(index) = short.iter().position(|&byte| byte == b'u') {
                    let user = &short[index + 1..];
                    if user.is_empty() {
                        input
                            .peek()
                            .is_none_or(|next| next.as_bytes().starts_with(b"-"))
                    } else {
                        // Preserve Unix -aux when the user named x exists.
                        // Otherwise retain the familiar BSD compatibility form.
                        std::str::from_utf8(user)
                            .ok()
                            .is_some_and(|name| procutils_common::uid::resolve_uid(name).is_none())
                    }
                } else {
                    short.contains(&b'x')
                }
            } else {
                false
            };
            if fallback {
                output.push(bsd_argument(short));
                continue;
            }
            value_next = short_takes_next_value(short);
        }
        output.push(arg);
    }
    output
}

fn list_items(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
}

fn resolve_uid_filter(users: &[String]) -> Result<HashSet<u32>, String> {
    let mut uids = HashSet::new();
    for raw in users {
        let mut found = false;
        for user in list_items(raw) {
            found = true;
            let uid = procutils_common::uid::resolve_uid(user)
                .ok_or_else(|| format!("ps: user name does not exist: {user}"))?;
            uids.insert(uid);
        }
        if !found {
            return Err("ps: empty user list".into());
        }
    }
    Ok(uids)
}

fn resolve_pid_filter(values: &[String]) -> Result<HashSet<i32>, String> {
    let mut pids = HashSet::new();
    for raw in values {
        let mut found = false;
        for value in list_items(raw) {
            found = true;
            let pid = value
                .parse::<i32>()
                .ok()
                .filter(|&pid| pid > 0)
                .ok_or_else(|| format!("ps: invalid process ID: {value}"))?;
            pids.insert(pid);
        }
        if !found {
            return Err("ps: empty process ID list".into());
        }
    }
    Ok(pids)
}

pub(super) struct Selection {
    all: bool,
    unix_a: bool,
    unix_x: bool,
    pub bsd: bool,
    pub user_format: bool,
    bsd_options: BsdOptions,
    running_only: bool,
    my_uid: u32,
    my_tty: i32,
    real_uids: HashSet<u32>,
    effective_uids: HashSet<u32>,
    pids: HashSet<i32>,
}

impl Selection {
    pub fn new(args: &Args, my_uid: u32, my_tty: i32) -> Result<Self, String> {
        let mut bsd_options = BsdOptions::default();
        for options in &args.bsd_options {
            bsd_options.a |= options.a;
            bsd_options.x |= options.x;
            bsd_options.user_format |= options.user_format;
            bsd_options.running |= options.running;
            bsd_options.current_tty |= options.current_tty;
            bsd_options.threads |= options.threads;
            bsd_options.mixed_threads |= options.mixed_threads;
        }
        Ok(Self {
            all: args.all || args.e,
            unix_a: args.a,
            unix_x: args.x,
            bsd: !args.bsd_options.is_empty(),
            user_format: bsd_options.user_format,
            running_only: args.r || bsd_options.running,
            bsd_options,
            my_uid,
            my_tty,
            real_uids: resolve_uid_filter(&args.real_user_filter)?,
            effective_uids: resolve_uid_filter(&args.user_filter)?,
            pids: resolve_pid_filter(&args.p)?,
        })
    }

    pub fn includes(&self, stat: &Stat, status: &Status) -> bool {
        self.selects(stat, status) && (!self.running_only || matches!(stat.state, 'R' | 'D'))
    }

    /// Group selection without the activity restriction. Flat thread modes
    /// apply activity to each task; mixed mode applies it to the summary.
    pub fn selects(&self, stat: &Stat, status: &Status) -> bool {
        let uid_selected =
            self.real_uids.contains(&status.ruid) || self.effective_uids.contains(&status.euid);
        let has_uids = !self.real_uids.is_empty() || !self.effective_uids.is_empty();
        let own = status.euid == self.my_uid;
        let tty_selected = self.bsd_options.current_tty && stat.tty_nr == self.my_tty;

        // Explicit PIDs replace broad selection, but PID and UID lists are
        // additive. In contrast, -e/-A with only UID filters still selects all.
        let selected = if !self.pids.is_empty() {
            self.pids.contains(&status.tgid) || uid_selected || tty_selected
        } else if self.all {
            true
        } else {
            let unix_selected = (self.unix_a && stat.tty_nr != 0 && status.tgid != stat.session)
                || (self.unix_x && own);
            let bsd_selected = match (self.bsd_options.a, self.bsd_options.x) {
                (true, true) => true,
                (true, false) => stat.tty_nr != 0,
                (false, true) => own,
                (false, false) => false,
            };
            let default_selected = !has_uids
                && !self.unix_a
                && !self.unix_x
                && !self.bsd_options.a
                && !self.bsd_options.x
                && !self.bsd_options.current_tty
                && own
                && if self.bsd {
                    stat.tty_nr != 0
                } else {
                    stat.tty_nr == self.my_tty
                };
            uid_selected || unix_selected || bsd_selected || tty_selected || default_selected
        };

        selected
    }
}
