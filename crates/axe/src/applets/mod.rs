pub(crate) mod admin_coreutils;
mod awk;
mod commands;
mod compression;
mod diffutils;
#[cfg(target_os = "linux")]
mod dns;
mod doctor;
pub(crate) mod errno;
#[cfg(target_os = "linux")]
mod fanotify_tools;
mod file;
mod findutils;
mod goblin;
mod http;
#[cfg(target_os = "linux")]
mod inotify_tools;
mod jq;
#[cfg(target_os = "linux")]
pub(crate) mod linux_inspect;
#[cfg(target_os = "linux")]
pub(crate) mod linux_network;
#[cfg(target_os = "linux")]
pub(crate) mod linux_storage;
#[cfg(target_os = "linux")]
mod mount;
#[cfg(target_os = "linux")]
mod ping;
#[cfg(target_os = "linux")]
mod procutils;
pub(crate) mod sftp;
mod sshd;
mod strings;
mod tar;
mod tree;
#[cfg(target_os = "linux")]
pub(crate) mod util_linux;
mod which;

use std::ffi::OsString;
use std::io::IsTerminal;

pub use awk::awk;
pub use commands::commands;
pub use compression::{bzip2, gzip, xz, zstd};
pub use diffutils::{cmp, diff, sdiff};
#[cfg(target_os = "linux")]
pub use dns::{host, nslookup};
pub use doctor::{doctor, doctor_builtin};
#[cfg(target_os = "linux")]
pub use fanotify_tools::{fsnotifywait, fsnotifywatch};
pub use file::file;
pub use findutils::{find, xargs};
pub use goblin::goblin;
pub use http::http;
#[cfg(target_os = "linux")]
pub use inotify_tools::{inotifywait, inotifywatch};
pub use jq::jq;
#[cfg(target_os = "linux")]
pub use ping::{ping, ping6, traceroute, traceroute6};
#[cfg(target_os = "linux")]
pub use procutils::{
    free, hugetop, killall, pgrep, pidof, pidwait, pkill, pmap, ps, pwdx, skill, slabtop, snice,
    sysctl, tload, top, vmstat, w, watch,
};
pub use sshd::sshd;
pub use strings::strings;
pub use tar::tar;
pub use tree::tree;
pub use which::which;
pub fn vzik(args: Vec<std::ffi::OsString>) -> i32 {
    ::vzik::entry(args)
}

pub(crate) fn in_process_io_status(result: std::io::Result<()>, error_status: i32) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => 141,
        Err(_) => error_status,
    }
}

pub fn grep(mut args: Vec<OsString>) -> i32 {
    if !args.is_empty()
        && std::io::stdout().is_terminal()
        && !args.iter().skip(1).any(|arg| arg == "--line-buffered")
    {
        args.insert(1, OsString::from("--line-buffered"));
    }
    uu_grep::uumain(args.into_iter())
}
pub fn sed(args: Vec<OsString>) -> i32 {
    use clap::Parser;

    let processed = sed_rs::cli::preprocess_args(args);

    let options = match sed_rs::cli::Options::try_parse_from(processed) {
        Ok(options) => options,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            return code;
        }
    };

    let (script, files) = match options.script_and_files() {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("sed: {error}");
            return 2;
        }
    };

    let commands = match sed_rs::command::parse(&script) {
        Ok(commands) => commands,
        Err(error) => {
            eprintln!("sed: {error}");
            return 2;
        }
    };

    let engine = match sed_rs::engine::Engine::new(commands, &options) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("sed: {error}");
            return 2;
        }
    };

    let result = if let Some(suffix) = &options.in_place {
        if files.is_empty() {
            eprintln!("sed: -i/--in-place requires at least one file argument");
            return 2;
        }
        engine.run_in_place(&files, suffix)
    } else {
        engine.run(&files)
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("sed: {error}");
            4
        }
    }
}
