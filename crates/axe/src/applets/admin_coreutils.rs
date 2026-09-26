use clap::Command;
use clap::error::ErrorKind;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;

pub type AppletFn = fn(Vec<OsString>) -> i32;

fn prepare(util_crate_name: &str) -> Result<(), i32> {
    uucore::panic::preserve_inherited_sigpipe();
    uucore::panic::mute_sigpipe_panic();

    let name = uucore::get_canonical_util_name(util_crate_name);
    uucore::locale::setup_localization(name).map_err(|error| {
        eprintln!("axe: could not initialize localization for '{name}': {error}");
        99
    })
}

fn finish() {
    if let Err(error) = std::io::stdout().flush() {
        eprintln!("axe: error flushing stdout: {error}");
    }
}

macro_rules! register {
    ($commands:expr, $name:literal, $util:ident) => {{
        fn adapter(args: Vec<OsString>) -> i32 {
            if let Err(code) = prepare(stringify!($util)) {
                return code;
            }

            let code = $util::uumain(args.into_iter());
            finish();
            code
        }
        $commands.insert($name.to_string(), adapter as AppletFn);
    }};
}

#[cfg(unix)]
fn resolved_or_numeric(id: u32, resolve: impl FnOnce(u32) -> std::io::Result<String>) -> String {
    resolve(id).unwrap_or_else(|_| id.to_string())
}

#[cfg(unix)]
fn getuid() -> u32 {
    // SAFETY: getuid has no arguments or caller-side safety requirements.
    unsafe { libc::getuid() }
}

#[cfg(unix)]
fn geteuid() -> u32 {
    // SAFETY: geteuid has no arguments or caller-side safety requirements.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn getgid() -> u32 {
    // SAFETY: getgid has no arguments or caller-side safety requirements.
    unsafe { libc::getgid() }
}

#[cfg(unix)]
fn getegid() -> u32 {
    // SAFETY: getegid has no arguments or caller-side safety requirements.
    unsafe { libc::getegid() }
}

#[cfg(unix)]
fn print_current_id(args: &[OsString]) -> Option<std::io::Result<()>> {
    use uucore::entries::{get_groups_gnu, gid2grp, uid2usr};

    let matches = uu_id::uu_app().try_get_matches_from(args.iter()).ok()?;
    if matches.get_many::<String>("USER").is_some()
        || matches.get_flag("audit")
        || matches.get_flag("context")
        || matches.get_flag("human-readable")
        || matches.get_flag("password")
    {
        return None;
    }

    let print_user = matches.get_flag("user");
    let print_group = matches.get_flag("group");
    let print_groups = matches.get_flag("groups");
    let names = matches.get_flag("name");
    let real = matches.get_flag("real");
    let zero = matches.get_flag("zero");
    if !(print_user || print_group || print_groups) && (names || real || zero) {
        return None;
    }

    let uid = if real { getuid() } else { geteuid() };
    let gid = if real { getgid() } else { getegid() };
    let line_ending = if zero { "\0" } else { "\n" };
    let mut stdout = std::io::stdout().lock();

    if print_user {
        let user = if names {
            resolved_or_numeric(uid, uid2usr)
        } else {
            uid.to_string()
        };
        return Some(write!(stdout, "{user}{line_ending}"));
    }
    if print_group {
        let group = if names {
            resolved_or_numeric(gid, gid2grp)
        } else {
            gid.to_string()
        };
        return Some(write!(stdout, "{group}{line_ending}"));
    }

    let group_anchor = if print_groups { gid } else { getgid() };
    let groups = match get_groups_gnu(Some(group_anchor)) {
        Ok(groups) => groups,
        Err(error) => return Some(Err(error)),
    };
    if print_groups {
        let delimiter = if zero { "\0" } else { " " };
        for (index, group) in groups.into_iter().enumerate() {
            if index != 0
                && let Err(error) = write!(stdout, "{delimiter}")
            {
                return Some(Err(error));
            }
            let group = if names {
                resolved_or_numeric(group, gid2grp)
            } else {
                group.to_string()
            };
            if let Err(error) = write!(stdout, "{group}") {
                return Some(Err(error));
            }
        }
        return Some(write!(stdout, "{line_ending}"));
    }

    let real_uid = getuid();
    let real_gid = getgid();
    let effective_uid = geteuid();
    let effective_gid = getegid();
    if let Err(error) = write!(
        stdout,
        "uid={real_uid}({}) gid={real_gid}({})",
        resolved_or_numeric(real_uid, uid2usr),
        resolved_or_numeric(real_gid, gid2grp)
    ) {
        return Some(Err(error));
    }
    if effective_uid != real_uid
        && let Err(error) = write!(
            stdout,
            " euid={effective_uid}({})",
            resolved_or_numeric(effective_uid, uid2usr)
        )
    {
        return Some(Err(error));
    }
    if effective_gid != real_gid
        && let Err(error) = write!(
            stdout,
            " egid={effective_gid}({})",
            resolved_or_numeric(effective_gid, gid2grp)
        )
    {
        return Some(Err(error));
    }
    if let Err(error) = write!(stdout, " groups=") {
        return Some(Err(error));
    }
    for (index, group) in groups.into_iter().enumerate() {
        if index != 0
            && let Err(error) = write!(stdout, ",")
        {
            return Some(Err(error));
        }
        if let Err(error) = write!(stdout, "{group}({})", resolved_or_numeric(group, gid2grp)) {
            return Some(Err(error));
        }
    }
    Some(writeln!(stdout))
}

#[cfg(unix)]
fn id(args: Vec<OsString>) -> i32 {
    if let Err(code) = prepare("uu_id") {
        return code;
    }

    let code = match print_current_id(&args) {
        Some(Ok(())) => 0,
        Some(Err(error)) => {
            eprintln!("id: {error}");
            1
        }
        None => uu_id::uumain(args.into_iter()),
    };
    finish();
    code
}

#[cfg(unix)]
fn print_current_groups() -> std::io::Result<()> {
    use uucore::entries::{get_groups_gnu, gid2grp};

    let groups = get_groups_gnu(None)?;
    let mut stdout = std::io::stdout().lock();

    for (index, group) in groups.into_iter().enumerate() {
        if index != 0 {
            write!(stdout, " ")?;
        }
        write!(stdout, "{}", resolved_or_numeric(group, gid2grp))?;
    }
    writeln!(stdout)
}

#[cfg(unix)]
fn groups(args: Vec<OsString>) -> i32 {
    if let Err(code) = prepare("uu_groups") {
        return code;
    }

    let code = if args.len() == 1 {
        print_current_groups().map_or_else(
            |error| {
                eprintln!("groups: {error}");
                1
            },
            |()| 0,
        )
    } else {
        uu_groups::uumain(args.into_iter())
    };
    finish();
    code
}

fn timeout(mut args: Vec<OsString>) -> i32 {
    if let Err(code) = prepare("uu_timeout") {
        return code;
    }

    if let Err(error) = rewrite_child(
        &mut args,
        uu_timeout::uu_app(),
        uu_timeout::options::COMMAND,
    ) {
        eprintln!("timeout: {error}");
        return 126;
    }
    let code = uu_timeout::uumain(args.into_iter());
    finish();
    code
}

fn nohup(mut args: Vec<OsString>) -> i32 {
    if let Err(code) = prepare("uu_nohup") {
        return code;
    }

    if let Err(error) = rewrite_child(&mut args, uu_nohup::uu_app(), "cmd") {
        eprintln!("nohup: {error}");
        return 126;
    }

    let code = uu_nohup::uumain(args.into_iter());
    finish();
    code
}

fn bracket(mut args: Vec<OsString>) -> i32 {
    if let Err(code) = prepare("uu_test") {
        return code;
    }

    if args.len() == 2 && (args[1] == OsStr::new("--help") || args[1] == OsStr::new("--version")) {
        let code = match uu_test::uu_app().try_get_matches_from(args) {
            Ok(_) => 0,
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
                code
            }
        };
        finish();
        return code;
    }

    if args.pop().as_deref() != Some(OsStr::new("]")) {
        eprintln!("[: missing ']'");
        return 2;
    }
    args[0] = OsString::from("test");
    let code = uu_test::uumain(args.into_iter());
    finish();
    code
}

fn rewrite_child(
    args: &mut Vec<OsString>,
    command: Command,
    argument_id: &str,
) -> std::io::Result<()> {
    if let Ok(matches) = command.try_get_matches_from(args.clone())
        && let Some(command_index) = matches
            .indices_of(argument_id)
            .and_then(|mut indices| indices.next())
    {
        crate::child::rewrite(args, command_index)?;
    }
    Ok(())
}

pub fn commands() -> HashMap<String, AppletFn> {
    let mut commands = HashMap::new();

    commands.insert("[".to_string(), bracket as AppletFn);
    register!(commands, "chgrp", uu_chgrp);
    register!(commands, "chmod", uu_chmod);
    register!(commands, "chown", uu_chown);
    #[cfg(unix)]
    register!(commands, "chroot", uu_chroot);
    #[cfg(unix)]
    commands.insert("groups".to_string(), groups as AppletFn);
    #[cfg(not(unix))]
    register!(commands, "groups", uu_groups);
    #[cfg(unix)]
    register!(commands, "hostid", uu_hostid);
    #[cfg(unix)]
    commands.insert("id".to_string(), id as AppletFn);
    #[cfg(not(unix))]
    register!(commands, "id", uu_id);
    #[cfg(unix)]
    register!(commands, "install", uu_install);
    register!(commands, "kill", uu_kill);
    register!(commands, "logname", uu_logname);
    register!(commands, "mkfifo", uu_mkfifo);
    register!(commands, "mknod", uu_mknod);
    #[cfg(unix)]
    register!(commands, "nice", uu_nice);
    commands.insert("nohup".to_string(), nohup as AppletFn);
    register!(commands, "pathchk", uu_pathchk);
    #[cfg(unix)]
    register!(commands, "pinky", uu_pinky);
    register!(commands, "stat", uu_stat);
    register!(commands, "stty", uu_stty);
    commands.insert("timeout".to_string(), timeout as AppletFn);
    register!(commands, "tty", uu_tty);
    #[cfg(unix)]
    register!(commands, "uptime", uu_uptime);
    #[cfg(unix)]
    register!(commands, "users", uu_users);
    #[cfg(unix)]
    register!(commands, "who", uu_who);
    commands
}
