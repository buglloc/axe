mod applets;
mod child;
mod executable;
mod ondemand;
#[cfg(unix)]
mod path_bridge;
mod registry;
mod tls_roots;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path};
use std::sync::Arc;

const HELP: &str = include_str!("help.txt");
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("AXE_BUILD_COMMIT"));

fn main() {
    // SAFETY: AXE has not initialized executable state or started any threads.
    unsafe {
        std::env::set_var("AXE", "true");
    }

    let mut raw = std::env::args_os();
    let argv0 = raw.next().unwrap_or_else(|| OsString::from("axe"));
    let args: Vec<OsString> = raw.collect();

    let executable = executable::initialize();
    if executable::is_relay_probe(&args) || executable::is_doctor_probe(&args) {
        return;
    }

    if args.len() == 1 && (args[0] == OsStr::new("--help") || args[0] == OsStr::new("-h")) {
        print!("{HELP}");
        return;
    }

    if args.len() == 1 && args[0] == OsStr::new("--version") {
        println!(
            "axe {VERSION} (edition {}; Brush {})",
            embedded::INPUTS.edition_id,
            brush_version()
        );
        return;
    }

    publish_shell_paths(&executable);

    let managed_applet = managed_applet_name(&argv0, &executable);
    let preferred_local = preferred_local_command(&argv0, &args, managed_applet.as_deref());
    let store_mode = match requested_store_mode(preferred_local.as_deref(), &args) {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("axe: {error}");
            std::process::exit(2);
        }
    };
    // SAFETY: mode selection runs before registry construction or any background threads.
    unsafe {
        std::env::set_var("AXE_STORE_MODE", store_mode.as_str());
    }

    let mode = if managed_applet.is_some()
        || args
            .first()
            .is_some_and(|arg| arg == brush_shell::bundled::DISPATCH_FLAG)
        || std::env::var_os(registry::INDEX_CACHE_ONLY_ENV).as_deref() == Some(OsStr::new("1"))
    {
        registry::BuildMode::CacheOnly
    } else {
        registry::BuildMode::RevalidateIfStale
    };
    let registry = match registry::build(mode, preferred_local.as_deref(), store_mode) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("axe: {error}");
            std::process::exit(2);
        }
    };

    if args.len() == 1 && args[0] == OsStr::new("--list") {
        if let Some(error) = &registry.blocking_index_error {
            eprintln!("axe: {error}");
            std::process::exit(126);
        }
        for name in &registry.visible_names {
            println!("{name}");
        }
        return;
    }

    let bundled_executable = Arc::clone(&executable);
    brush_shell::bundled::install_with_executable(
        registry.commands.clone(),
        Arc::new(move || bundled_executable.bundled_executable()),
    );

    if args
        .first()
        .is_some_and(|arg| arg == brush_shell::bundled::DISPATCH_FLAG)
    {
        let Some(name) = args.get(1) else {
            eprintln!("axe: bundled dispatch requires a command name");
            std::process::exit(2);
        };
        let Some(name) = name.to_str() else {
            eprintln!("axe: bundled command name is not valid UTF-8");
            std::process::exit(127);
        };
        std::process::exit(registry::invoke(
            &registry.commands,
            name,
            args.iter().skip(2).cloned(),
        ));
    }

    if let Some(name) = managed_applet {
        std::process::exit(registry::invoke(&registry.commands, &name, args));
    }

    if let Some(name) = busybox_name(&argv0, &registry.commands) {
        std::process::exit(registry::invoke(&registry.commands, name, args));
    }
    if args.first() == Some(&OsString::from("--applet")) {
        let Some(name) = args.get(1) else {
            eprintln!("axe: --applet requires a command name");
            std::process::exit(2);
        };
        let Some(name) = name.to_str() else {
            eprintln!("axe: applet name is not valid UTF-8");
            std::process::exit(127);
        };
        std::process::exit(registry::invoke(
            &registry.commands,
            name,
            applet_args(args.iter().skip(2).cloned()),
        ));
    }
    if let Some(name) = args.first().and_then(|arg| arg.to_str())
        && registry.commands.contains_key(name)
    {
        std::process::exit(registry::invoke(
            &registry.commands,
            name,
            applet_args(args.iter().skip(1).cloned()),
        ));
    }

    if let Some(error) = &registry.blocking_index_error
        && preferred_local
            .as_deref()
            .is_some_and(|name| !registry.commands.contains_key(name))
    {
        eprintln!("axe: {error}");
        std::process::exit(126);
    }

    #[cfg(unix)]
    {
        let bridge_path = executable.bridge_path();
        if let Err(error) = path_bridge::install(
            &registry.visible_names,
            bridge_path.as_deref(),
            executable.runtime_root(),
        ) {
            eprintln!("axe: applet PATH bridge unavailable: {error}");
        }
    }

    disable_persistent_shell_history();
    brush_shell::entry::run();
}

fn preferred_local_command(
    argv0: &OsStr,
    args: &[OsString],
    managed_applet: Option<&str>,
) -> Option<String> {
    if args
        .first()
        .is_some_and(|arg| arg == brush_shell::bundled::DISPATCH_FLAG)
    {
        return args.get(1)?.to_str().map(str::to_owned);
    }
    if let Some(name) = managed_applet {
        return Some(name.to_owned());
    }
    if args.first() == Some(&OsString::from("--applet")) {
        return args.get(1)?.to_str().map(str::to_owned);
    }

    let argv0_name = Path::new(argv0).file_name()?.to_str()?;
    if argv0_name != "axe" {
        return Some(argv0_name.to_owned());
    }
    args.first()
        .and_then(|argument| argument.to_str())
        .filter(|name| !name.starts_with('-'))
        .map(str::to_owned)
}

fn requested_store_mode(
    preferred_local: Option<&str>,
    args: &[OsString],
) -> Result<registry::StoreMode, String> {
    let inherited = registry::StoreMode::from_environment()?;
    if preferred_local != Some("sshd") {
        return Ok(inherited);
    }

    let mut requested = registry::StoreMode::Auto;
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        if argument == "--store-mode" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| "--store-mode requires a value".to_string())?;
            requested = value
                .to_str()
                .ok_or_else(|| "--store-mode is not valid UTF-8".to_string())?
                .parse()?;
            index += 2;
            continue;
        }
        if let Some(value) = argument
            .to_str()
            .and_then(|argument| argument.strip_prefix("--store-mode="))
        {
            requested = value.parse()?;
        }
        index += 1;
    }

    Ok(inherited.restrict(requested))
}

fn applet_args(args: impl IntoIterator<Item = OsString>) -> impl Iterator<Item = OsString> {
    let mut args = args.into_iter();
    let first = args.next();
    first
        .into_iter()
        .filter(|arg| arg != OsStr::new("--"))
        .chain(args)
}

fn publish_shell_paths(executable: &executable::Executable) {
    let publishable_path = executable.bridge_path();
    // SAFETY: this runs before registry construction or any other threads.
    unsafe {
        if let Some(path) = publishable_path.as_deref() {
            std::env::set_var("SHELL", path);
        }
        match publishable_path {
            Some(path) => std::env::set_var("AXE_SHELL", path),
            None => std::env::remove_var("AXE_SHELL"),
        }
    }
}

fn managed_applet_name(argv0: &OsStr, executable: &executable::Executable) -> Option<String> {
    let name = Path::new(argv0).components().next()?;
    let Component::Normal(name) = name else {
        return None;
    };
    if Path::new(argv0).components().nth(1).is_some() || name == OsStr::new("axe") {
        return None;
    }
    let applet_dir = std::env::var_os("AXE_APPLET_DIR")?;
    let target = std::fs::read_link(Path::new(&applet_dir).join(name)).ok()?;
    executable
        .matches(&target)
        .then(|| name.to_string_lossy().into_owned())
}

fn disable_persistent_shell_history() {
    // SAFETY: axe has not started the Brush runtime or any other threads yet.
    unsafe {
        std::env::set_var("HISTFILE", "/dev/null");
    }
}

fn busybox_name<'a>(
    argv0: &OsStr,
    commands: &'a std::collections::HashMap<String, brush_shell::bundled::BundledCommand>,
) -> Option<&'a str> {
    let name = Path::new(argv0).file_name()?.to_str()?;
    commands.get_key_value(name).map(|(name, _)| name.as_str())
}

fn brush_version() -> &'static str {
    "0.4.0"
}
