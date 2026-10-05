use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

pub fn free(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_free::Args>(args, procutils_free::run)
}

pub fn killall(args: Vec<OsString>) -> i32 {
    let args = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let args = procutils_killall::preprocess_argv(args);
    parse_and_run::<procutils_killall::Args>(
        args.into_iter().map(OsString::from).collect(),
        procutils_killall::run,
    )
}

pub fn hugetop(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_hugetop::Args>(args, procutils_hugetop::run)
}

pub fn pgrep(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_pgrep::Args>(args, procutils_pgrep::run)
}

pub fn pmap(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_pmap::Args>(args, procutils_pmap::run)
}

pub fn pwdx(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_pwdx::Args>(args, procutils_pwdx::run)
}

pub fn skill(args: Vec<OsString>) -> i32 {
    let args = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let args = procutils_skill::preprocess_argv(args);
    parse_and_run::<procutils_skill::Args>(
        args.into_iter().map(OsString::from).collect(),
        procutils_skill::run,
    )
}

pub fn slabtop(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_slabtop::Args>(args, procutils_slabtop::run)
}

pub fn snice(args: Vec<OsString>) -> i32 {
    let args = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let args = procutils_snice::preprocess_argv(args);
    parse_and_run::<procutils_snice::Args>(
        args.into_iter().map(OsString::from).collect(),
        procutils_snice::run,
    )
}

pub fn sysctl(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_sysctl::Args>(args, procutils_sysctl::run)
}

pub fn tload(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_tload::Args>(args, procutils_tload::run)
}

pub fn top(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_top::Args>(args, procutils_top::run)
}

pub fn vmstat(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_vmstat::Args>(args, procutils_vmstat::run)
}

pub fn w(args: Vec<OsString>) -> i32 {
    parse_and_run::<procutils_w::Args>(args, procutils_w::run)
}

pub fn ps(args: Vec<OsString>) -> i32 {
    let args = procutils_ps::preprocess_argv(args);
    parse_and_run::<procutils_ps::Args>(args, procutils_ps::run)
}

pub fn pkill(args: Vec<OsString>) -> i32 {
    let args = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let args = procutils_pkill::preprocess_argv(args);
    parse_and_run::<procutils_pkill::Args>(
        args.into_iter().map(OsString::from).collect(),
        procutils_pkill::run,
    )
}

pub fn pidof(args: Vec<OsString>) -> i32 {
    run_uutil("uu_pidof", args, uu_pidof::uumain)
}

pub fn pidwait(args: Vec<OsString>) -> i32 {
    run_uutil("uu_pidwait", args, uu_pidwait::uumain)
}

pub fn watch(mut args: Vec<OsString>) -> i32 {
    if let Ok(matches) = procutils_watch::Args::command().try_get_matches_from(args.clone())
        && matches.get_flag("exec")
        && let Some(command_index) = matches
            .indices_of("command")
            .and_then(|mut indices| indices.next())
        && let Err(error) = crate::child::rewrite(&mut args, command_index)
    {
        eprintln!("watch: {error}");
        return 4;
    }
    parse_and_run::<procutils_watch::Args>(args, procutils_watch::run)
}

fn run_uutil(
    util_crate_name: &str,
    args: Vec<OsString>,
    run: impl FnOnce(std::vec::IntoIter<OsString>) -> i32,
) -> i32 {
    procps_uucore::panic::preserve_inherited_sigpipe();
    procps_uucore::panic::mute_sigpipe_panic();

    let name = procps_uucore::get_canonical_util_name(util_crate_name);
    if let Err(error) = procps_uucore::locale::setup_localization(name) {
        eprintln!("axe: could not initialize localization for '{name}': {error}");
        return 99;
    }

    let code = run(args.into_iter());
    if let Err(error) = std::io::stdout().flush() {
        eprintln!("axe: error flushing stdout: {error}");
    }
    code
}

fn parse_and_run<A: Parser>(args: Vec<OsString>, run: fn(A) -> ExitCode) -> i32 {
    let args = match A::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            let code = if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                0
            } else {
                2
            };
            let _ = error.print();
            return code;
        }
    };

    let status = run(args);
    if status == ExitCode::SUCCESS {
        0
    } else if status == ExitCode::from(2) {
        2
    } else if status == ExitCode::from(3) {
        3
    } else {
        1
    }
}
