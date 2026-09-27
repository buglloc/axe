//! Bundled commands: utilities that ship inside the brush binary.
//!
//! Utilities are shipped busybox-style (one binary, many names) but execute
//! as a subprocess of brush so that shell redirections, pipes, and
//! process-group state are honored by code that reads/writes the host
//! process's standard fds (e.g., uutils crates).
//!
//! ## Protocol
//!
//! The brush binary recognizes a hidden first-position argument
//! [`DISPATCH_FLAG`] followed by `<NAME> [ARGS...]`. When present, brush
//! dispatches early in `main()` to the registered function for `NAME`, before
//! any shell state is built, and exits with the function's return code. The
//! dispatched function has the same signature as `uutils`' `uumain`:
//! `fn(Vec<OsString>) -> i32`, with the bundled name as `argv[0]`.
//!
//! Embedding binaries that parse their own entry routes (multicall `argv[0]`
//! names, explicit applet flags, the hidden flag itself) install the registry
//! once and call [`invoke_installed`] instead: it runs the same lookup and
//! entry point as [`maybe_dispatch`] for an already-parsed `NAME [ARGS...]`
//! and reports unknown names as [`UnknownBundledCommand`].
//!
//! ## Shell integration
//!
//! For every entry in the registry, [`register_commands`] installs a Brush
//! builtin (using `register_builtin_if_unset`, so Brush's own builtins always
//! win on conflict). Spawn-backed entries use brush-core's external-command
//! machinery and preserve descriptor-aware self-exec. Native callbacks execute
//! synchronously against a snapshot of the live parent shell with the
//! context-derived standard streams.
//!
//! The mechanism is generic: each registry entry carries its standalone
//! multicall function and shell execution policy.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use brush_core::ExecutionExitCode;
use brush_core::builtins::{BoxFuture, ContentOptions, ContentType, Registration};
use brush_core::commands::{self, CommandArg, ExecutionContext};
use brush_core::extensions::ShellExtensions;

/// The leading flag that signals a bundled-command dispatch.
///
/// Deliberately obscure so that it's unlikely to collide with future
/// first-class shell flags or with scripts that happen to contain the
/// literal token.
pub const DISPATCH_FLAG: &str = "--invoke-bundled";

/// Signature of a bundled command's entry point — matches `uu_*::uumain`.
pub type BundledFn = fn(args: Vec<OsString>) -> i32;

/// A bundled command and its shell execution policy.
#[derive(Clone, Copy)]
pub struct BundledCommand {
    /// Standalone multicall entry point.
    pub entry: BundledFn,
    /// How the command executes when resolved inside a live Brush shell.
    pub shell_execution: ShellExecution,
}

impl BundledCommand {
    /// Wraps a conventional subprocess-backed bundled command.
    #[must_use]
    pub const fn spawn(entry: BundledFn) -> Self {
        Self {
            entry,
            shell_execution: ShellExecution::Spawn,
        }
    }

    /// Wraps a command whose shell form runs synchronously in the parent shell.
    #[must_use]
    pub const fn builtin(entry: BundledFn, callback: InProcessFn) -> Self {
        Self {
            entry,
            shell_execution: ShellExecution::Builtin(callback),
        }
    }
}

/// Execution policy for a bundled command resolved by the shell.
#[derive(Clone, Copy)]
pub enum ShellExecution {
    /// Re-execute the bundled binary in a child process.
    Spawn,
    /// Invoke an AXE-owned callback against a snapshot of the live parent shell.
    Builtin(InProcessFn),
}

/// Signature of an injected parent-shell builtin.
pub type InProcessFn = fn(InProcessInvocation) -> u8;

/// Owned live-shell data supplied to an in-process bundled callback.
pub struct InProcessInvocation {
    /// Arguments including the invoked command name at index zero.
    pub argv: Vec<OsString>,
    /// Shell state captured at command invocation.
    pub shell: ShellSnapshot,
    /// Context-derived standard input after shell redirections.
    pub stdin: Box<dyn Read>,
    /// Context-derived standard output after shell redirections.
    pub stdout: Box<dyn Write>,
    /// Context-derived standard error after shell redirections.
    pub stderr: Box<dyn Write>,
}

/// Stable subset of the live Brush state needed by AXE-owned diagnostics.
#[derive(Clone, Debug)]
pub struct ShellSnapshot {
    /// Current shell working directory.
    pub current_directory: PathBuf,
    /// Current shell name, accounting for script call frames.
    pub name: Option<String>,
    /// Brush subshell depth.
    pub depth: usize,
    /// Whether the shell is interactive.
    pub interactive: bool,
    /// Whether the shell is a login shell.
    pub login: bool,
    /// Whether restricted-shell mode is active.
    pub restricted: bool,
    /// How the current top-level command stream was supplied.
    pub command_source: &'static str,
    /// Whether shell job control is enabled.
    pub job_control: bool,
    /// Sorted exported variables visible in this shell.
    pub exported_variables: Vec<(String, String)>,
}

/// Structured details for the most recent bundled child-launch failure.
#[derive(Clone, Debug)]
pub struct BundledLaunchFailure {
    /// Bundled command that failed to launch.
    pub command: String,
    /// Stable operation name.
    pub operation: &'static str,
    /// Stable machine-readable failure code.
    pub code: &'static str,
    /// Original I/O error kind when one was available.
    pub io_error_kind: Option<io::ErrorKind>,
    /// Raw platform errno when one was available.
    pub errno: Option<i32>,
    /// Human-readable source error.
    pub message: String,
}

/// Bounded observation of bundled child-launch health.
#[derive(Clone, Debug, Default)]
pub struct BundledLaunchObservation {
    /// Most recent child-launch failure.
    pub last_failure: Option<BundledLaunchFailure>,
    /// Whether a later bundled child launch succeeded.
    pub later_bundled_launch_succeeded: bool,
}
#[cfg(target_os = "linux")]
pub use brush_core::commands::DescriptorExecutable;
#[cfg(target_os = "linux")]
pub use brush_core::sys::descriptor_exec::{
    INHERITED_EXECUTABLE_FALLBACK, INHERITED_EXECUTABLE_FD, INHERITED_RUNTIME_ROOT,
};

/// Provider consulted immediately before each bundled child launch.
pub type BundledExecutableProvider =
    Arc<dyn Fn() -> Option<BundledExecutable> + Send + Sync + 'static>;

/// Capability used to launch another copy of the bundled executable.
#[derive(Clone)]
pub enum BundledExecutable {
    /// A conventional executable path.
    Path(PathBuf),
    /// A retained executable descriptor, with an optional path fallback for
    /// kernels or policies that reject descriptor execution.
    #[cfg(target_os = "linux")]
    Descriptor {
        /// Retained executable descriptor.
        descriptor: DescriptorExecutable,
        /// Checked path used when descriptor execution is unavailable.
        fallback_path: Option<PathBuf>,
    },
}

impl BundledExecutable {
    fn path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path),
            #[cfg(target_os = "linux")]
            Self::Descriptor { fallback_path, .. } => fallback_path.as_deref(),
        }
    }

    #[cfg(target_os = "linux")]
    fn descriptor(&self) -> Option<&DescriptorExecutable> {
        match self {
            Self::Path(_) => None,
            Self::Descriptor { descriptor, .. } => Some(descriptor),
        }
    }
}

/// Process-wide registry. Set once at startup, read on each command invocation.
static REGISTRY: OnceLock<HashMap<String, BundledCommand>> = OnceLock::new();

/// Provider for the current capability to launch the running executable.
static SELF_EXE: OnceLock<BundledExecutableProvider> = OnceLock::new();
static BUNDLED_LAUNCH: LazyLock<Mutex<BundledLaunchObservation>> =
    LazyLock::new(|| Mutex::new(BundledLaunchObservation::default()));

/// Installs a conventional subprocess-backed bundled-command registry.
/// Idempotent: only the first call takes effect.
#[allow(
    clippy::implicit_hasher,
    reason = "registry uses the default hasher; callers build with HashMap::new()"
)]
pub fn install(commands: HashMap<String, BundledFn>) {
    let commands = commands
        .into_iter()
        .map(|(name, entry)| (name, BundledCommand::spawn(entry)))
        .collect();
    let _ = REGISTRY.set(commands);
}

/// Installs command policies with a dynamic executable capability provider.
///
/// The provider is consulted immediately before every bundled child launch, so
/// callers can revalidate paths or materialize a lazy fallback after startup.
#[allow(
    clippy::implicit_hasher,
    reason = "registry uses the default hasher; callers build with HashMap::new()"
)]
pub fn install_with_executable(
    commands: HashMap<String, BundledCommand>,
    executable: BundledExecutableProvider,
) {
    let _ = SELF_EXE.set(executable);
    let _ = REGISTRY.set(commands);
}

/// Installs the registry from all compiled-in providers.
///
/// Providers are controlled by Cargo features. Binaries should call this
/// once, before [`maybe_dispatch`], so both the dispatch fast path and the
/// shell's shim builtins see a populated registry.
pub fn install_default_providers() {
    #[allow(unused_mut)]
    let mut commands: HashMap<String, BundledFn> = HashMap::new();

    #[cfg(feature = "experimental-bundled-coreutils")]
    commands.extend(brush_coreutils_builtins::bundled_commands());

    install(commands);
}

/// Returns the registered bundled commands, if [`install`] was called.
#[must_use]
pub fn registry() -> Option<&'static HashMap<String, BundledCommand>> {
    REGISTRY.get()
}

/// Returns the bounded bundled child-launch observation.
#[must_use]
pub fn launch_observation() -> BundledLaunchObservation {
    BUNDLED_LAUNCH
        .lock()
        .map(|observation| observation.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
}

/// Runs the bundled-command fast path if the process was invoked for it.
///
/// If the process was invoked as `brush <DISPATCH_FLAG> <NAME> [ARGS...]`
/// (with `<DISPATCH_FLAG>` as the very first argument after `argv[0]`), runs
/// the registered function and returns its exit code as `Some(code)`. The
/// caller is responsible for exiting the process with that code —
/// centralizing the exit call in the binary's `main()` keeps destructors /
/// panic hooks / tracing guards in the loop.
///
/// Returns `None` when the process was not invoked as a bundled dispatch, so
/// normal shell startup can proceed.
///
/// The dispatch flag is only recognized in the leading position so that
/// ordinary scripts and command lines containing the literal token elsewhere
/// are not affected.
#[must_use]
pub fn maybe_dispatch() -> Option<i32> {
    let mut raw = std::env::args_os();
    let _argv0 = raw.next();
    let first = raw.next()?;
    if first != DISPATCH_FLAG {
        return None;
    }

    // Everything after `DISPATCH_FLAG` belongs to the bundled command. The
    // first such argument is the command name; subsequent arguments form its
    // argv (with the name itself supplied as argv[0] to match the convention
    // `uutils` and most CLI tools expect).
    let rest: Vec<OsString> = raw.collect();
    let Some((name, args)) = rest.split_first() else {
        eprintln!("brush: {DISPATCH_FLAG} requires a command name");
        return Some(exit_code(ExecutionExitCode::InvalidUsage));
    };

    Some(invoke_installed(name, args.iter().cloned()).unwrap_or_else(|error| {
        eprintln!("brush: {error}");
        exit_code(ExecutionExitCode::NotFound)
    }))
}

/// Error returned by [`invoke_installed`] when no installed bundled command
/// has the requested name. Callers report it and exit with status 127.
#[derive(Debug)]
pub struct UnknownBundledCommand(OsString);

impl fmt::Display for UnknownBundledCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown bundled command: {}",
            self.0.to_string_lossy()
        )
    }
}

/// Runs the installed bundled command `name` in the current process.
///
/// The command receives `name` as `argv[0]` followed by `args`, and its exit
/// code is returned. The registry is keyed by UTF-8 `String`, so a non-UTF-8
/// name never matches: it is rejected up front rather than looked up through
/// a lossy key that could collide with a real registration.
///
/// # Errors
///
/// Returns [`UnknownBundledCommand`] when the registry is not installed or
/// has no command named `name`.
pub fn invoke_installed(
    name: &OsStr,
    args: impl IntoIterator<Item = OsString>,
) -> Result<i32, UnknownBundledCommand> {
    let command = name
        .to_str()
        .and_then(|name| REGISTRY.get()?.get(name))
        .ok_or_else(|| UnknownBundledCommand(name.to_owned()))?;

    let mut argv = vec![name.to_owned()];
    argv.extend(args);
    Ok((command.entry)(argv))
}

fn exit_code(code: ExecutionExitCode) -> i32 {
    u8::from(code).into()
}

/// Returns a fresh capability for launching the running Brush executable.
fn self_exe() -> Option<BundledExecutable> {
    let provider = SELF_EXE
        .get_or_init(|| Arc::new(|| std::env::current_exe().ok().map(BundledExecutable::Path)));
    provider()
}

/// Returns a path usable by path-only nested command implementations.
///
/// Descriptor-aware Brush shims do not require this path. `None` means the
/// caller must degrade because its process API cannot accept a descriptor.
#[must_use]
pub fn executable_path() -> Option<PathBuf> {
    self_exe().and_then(|executable| executable.path().map(Path::to_owned))
}

/// Help/usage content provider for the shim builtin. brush calls this for
/// `help <name>`, `type <name>`, etc.
#[allow(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    reason = "signature dictated by brush_core::builtins::CommandContentFunc"
)]
fn shim_content(
    name: &str,
    content_type: ContentType,
    _options: &ContentOptions,
) -> Result<String, brush_core::Error> {
    match content_type {
        ContentType::ShortDescription => Ok(format!("{name} - bundled command")),
        ContentType::DetailedHelp => Ok(format!(
            "{name} - bundled command (executes via `brush {DISPATCH_FLAG} {name}`)\n"
        )),
        // A bundled command never contributes its own short-usage or man page
        // through this path; detailed help comes from the bundled utility
        // itself (`brush <DISPATCH_FLAG> <name> --help` or equivalent).
        ContentType::ShortUsage | ContentType::ManPage => Ok(String::new()),
    }
}

/// Builtin execute function shared by all bundled commands. Looks up the
/// invoked name from `context.command_name` and re-executes the running
/// brush binary as `brush <DISPATCH_FLAG> <name> <args>`.
///
/// Reuses the same entry point the `command` builtin uses (see
/// `brush-builtins/src/command.rs`): constructs a [`commands::SimpleCommand`]
/// whose `command_name` is the absolute brush exe path. Because that contains
/// a path separator, `SimpleCommand::execute` routes directly to the
/// external-execution path, bypassing the builtin/function lookup that would
/// otherwise re-enter this very shim.
///
/// `use_functions = false` is defensive: even though the path-separator
/// branch already skips function dispatch, we don't want a hypothetical
/// refactor of `SimpleCommand` to silently break us.
//
// TODO(bundled): Process-group propagation.
// The shim leaves `SimpleCommand::process_group_id` as `None`, so when a
// bundled command appears in a pipeline it doesn't join the pipeline's
// pgid — job control and pipeline-wide signal delivery misbehave.
// `ExecutionContext` doesn't currently carry the dispatcher's pgid, so
// fixing this requires plumbing the pgid through the builtin dispatch
// boundary (likely as a field on `ExecutionParameters` or a new
// `ExecutionContext` accessor).
//
// TODO(bundled): Pipeline serialization.
// The builtin contract returns an `ExecutionResult` (a completed command),
// not an `ExecutionSpawnResult` (a spawn handle), so this function has to
// `.await` the child to completion before returning. That's fine for a
// standalone bundled command or for the tail of a pipeline, but for a
// bundled stage in the middle of `a | b | c` it means stage N only
// "starts" (from brush's perspective) after its child has fully exited —
// downstream stages get no parallelism with it. Fixing this means
// bypassing the builtin API for bundled dispatch: either detect the shim
// inside `SimpleCommand::execute`'s dispatch table and return an
// `ExecutionSpawnResult::StartedProcess` directly (same shape as external
// dispatch), or generalize the builtin API so a builtin can return a
// spawn handle instead of a finished result.
fn shim_execute<SE: ShellExtensions>(
    context: ExecutionContext<'_, SE>,
    args: Vec<CommandArg>,
) -> BoxFuture<'_, Result<brush_core::ExecutionResult, brush_core::Error>> {
    Box::pin(async move {
        let Some(executable) = self_exe() else {
            let _ = writeln!(
                context.stderr(),
                "brush: cannot determine how to launch running executable"
            );
            return Ok(ExecutionExitCode::CannotExecute.into());
        };
        let exe_path = executable
            .path()
            .unwrap_or_else(|| Path::new("/dev/null"))
            .to_string_lossy()
            .into_owned();

        // Build the argv for the spawned brush. `SimpleCommand::args[0]` is
        // dropped by the external-execution path (argv[0] of the spawned
        // process comes from `cmd.argv0` below), so a placeholder suffices;
        // args[1..] become the spawned process's argv[1..]. The caller's
        // `args[0]` is the bundled name by builtin-dispatch convention — we
        // replace it with an explicit `<name>` after `DISPATCH_FLAG` so the
        // child's dispatcher sees it in a fixed slot.
        let bundled_name = context.command_name.clone();
        let mut child_args: Vec<CommandArg> = Vec::with_capacity(args.len() + 2);
        child_args.push(CommandArg::String(String::new())); // args[0], dropped
        child_args.push(CommandArg::String(DISPATCH_FLAG.into()));
        child_args.push(CommandArg::String(bundled_name.clone()));
        child_args.extend(args.into_iter().skip(1));

        let mut cmd = commands::SimpleCommand::new(
            commands::ShellForCommand::ParentShell(context.shell),
            context.params,
            exe_path,
            child_args,
        );
        cmd.use_functions = false;
        // Override the spawned process's argv[0] so tools that report errors
        // via their own argv[0] (uutils' `uucore::util_name()` reads
        // `std::env::args_os()[0]` into a LazyLock at first use) render as
        // `<name>:` rather than `brush:`. Without this the child sees the
        // brush exe path as argv[0] and misattributes errors.
        cmd.argv0 = Some(bundled_name.clone());
        #[cfg(target_os = "linux")]
        {
            cmd.executable_descriptor = executable.descriptor().cloned();
        }

        let spawn_result = match cmd.execute().await {
            Ok(result) => {
                let mut observation = BUNDLED_LAUNCH
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if observation.last_failure.is_some() {
                    observation.later_bundled_launch_succeeded = true;
                }
                result
            }
            Err(error) => {
                let failure = bundled_launch_failure(&bundled_name, &error);
                let mut observation = BUNDLED_LAUNCH
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                observation.last_failure = Some(failure);
                observation.later_bundled_launch_succeeded = false;
                return Err(error);
            }
        };
        let wait_result = spawn_result.wait().await?;
        Ok(wait_result.into())
    })
}

fn bundled_launch_failure(command: &str, error: &brush_core::Error) -> BundledLaunchFailure {
    let io_error = error.as_io_error();
    let io_error_kind = io_error.map(io::Error::kind);
    BundledLaunchFailure {
        command: command.to_owned(),
        operation: "launch bundled child",
        code: io_error_kind.map_or("shell_error", stable_io_error_code),
        io_error_kind,
        errno: io_error.and_then(io::Error::raw_os_error),
        message: io_error.map_or_else(|| error.to_string(), ToString::to_string),
    }
}

fn stable_io_error_code(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::ConnectionRefused => "connection_refused",
        io::ErrorKind::ConnectionReset => "connection_reset",
        io::ErrorKind::ConnectionAborted => "connection_aborted",
        io::ErrorKind::NotConnected => "not_connected",
        io::ErrorKind::AddrInUse => "address_in_use",
        io::ErrorKind::AddrNotAvailable => "address_not_available",
        io::ErrorKind::BrokenPipe => "broken_pipe",
        io::ErrorKind::AlreadyExists => "already_exists",
        io::ErrorKind::WouldBlock => "would_block",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::WriteZero => "write_zero",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::Unsupported => "unsupported",
        io::ErrorKind::UnexpectedEof => "unexpected_eof",
        io::ErrorKind::OutOfMemory => "out_of_memory",
        _ => "io_error",
    }
}

/// Constructs a [`Registration`] for the bundled-shim builtin. The same
/// registration value can be reused for every bundled name; per-name
/// dispatch happens via `context.command_name` at execution time.
fn shim_registration<SE: ShellExtensions>() -> Registration<SE> {
    Registration {
        execute_func: shim_execute::<SE>,
        content_func: shim_content,
        disabled: false,
        special_builtin: false,
        declaration_builtin: false,
    }
}

fn in_process_execute<SE: ShellExtensions>(
    context: ExecutionContext<'_, SE>,
    args: Vec<CommandArg>,
    callback: InProcessFn,
) -> BoxFuture<'_, Result<brush_core::ExecutionResult, brush_core::Error>> {
    Box::pin(async move {
        let options = context.shell.options();
        let command_source = if options.interactive {
            "interactive"
        } else if options.command_string_mode {
            "command_string"
        } else if options.read_commands_from_stdin {
            "stdin"
        } else {
            "script"
        };
        let mut exported_variables = context
            .shell
            .env()
            .iter_exported()
            .filter_map(|(name, _)| {
                context
                    .shell
                    .env_str(name)
                    .map(|value| (name.clone(), value.into_owned()))
            })
            .collect::<Vec<_>>();
        exported_variables.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let shell = ShellSnapshot {
            current_directory: context.shell.working_dir().to_owned(),
            name: context
                .shell
                .current_shell_name()
                .map(|name| name.into_owned()),
            depth: context.shell.depth(),
            interactive: options.interactive,
            login: options.login_shell,
            restricted: options.restricted_shell,
            command_source,
            job_control: options.enable_job_control,
            exported_variables,
        };
        let mut argv = Vec::with_capacity(args.len().max(1));
        argv.push(OsString::from(&context.command_name));
        argv.extend(
            args.into_iter()
                .skip(1)
                .map(|arg| OsString::from(arg.to_string())),
        );
        let invocation = InProcessInvocation {
            argv,
            shell,
            stdin: Box::new(context.stdin()),
            stdout: Box::new(context.stdout()),
            stderr: Box::new(context.stderr()),
        };
        let status = callback(invocation);

        Ok(brush_core::ExecutionResult::new(status))
    })
}

fn builtin_execute<SE: ShellExtensions>(
    context: ExecutionContext<'_, SE>,
    args: Vec<CommandArg>,
) -> BoxFuture<'_, Result<brush_core::ExecutionResult, brush_core::Error>> {
    let callback = REGISTRY
        .get()
        .and_then(|registry| registry.get(&context.command_name))
        .and_then(|command| match command.shell_execution {
            ShellExecution::Builtin(callback) => Some(callback),
            ShellExecution::Spawn => None,
        });
    match callback {
        Some(callback) => in_process_execute(context, args, callback),
        None => shim_execute(context, args),
    }
}

fn builtin_registration<SE: ShellExtensions>() -> Registration<SE> {
    Registration {
        execute_func: builtin_execute::<SE>,
        content_func: shim_content,
        disabled: false,
        special_builtin: false,
        declaration_builtin: false,
    }
}

/// Registers a builtin for every installed bundled command.
///
/// Uses `register_builtin_if_unset` so Brush's own builtins win on conflict.
/// Spawn commands retain descriptor-aware self-reexec; injected builtin
/// commands execute synchronously against the live parent shell.
pub fn register_commands<SE: ShellExtensions>(shell: &mut brush_core::Shell<SE>) {
    let Some(registry) = REGISTRY.get() else {
        return;
    };
    for (name, command) in registry {
        let registration = match command.shell_execution {
            ShellExecution::Spawn => shim_registration::<SE>(),
            ShellExecution::Builtin(_) => builtin_registration::<SE>(),
        };
        shell.register_builtin_if_unset(name.clone(), registration);
    }
}
