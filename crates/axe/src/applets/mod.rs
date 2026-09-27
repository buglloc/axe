pub(crate) mod admin_coreutils;
mod awk;
mod commands;
mod compression;
mod diffutils;
#[cfg(target_os = "linux")]
mod dns;
mod doctor;
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

pub use awk::awk;
pub use commands::commands;
pub use compression::{bzip2, gzip, xz};
pub use diffutils::{cmp, diff, diff3};
#[cfg(target_os = "linux")]
pub use dns::{host, nslookup};
pub use doctor::{doctor, doctor_builtin};
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
    free, hugetop, pgrep, pidof, pidwait, pkill, pmap, ps, pwdx, skill, slabtop, snice, sysctl,
    tload, top, vmstat, w, watch,
};
pub use sshd::sshd;
pub use strings::strings;
pub use tar::tar;
pub use tree::tree;
pub use which::which;
pub fn vzik(args: Vec<std::ffi::OsString>) -> i32 {
    ::vzik::entry(args)
}

pub fn grep(args: Vec<OsString>) -> i32 {
    uu_grep::uumain(args.into_iter())
}

pub fn sed(args: Vec<OsString>) -> i32 {
    use clap::Parser;

    let processed = args.into_iter().map(|arg| {
        let Some(text) = arg.to_str() else {
            return arg;
        };
        if text == "-i" {
            OsString::from("--in-place")
        } else if let Some(suffix) = text.strip_prefix("-i").filter(|_| !text.starts_with("--")) {
            OsString::from(format!("--in-place={suffix}"))
        } else {
            arg
        }
    });

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

    if script.is_empty() {
        eprintln!("sed: empty script");
        return 2;
    }

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
        Ok(()) => 0,
        Err(error) => {
            eprintln!("sed: {error}");
            2
        }
    }
}
