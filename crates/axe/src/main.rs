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
    let routes = DirectInvocation::parse(&argv0, &args, managed_applet.as_deref());
    let preferred = DirectInvocation::preferred(&routes);
    let preferred_local = preferred.and_then(|route| route.name?.to_str());
    let store_mode =
        match requested_store_mode(preferred_local, preferred.map_or(&[], |route| route.args)) {
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

    let mode = if preferred
        .is_some_and(|route| matches!(route.route, Route::Hidden | Route::Managed))
        || std::env::var_os(registry::INDEX_CACHE_ONLY_ENV).as_deref() == Some(OsStr::new("1"))
    {
        registry::BuildMode::CacheOnly
    } else {
        registry::BuildMode::RevalidateIfStale
    };
    let registry = match registry::build(mode, preferred_local, store_mode) {
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
        registry.commands,
        Arc::new(move || bundled_executable.bundled_executable()),
    );

    for route in routes.iter().flatten() {
        if let Some(code) = route.dispatch() {
            std::process::exit(code);
        }
    }

    if let Some(error) = &registry.blocking_index_error
        && preferred_local.is_some_and(|name| !registry::is_installed(name))
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
    brush_shell::entry::run(Some(r"axe \w\$ "));
}

/// How argv selected an applet, in dispatch precedence order.
#[derive(Clone, Copy)]
enum Route {
    /// `axe --invoke-bundled NAME [ARG ...]`, used by Brush shims and AXE self-exec.
    Hidden,
    /// `argv[0]` names an `AXE_APPLET_DIR` bridge link to this executable.
    Managed,
    /// `argv[0]` is a registered applet name (BusyBox-style link or copy).
    BusyBox,
    /// `axe --applet NAME [--] [ARG ...]`.
    Explicit,
    /// `axe NAME [--] [ARG ...]`.
    Positional,
}

impl Route {
    /// Explicit routes always dispatch; the others fall through to the next
    /// route or the shell when their name is not registered.
    fn explicit(self) -> bool {
        matches!(self, Self::Hidden | Self::Managed | Self::Explicit)
    }
}

/// Applet entry route parsed once from argv, before the registry exists.
struct DirectInvocation<'a> {
    route: Route,
    /// `None` when hidden dispatch or `--applet` lacks a command name.
    name: Option<&'a OsStr>,
    args: &'a [OsString],
}

impl<'a> DirectInvocation<'a> {
    /// Returns the candidate routes in dispatch precedence order: hidden
    /// dispatch or the managed bridge alone, otherwise the `argv[0]` name
    /// followed by `--applet` or the positional name.
    fn parse(
        argv0: &'a OsStr,
        args: &'a [OsString],
        managed_applet: Option<&'a str>,
    ) -> [Option<Self>; 2] {
        if args
            .first()
            .is_some_and(|arg| arg == brush_shell::bundled::DISPATCH_FLAG)
        {
            let hidden = Self {
                route: Route::Hidden,
                name: args.get(1).map(OsString::as_os_str),
                args: args.get(2..).unwrap_or_default(),
            };
            return [Some(hidden), None];
        }
        if let Some(name) = managed_applet {
            let managed = Self {
                route: Route::Managed,
                name: Some(OsStr::new(name)),
                args,
            };
            return [Some(managed), None];
        }

        let busybox = Path::new(argv0)
            .file_name()
            .filter(|name| name.to_str().is_some_and(|name| name != "axe"))
            .map(|name| Self {
                route: Route::BusyBox,
                name: Some(name),
                args,
            });
        let leading = match args.first() {
            Some(flag) if flag == "--applet" => Some(Self {
                route: Route::Explicit,
                name: args.get(1).map(OsString::as_os_str),
                args: without_separator(args.get(2..).unwrap_or_default()),
            }),
            Some(name) if name.to_str().is_some_and(|name| !name.starts_with('-')) => Some(Self {
                route: Route::Positional,
                name: Some(name),
                args: without_separator(&args[1..]),
            }),
            _ => None,
        };
        [busybox, leading]
    }

    /// Returns the route whose applet selects Store mode before the registry
    /// exists: the first explicit route, otherwise the first candidate.
    fn preferred(routes: &[Option<Self>; 2]) -> Option<&Self> {
        let mut candidates = routes.iter().flatten();
        candidates
            .clone()
            .find(|route| route.route.explicit())
            .or_else(|| candidates.next())
    }

    /// Runs the applet from the installed registry, or returns `None` when a
    /// fallible route names no registered applet.
    fn dispatch(&self) -> Option<i32> {
        let Some(name) = self.name else {
            let flag = match self.route {
                Route::Hidden => brush_shell::bundled::DISPATCH_FLAG,
                _ => "--applet",
            };
            eprintln!("axe: {flag} requires a command name");
            return Some(2);
        };
        if !self.route.explicit() && !name.to_str().is_some_and(registry::is_installed) {
            return None;
        }

        let code = brush_shell::bundled::invoke_installed(name, self.args.iter().cloned())
            .unwrap_or_else(|error| {
                eprintln!("axe: {error}");
                127
            });
        Some(code)
    }
}

fn without_separator(args: &[OsString]) -> &[OsString] {
    match args.split_first() {
        Some((first, rest)) if first == "--" => rest,
        _ => args,
    }
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

fn brush_version() -> &'static str {
    "0.4.0"
}
