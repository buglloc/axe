#[cfg(feature = "applet-admin-coreutils")]
pub(crate) mod admin_coreutils;
#[cfg(feature = "applet-awk")]
mod awk;
mod commands;
#[cfg(feature = "applet-compression")]
mod compression;
#[cfg(feature = "applet-diffutils")]
mod diffutils;
#[cfg(all(feature = "applet-linux-network", target_os = "linux"))]
mod dns;
mod doctor;
#[cfg(feature = "applet-file")]
mod file;
#[cfg(feature = "applet-findutils")]
mod findutils;
#[cfg(feature = "applet-goblin")]
mod goblin;
#[cfg(feature = "applet-gzip")]
mod gzip;
#[cfg(feature = "applet-http")]
mod http;
#[cfg(all(feature = "applet-inotify", target_os = "linux"))]
mod inotify_tools;
#[cfg(feature = "applet-jq")]
mod jq;
#[cfg(all(feature = "applet-linux-inspect", target_os = "linux"))]
pub(crate) mod linux_inspect;
#[cfg(all(feature = "applet-linux-network", target_os = "linux"))]
pub(crate) mod linux_network;
#[cfg(all(feature = "applet-linux-storage", target_os = "linux"))]
pub(crate) mod linux_storage;
#[cfg(all(feature = "applet-linux-network", target_os = "linux"))]
mod ping;
#[cfg(all(feature = "applet-procutils", target_os = "linux"))]
mod procutils;
#[cfg(feature = "applet-daemons")]
pub(crate) mod relay;
#[cfg(feature = "applet-daemons")]
pub(crate) mod sftp;
#[cfg(feature = "applet-daemons")]
mod sshd;
#[cfg(feature = "applet-strings")]
mod strings;
#[cfg(feature = "applet-tar")]
mod tar;
#[cfg(feature = "applet-tree")]
mod tree;
#[cfg(all(feature = "applet-util-linux", target_os = "linux"))]
pub(crate) mod util_linux;
#[cfg(feature = "applet-which")]
mod which;

#[cfg(any(feature = "applet-grep", feature = "applet-sed"))]
use std::ffi::OsString;

#[cfg(feature = "applet-awk")]
pub use awk::awk;
pub use commands::commands;
#[cfg(feature = "applet-compression")]
pub use compression::{bzip2, xz};
#[cfg(feature = "applet-diffutils")]
pub use diffutils::{cmp, diff, diff3};
#[cfg(all(feature = "applet-linux-network", target_os = "linux"))]
pub use dns::{host, nslookup};
pub use doctor::{doctor, doctor_builtin};
#[cfg(feature = "applet-file")]
pub use file::file;
#[cfg(feature = "applet-findutils")]
pub use findutils::{find, xargs};
#[cfg(feature = "applet-goblin")]
pub use goblin::goblin;
#[cfg(feature = "applet-gzip")]
pub use gzip::gzip;
#[cfg(feature = "applet-http")]
pub use http::http;
#[cfg(all(feature = "applet-inotify", target_os = "linux"))]
pub use inotify_tools::{inotifywait, inotifywatch};
#[cfg(feature = "applet-jq")]
pub use jq::jq;
#[cfg(all(feature = "applet-linux-network", target_os = "linux"))]
pub use ping::{ping, ping6, traceroute, traceroute6};
#[cfg(all(feature = "applet-procutils", target_os = "linux"))]
pub use procutils::{
    free, hugetop, pgrep, pidof, pidwait, pkill, pmap, ps, pwdx, skill, slabtop, snice, sysctl,
    tload, top, vmstat, w, watch,
};
#[cfg(feature = "applet-daemons")]
pub use sshd::sshd;
#[cfg(feature = "applet-strings")]
pub use strings::strings;
#[cfg(feature = "applet-tar")]
pub use tar::tar;
#[cfg(feature = "applet-tree")]
pub use tree::tree;
#[cfg(feature = "applet-which")]
pub use which::which;
#[cfg(feature = "applet-vzik")]
pub fn vzik(args: Vec<std::ffi::OsString>) -> i32 {
    ::vzik::entry(args)
}

#[cfg(feature = "applet-grep")]
pub fn grep(args: Vec<OsString>) -> i32 {
    uu_grep::uumain(args.into_iter())
}

#[cfg(feature = "applet-sed")]
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
