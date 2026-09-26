use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use brush_shell::bundled::{InProcessInvocation, ShellSnapshot};
use serde::Serialize;
use vzik::environment::{
    Failure, HostObservation, IsolationObservation, KernelObservation, MemoryObservation,
    Observation, ObservationStatus, RestrictionObservation,
};

use crate::executable::{ExecutableObservation, RelayObservation, SelfExecProbe};

const HELP: &str = "Usage: doctor [--path DIR] [--verbose]\n\
       doctor [--path DIR] --json\n\
\n\
Report the current AXE process, shell, host, isolation indicators, restrictions, and probes.\n\
\n\
Options:\n\
  --path DIR  use DIR for filesystem probes\n\
  --verbose   include full observations, probe stages, and empty sections\n\
  --json      emit one compact, complete axe_doctor schema v3 document\n\
  -h, --help  display this help and exit\n";
const REPORTED_ENVIRONMENT_NAMES: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "AXE",
    "AXE_SHELL",
    "PATH",
    "AXE_WORK_DIR",
    "AXE_STORE_MODE",
    "AXE_APPLET_DIR",
    "XDG_RUNTIME_DIR",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "TERM",
];
static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum EncodedOsValue {
    Utf8(String),
    Base64 { base64: String },
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", content = "observation", rename_all = "snake_case")]
pub(crate) enum ProbeState<T> {
    NotRun,
    Complete(Observation<T>),
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorReport {
    pub schema: &'static str,
    pub schema_version: u32,
    pub scope: &'static str,
    pub axe: AxeSection,
    pub launch: LaunchSection,
    pub shell: ShellSection,
    pub host: HostSection,
    pub environment: EnvironmentSection,
    pub isolation: IsolationObservation,
    pub restrictions: RestrictionObservation,
    pub capabilities: CapabilitySection,
    pub filesystem: FilesystemProbe,
    pub degradations: Vec<Degradation>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AxeSection {
    pub version: String,
    pub edition: &'static str,
    pub target: &'static str,
    pub commit: &'static str,
    pub target_os: &'static str,
    pub target_arch: &'static str,
    pub pid: Observation<u32>,
    pub ppid: Observation<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LaunchSection {
    pub argv0: Observation<EncodedOsValue>,
    pub dispatch_source: Observation<String>,
    pub startup_cwd: Observation<EncodedOsValue>,
    pub current_executable: Observation<EncodedOsValue>,
    pub has_launch_candidate: Observation<bool>,
    pub candidates: Vec<LaunchCandidate>,
    pub relay: Observation<EncodedOsValue>,
    pub bridge: BridgeObservation,
    pub self_exec: ProbeState<SelfExecProbe>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BridgeObservation {
    pub state: &'static str,
    pub path: Observation<EncodedOsValue>,
    pub failure: Option<Failure>,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LaunchCandidate {
    pub kind: &'static str,
    pub source: &'static str,
    pub path: Observation<EncodedOsValue>,
    pub usable: Observation<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ShellSection {
    pub current_cwd: Observation<EncodedOsValue>,
    pub name: Observation<String>,
    pub depth: Observation<usize>,
    pub interactive: Observation<bool>,
    pub login: Observation<bool>,
    pub restricted: Observation<bool>,
    pub command_source: Observation<String>,
    pub job_control: Observation<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HostSection {
    pub os: Observation<String>,
    pub architecture: Observation<String>,
    pub hostname: Observation<String>,
    pub distro: Observation<vzik::environment::DistroObservation>,
    pub kernel: KernelObservation,
    pub memory: MemoryObservation,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct EnvironmentSection {
    pub description: &'static str,
    pub variables: Vec<ExportedVariable>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ExportedVariable {
    pub name: EncodedOsValue,
    pub source: &'static str,
    pub value: Observation<EncodedOsValue>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CapabilitySection {
    pub kernel: KernelCapabilities,
    pub process: ProcessCapabilities,
    pub memory: MemoryCapabilities,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct KernelCapabilities {
    pub procfs: Observation<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProcessCapabilities {
    pub fork: ProbeState<bool>,
    pub self_exec: ProbeState<SelfExecProbe>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MemoryCapabilities {
    pub memfd_create: ProbeState<bool>,
    pub mfd_exec_flag: ProbeState<bool>,
    pub rw_to_rx: ProbeState<bool>,
    pub map_jit_rx: ProbeState<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FilesystemProbe {
    pub path: Observation<EncodedOsValue>,
    pub available_bytes: Observation<u64>,
    pub mount_execution_policy: Observation<String>,
    pub create_directory: Observation<bool>,
    pub create_file: Observation<bool>,
    pub write_file: Observation<bool>,
    pub sync_file: Observation<bool>,
    pub cleanup: Observation<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Degradation {
    pub component: String,
    pub command: Option<String>,
    pub operation: String,
    pub class: String,
    pub failure: Failure,
    pub fallback: Observation<String>,
    pub recovered: bool,
}

#[derive(Clone, Debug)]
struct Options {
    path: Option<PathBuf>,
    json: bool,
    verbose: bool,
}

enum ScopeInput {
    Process,
    Shell(ShellSnapshot),
}

pub fn doctor(args: Vec<OsString>) -> i32 {
    let stdout = io::stdout();
    let stderr = io::stderr();
    run(
        args,
        ScopeInput::Process,
        &mut stdout.lock(),
        &mut stderr.lock(),
    )
}

pub fn doctor_builtin(invocation: InProcessInvocation) -> u8 {
    let InProcessInvocation {
        argv,
        shell,
        stdin: _stdin,
        mut stdout,
        mut stderr,
    } = invocation;
    run(argv, ScopeInput::Shell(shell), &mut stdout, &mut stderr) as u8
}

fn run(
    args: Vec<OsString>,
    scope: ScopeInput,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let options = match parse_options(&args) {
        Ok(Some(options)) => options,
        Ok(None) => return write_output(stdout, HELP.as_bytes()),
        Err(error) => {
            let _ = writeln!(stderr, "doctor: {error}");
            return 2;
        }
    };
    let report = collect_report(&options, scope);

    if options.json {
        match serde_json::to_vec(&report) {
            Ok(mut output) => {
                output.push(b'\n');
                write_output(stdout, &output)
            }
            Err(error) => {
                let _ = writeln!(stderr, "doctor: cannot serialize report: {error}");
                5
            }
        }
    } else {
        write_output(stdout, render_human(&report, options.verbose).as_bytes())
    }
}

fn parse_options(args: &[OsString]) -> Result<Option<Options>, String> {
    let mut options = Options {
        path: None,
        json: false,
        verbose: false,
    };
    let mut index = 1;
    while index < args.len() {
        match args[index].to_str() {
            Some("-h" | "--help") if args.len() == 2 => return Ok(None),
            Some("--json") if !options.json => options.json = true,
            Some("--verbose") if !options.verbose => options.verbose = true,
            Some("--path") if options.path.is_none() => {
                let Some(path) = args.get(index + 1) else {
                    return Err("--path requires a directory".into());
                };
                options.path = Some(PathBuf::from(path));
                index += 1;
            }
            Some("--json") => return Err("--json may be specified only once".into()),
            Some("--verbose") => return Err("--verbose may be specified only once".into()),
            Some("--path") => return Err("--path may be specified only once".into()),
            Some(argument) => {
                return Err(format!("unknown argument {argument}; run 'doctor --help'"));
            }
            None => return Err("arguments must be valid UTF-8 except for --path values".into()),
        }
        index += 1;
    }

    if options.json && options.verbose {
        return Err("--json and --verbose cannot be combined".into());
    }

    Ok(Some(options))
}

fn collect_report(options: &Options, scope: ScopeInput) -> DoctorReport {
    let passive = vzik::environment::observe();
    let executable = crate::executable::current();
    let self_exec = ProbeState::Complete(executable.probe_self_exec());
    let fork = ProbeState::Complete(probe_fork());
    let memfd_create = probe_memfd(false);
    let mfd_exec_flag = probe_memfd(true);
    let rw_to_rx = ProbeState::Complete(probe_rw_to_rx());
    let map_jit_rx = probe_map_jit();
    let (scope_name, shell, variables, default_path) = scope_snapshot(scope, options);
    let filesystem = probe_filesystem(&default_path);
    let launch_observation = executable.observe();
    let mut degradations = collect_degradations(&launch_observation, &self_exec);
    record_probe_degradation(&mut degradations, "process", "fork", &fork);
    record_probe_degradation(&mut degradations, "memory", "memfd_create", &memfd_create);
    record_probe_degradation(&mut degradations, "memory", "mprotect(RW->RX)", &rw_to_rx);
    record_probe_degradation(&mut degradations, "memory", "mmap(MAP_JIT|RX)", &map_jit_rx);
    record_filesystem_degradations(&mut degradations, &filesystem);
    degradations.extend(
        executable
            .startup_facts()
            .discovery_failures
            .iter()
            .cloned()
            .map(|failure| Degradation {
                component: "launch".into(),
                operation: failure.operation.clone(),
                command: None,
                class: failure.class.clone(),
                failure,
                fallback: Observation::available("continued with the next launch candidate".into()),
                recovered: true,
            }),
    );

    let launch = launch_section(&launch_observation, &self_exec);
    let process_self_exec = self_exec.clone();
    let HostObservation {
        os,
        architecture,
        hostname,
        distro,
    } = passive.host;

    DoctorReport {
        schema: "axe_doctor",
        schema_version: 3,
        scope: scope_name,
        axe: AxeSection {
            version: crate::VERSION.to_owned(),
            edition: crate::embedded::EDITION_ID,
            target: env!("AXE_BUILD_TARGET"),
            commit: env!("AXE_BUILD_COMMIT"),
            target_os: std::env::consts::OS,
            target_arch: std::env::consts::ARCH,
            pid: passive.process.pid,
            ppid: passive.process.ppid,
        },
        launch,
        shell,
        host: HostSection {
            os,
            architecture,
            hostname,
            distro,
            kernel: passive.kernel,
            memory: passive.memory,
        },
        environment: EnvironmentSection {
            description: "selected exported environment variables",
            variables: environment_variables(variables),
        },
        isolation: passive.isolation,
        restrictions: passive.restrictions,
        capabilities: CapabilitySection {
            kernel: KernelCapabilities {
                procfs: procfs_observation(),
            },
            process: ProcessCapabilities {
                fork,
                self_exec: process_self_exec,
            },
            memory: MemoryCapabilities {
                memfd_create,
                mfd_exec_flag,
                rw_to_rx,
                map_jit_rx,
            },
        },
        filesystem,
        degradations,
    }
}

fn scope_snapshot(
    scope: ScopeInput,
    options: &Options,
) -> (
    &'static str,
    ShellSection,
    Vec<(OsString, OsString, &'static str)>,
    PathBuf,
) {
    match scope {
        ScopeInput::Shell(shell) => {
            let default_path = options
                .path
                .clone()
                .or_else(|| {
                    shell
                        .exported_variables
                        .iter()
                        .find(|(name, _)| name == "AXE_WORK_DIR")
                        .map(|(_, value)| PathBuf::from(value))
                })
                .unwrap_or_else(|| shell.current_directory.clone());
            let variables = shell
                .exported_variables
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value), "shell_export"))
                .collect();
            (
                "shell",
                ShellSection {
                    current_cwd: Observation::available(encode_os(
                        shell.current_directory.as_os_str(),
                    )),
                    name: shell.name.map_or_else(
                        || Observation::absent("shell name is not set"),
                        Observation::available,
                    ),
                    depth: Observation::available(shell.depth),
                    interactive: Observation::available(shell.interactive),
                    login: Observation::available(shell.login),
                    restricted: Observation::available(shell.restricted),
                    command_source: Observation::available(shell.command_source.to_owned()),
                    job_control: Observation::available(shell.job_control),
                },
                variables,
                default_path,
            )
        }
        ScopeInput::Process => {
            let default_path = options
                .path
                .clone()
                .or_else(|| std::env::var_os("AXE_WORK_DIR").map(PathBuf::from))
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let variables = std::env::vars_os()
                .map(|(name, value)| (name, value, "process_environment"))
                .collect();
            (
                "process",
                ShellSection {
                    current_cwd: Observation::<EncodedOsValue>::not_applicable(
                        "live shell state is unavailable in process scope",
                    ),
                    name: Observation::<String>::not_applicable(
                        "live shell state is unavailable in process scope",
                    ),
                    depth: Observation::<usize>::not_applicable(
                        "live shell state is unavailable in process scope",
                    ),
                    interactive: Observation::<bool>::not_applicable(
                        "live shell state is unavailable in process scope",
                    ),
                    login: Observation::<bool>::not_applicable(
                        "live shell state is unavailable in process scope",
                    ),
                    restricted: Observation::<bool>::not_applicable(
                        "live shell state is unavailable in process scope",
                    ),
                    command_source: Observation::<String>::not_applicable(
                        "live shell state is unavailable in process scope",
                    ),
                    job_control: Observation::<bool>::not_applicable(
                        "live shell state is unavailable in process scope",
                    ),
                },
                variables,
                default_path,
            )
        }
    }
}

fn launch_section(
    observation: &ExecutableObservation,
    self_exec: &ProbeState<SelfExecProbe>,
) -> LaunchSection {
    let executable = crate::executable::current();
    let facts = executable.startup_facts();
    let candidates = observation
        .candidates
        .iter()
        .map(|candidate| LaunchCandidate {
            kind: candidate.kind,
            source: candidate.source,
            path: candidate.path.as_deref().map_or_else(
                || Observation::not_applicable("candidate has no stable pathname"),
                |path| Observation::available(encode_os(path.as_os_str())),
            ),
            usable: Observation::available(candidate.usable),
        })
        .collect::<Vec<_>>();
    let has_launch_candidate = candidates.iter().any(|candidate| {
        candidate.usable.status == ObservationStatus::Available
            && candidate.usable.value == Some(true)
    });
    let relay = match &observation.relay {
        RelayObservation::NotAttempted => Observation::unknown("lazy relay has not been attempted"),
        #[cfg(target_os = "linux")]
        RelayObservation::Available(path) => Observation::available(encode_os(path.as_os_str())),
        #[cfg(target_os = "linux")]
        RelayObservation::Failed(failure) => Observation::unavailable(failure.clone()),
    };
    let bridge = match crate::path_bridge::observe() {
        None => BridgeObservation {
            state: "not_attempted",
            path: Observation::not_applicable("PATH bridge installation was not attempted"),
            failure: None,
        },
        Some(Ok(crate::path_bridge::BridgeState::Reused(path))) => BridgeObservation {
            state: "reused",
            path: Observation::available(encode_os(path.as_os_str()))
                .with_evidence("startup", "reused an inherited PATH bridge"),
            failure: None,
        },
        Some(Ok(crate::path_bridge::BridgeState::Published(path))) => BridgeObservation {
            state: "published",
            path: Observation::available(encode_os(path.as_os_str()))
                .with_evidence("startup", "published a PATH bridge"),
            failure: None,
        },
        Some(Ok(crate::path_bridge::BridgeState::Disabled)) => BridgeObservation {
            state: "disabled",
            path: Observation::absent("no PATH bridge target or writable root was available"),
            failure: None,
        },
        Some(Err(failure)) => BridgeObservation {
            state: "failed",
            path: Observation::unavailable(failure.clone()),
            failure: Some(failure.clone()),
        },
    };

    LaunchSection {
        argv0: Observation::available(encode_os(&facts.argv0)),
        dispatch_source: Observation::available("multicall registry".into()),
        startup_cwd: match &facts.startup_directory {
            Ok(path) => Observation::available(encode_os(path.as_os_str())),
            Err(failure) => Observation::unavailable(failure.clone()),
        },
        current_executable: std::env::current_exe().map_or_else(
            |error| Observation::unavailable(Failure::from_io("read current executable", &error)),
            |path| Observation::available(encode_os(path.as_os_str())),
        ),
        has_launch_candidate: Observation::available(has_launch_candidate),
        candidates,
        relay,
        bridge,
        self_exec: self_exec.clone(),
    }
}

fn environment_variables(
    variables: Vec<(OsString, OsString, &'static str)>,
) -> Vec<ExportedVariable> {
    REPORTED_ENVIRONMENT_NAMES
        .iter()
        .map(|name| {
            variables
                .iter()
                .find(|(candidate, _, _)| candidate == OsStr::new(name))
                .map_or_else(
                    || ExportedVariable {
                        name: EncodedOsValue::Utf8((*name).to_owned()),
                        source: "not_exported",
                        value: Observation::absent("variable is not exported"),
                    },
                    |(_, value, source)| ExportedVariable {
                        name: EncodedOsValue::Utf8((*name).to_owned()),
                        source,
                        value: Observation::available(encode_os(value)),
                    },
                )
        })
        .collect()
}

fn collect_degradations(
    executable: &ExecutableObservation,
    self_exec: &ProbeState<SelfExecProbe>,
) -> Vec<Degradation> {
    let mut result = Vec::new();
    if let Some(failure) = executable.relay.failure() {
        result.push(Degradation {
            component: "launch".into(),
            command: None,
            operation: failure.operation.clone(),
            class: failure.class.clone(),
            failure: failure.clone(),
            fallback: Observation::unknown("no relay fallback is currently available"),
            recovered: false,
        });
    }
    if let Some(Err(failure)) = crate::path_bridge::observe() {
        result.push(Degradation {
            component: "path_bridge".into(),
            command: None,
            operation: failure.operation.clone(),
            class: failure.class.clone(),
            failure: failure.clone(),
            fallback: Observation::available("shell command resolution remains available".into()),
            recovered: true,
        });
    }
    let bundled = brush_shell::bundled::launch_observation();
    if let Some(launch_failure) = bundled.last_failure {
        let failure = Failure {
            operation: launch_failure.operation.into(),
            class: bundled_failure_class(launch_failure.errno).into(),
            code: Some(launch_failure.code.into()),
            errno: launch_failure.errno,
            message: launch_failure.message,
        };
        result.push(Degradation {
            component: "bundled_command".into(),
            operation: failure.operation.clone(),
            command: Some(launch_failure.command),
            class: failure.class.clone(),
            failure,
            fallback: Observation::available(
                "native parent-shell builtins remain available".into(),
            ),
            recovered: bundled.later_bundled_launch_succeeded,
        });
    }
    if let ProbeState::Complete(Observation {
        status: ObservationStatus::Unavailable,
        error: Some(failure),
        ..
    }) = self_exec
    {
        result.push(Degradation {
            component: "self_exec_probe".into(),
            command: None,
            operation: failure.operation.clone(),
            class: failure.class.clone(),
            failure: failure.clone(),
            fallback: Observation::unknown(
                "probe failure does not disable known launch candidates",
            ),
            recovered: false,
        });
    }
    result
}

fn record_probe_degradation<T>(
    degradations: &mut Vec<Degradation>,
    component: &str,
    operation: &str,
    probe: &ProbeState<T>,
) {
    let ProbeState::Complete(observation) = probe else {
        return;
    };
    record_observation_degradation(degradations, component, operation, observation);
}

fn record_filesystem_degradations(
    degradations: &mut Vec<Degradation>,
    filesystem: &FilesystemProbe,
) {
    record_observation_degradation(
        degradations,
        "filesystem",
        "query available bytes",
        &filesystem.available_bytes,
    );
    record_observation_degradation(
        degradations,
        "filesystem",
        "create probe directory",
        &filesystem.create_directory,
    );
    record_observation_degradation(
        degradations,
        "filesystem",
        "create probe file",
        &filesystem.create_file,
    );
    record_observation_degradation(
        degradations,
        "filesystem",
        "write probe file",
        &filesystem.write_file,
    );
    record_observation_degradation(
        degradations,
        "filesystem",
        "sync probe file",
        &filesystem.sync_file,
    );
    record_observation_degradation(
        degradations,
        "filesystem",
        "clean probe artifacts",
        &filesystem.cleanup,
    );
}

fn record_observation_degradation<T>(
    degradations: &mut Vec<Degradation>,
    component: &str,
    operation: &str,
    observation: &Observation<T>,
) {
    let Some(failure) = &observation.error else {
        return;
    };
    degradations.push(Degradation {
        component: component.to_owned(),
        command: None,
        operation: operation.to_owned(),
        class: failure.class.clone(),
        failure: failure.clone(),
        fallback: Observation::absent("active probe has no fallback"),
        recovered: false,
    });
}

fn procfs_observation() -> Observation<bool> {
    match fs::metadata("/proc/self/status") {
        Ok(_) => {
            Observation::available(true).with_evidence("/proc/self/status", "metadata succeeded")
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Observation::available(false).with_evidence("/proc/self/status", "path is absent")
        }
        Err(error) => Observation::unavailable(Failure::from_io("inspect procfs", &error)),
    }
}

fn probe_fork() -> Observation<bool> {
    #[cfg(unix)]
    {
        // SAFETY: the child calls only async-signal-safe `_exit`; the parent waits for that PID.
        let pid = unsafe { libc::fork() };
        if pid == -1 {
            return Observation::unavailable(Failure::from_io(
                "fork probe child",
                &io::Error::last_os_error(),
            ));
        }
        if pid == 0 {
            // SAFETY: `_exit` terminates the post-fork child without Rust destructors.
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        // SAFETY: `pid` is the live child returned by fork and `status` is writable.
        if unsafe { libc::waitpid(pid, &mut status, 0) } == -1 {
            return Observation::unavailable(Failure::from_io(
                "wait for fork probe child",
                &io::Error::last_os_error(),
            ));
        }
        Observation::available(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0)
    }
    #[cfg(not(unix))]
    {
        Observation::unsupported("fork is unavailable on this platform")
    }
}

fn probe_memfd(with_exec_flag: bool) -> ProbeState<bool> {
    #[cfg(target_os = "linux")]
    {
        const MFD_CLOEXEC: libc::c_uint = 0x0001;
        const MFD_EXEC: libc::c_uint = 0x0010;
        let name = b"axe-doctor\0";
        let flags = MFD_CLOEXEC | if with_exec_flag { MFD_EXEC } else { 0 };
        // SAFETY: the name is NUL-terminated and flags follow memfd_create's ABI.
        let fd =
            unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags) } as libc::c_int;
        if fd == -1 {
            return ProbeState::Complete(memfd_error(with_exec_flag, io::Error::last_os_error()));
        }
        // SAFETY: fd was returned by memfd_create and is owned by this probe.
        unsafe { libc::close(fd) };
        ProbeState::Complete(Observation::available(true))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = with_exec_flag;
        ProbeState::Complete(Observation::unsupported("memfd_create is Linux-specific"))
    }
}

#[cfg(target_os = "linux")]
fn memfd_error(with_exec_flag: bool, error: io::Error) -> Observation<bool> {
    if with_exec_flag && error.raw_os_error() == Some(libc::EINVAL) {
        Observation::unsupported(
            "this kernel predates MFD_EXEC; legacy executable memfds do not require the flag",
        )
    } else {
        Observation::unavailable(Failure::from_io(
            if with_exec_flag {
                "memfd_create MFD_EXEC"
            } else {
                "memfd_create"
            },
            &error,
        ))
    }
}

fn probe_rw_to_rx() -> Observation<bool> {
    #[cfg(unix)]
    {
        let length = 4096;
        // SAFETY: anonymous private mapping with a null hint and valid flags.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Observation::unavailable(Failure::from_io(
                "mmap RW probe page",
                &io::Error::last_os_error(),
            ));
        }
        // SAFETY: mapping names a live page-sized mapping created above.
        let protected =
            unsafe { libc::mprotect(mapping, length, libc::PROT_READ | libc::PROT_EXEC) };
        let protect_error = (protected == -1).then(io::Error::last_os_error);
        // SAFETY: mapping and length are the exact values returned to this probe.
        let cleanup = unsafe { libc::munmap(mapping, length) };
        if let Some(error) = protect_error {
            return Observation::unavailable(Failure::from_io("mprotect RW to RX", &error));
        }
        if cleanup == -1 {
            return Observation::unavailable(Failure::from_io(
                "unmap executable probe page",
                &io::Error::last_os_error(),
            ));
        }
        Observation::available(true)
    }
    #[cfg(not(unix))]
    {
        Observation::unsupported("mmap/mprotect are unavailable on this platform")
    }
}

fn probe_map_jit() -> ProbeState<bool> {
    #[cfg(target_os = "macos")]
    {
        let length = 4096;
        // SAFETY: anonymous private MAP_JIT mapping with executable protection
        // has no file descriptor or caller-provided pointer.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return ProbeState::Complete(Observation::unavailable(Failure::from_io(
                "mmap MAP_JIT probe page",
                &io::Error::last_os_error(),
            )));
        }
        // SAFETY: mapping and length are the exact values returned above.
        let cleanup = unsafe { libc::munmap(mapping, length) };
        if cleanup == -1 {
            return ProbeState::Complete(Observation::unavailable(Failure::from_io(
                "unmap MAP_JIT probe page",
                &io::Error::last_os_error(),
            )));
        }
        ProbeState::Complete(Observation::available(true))
    }
    #[cfg(not(target_os = "macos"))]
    {
        ProbeState::Complete(Observation::not_applicable("MAP_JIT is macOS-specific"))
    }
}

fn probe_filesystem(path: &Path) -> FilesystemProbe {
    let path_observation = Observation::available(encode_os(path.as_os_str()));
    let available_bytes = axe_paths::available_bytes(path).map_or_else(
        |error| {
            Observation::unavailable(Failure::from_io("read available filesystem bytes", &error))
        },
        Observation::available,
    );
    let mount_execution_policy = match axe_paths::mount_execution_policy(path) {
        axe_paths::MountExecutionPolicy::Supported => Observation::available("supported".into()),
        axe_paths::MountExecutionPolicy::NoExec => Observation::available("noexec".into()),
        axe_paths::MountExecutionPolicy::Unknown => {
            Observation::unknown("mount information is unavailable or incomplete")
        }
        axe_paths::MountExecutionPolicy::Unsupported => {
            Observation::unsupported("mount execution policy is unsupported on this platform")
        }
    };
    let directory = path.join(format!(
        ".axe-doctor-{}-{}",
        std::process::id(),
        NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
    ));
    let create_directory = match fs::create_dir(&directory) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if let Err(error) =
                    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                {
                    let cleanup = cleanup_probe(&directory, None);
                    return FilesystemProbe {
                        path: path_observation,
                        available_bytes,
                        mount_execution_policy,
                        create_directory: Observation::unavailable(Failure::from_io(
                            "make probe directory private",
                            &error,
                        )),
                        create_file: Observation::not_applicable("probe directory setup failed"),
                        write_file: Observation::not_applicable("probe directory setup failed"),
                        sync_file: Observation::not_applicable("probe directory setup failed"),
                        cleanup,
                    };
                }
            }
            Observation::available(true)
        }
        Err(error) => {
            return FilesystemProbe {
                path: path_observation,
                available_bytes,
                mount_execution_policy,
                create_directory: Observation::unavailable(Failure::from_io(
                    "create private probe directory",
                    &error,
                )),
                create_file: Observation::not_applicable("probe directory creation failed"),
                write_file: Observation::not_applicable("probe directory creation failed"),
                sync_file: Observation::not_applicable("probe directory creation failed"),
                cleanup: Observation::not_applicable("probe directory was not created"),
            };
        }
    };
    let file_path = directory.join("run");
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
    {
        Ok(file) => file,
        Err(error) => {
            let cleanup = cleanup_probe(&directory, Some(&file_path));
            return FilesystemProbe {
                path: path_observation,
                available_bytes,
                mount_execution_policy,
                create_directory,
                create_file: Observation::unavailable(Failure::from_io(
                    "create probe file",
                    &error,
                )),
                write_file: Observation::not_applicable("probe file creation failed"),
                sync_file: Observation::not_applicable("probe file creation failed"),
                cleanup,
            };
        }
    };
    let create_file = Observation::available(true);
    let write_file = match file.write_all(b"axe doctor filesystem probe\n") {
        Ok(()) => Observation::available(true),
        Err(error) => Observation::unavailable(Failure::from_io("write probe file", &error)),
    };
    let sync_file = match file.sync_all() {
        Ok(()) => Observation::available(true),
        Err(error) => Observation::unavailable(Failure::from_io("sync probe file", &error)),
    };
    drop(file);
    let cleanup = cleanup_probe(&directory, Some(&file_path));

    FilesystemProbe {
        path: path_observation,
        available_bytes,
        mount_execution_policy,
        create_directory,
        create_file,
        write_file,
        sync_file,
        cleanup,
    }
}

fn cleanup_probe(directory: &Path, file: Option<&Path>) -> Observation<bool> {
    let file_result = file.map(fs::remove_file).transpose();
    let directory_result = fs::remove_dir(directory);
    match (file_result, directory_result) {
        (Ok(_), Ok(())) => Observation::available(true),
        (Err(error), _) => Observation::unavailable(Failure::from_io("remove probe file", &error)),
        (_, Err(error)) => {
            Observation::unavailable(Failure::from_io("remove probe directory", &error))
        }
    }
}

pub(crate) fn encode_os(value: &OsStr) -> EncodedOsValue {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        match value.to_str() {
            Some(value) => EncodedOsValue::Utf8(value.to_owned()),
            None => EncodedOsValue::Base64 {
                base64: base64::engine::general_purpose::STANDARD.encode(value.as_bytes()),
            },
        }
    }
    #[cfg(not(unix))]
    {
        EncodedOsValue::Utf8(value.to_string_lossy().into_owned())
    }
}

fn bundled_failure_class(errno: Option<i32>) -> &'static str {
    match errno {
        Some(libc::EAGAIN) | Some(libc::ENOMEM) => "transient",
        Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOSYS) => "stable_restriction",
        _ => "runtime_launch",
    }
}

fn write_output(writer: &mut impl Write, bytes: &[u8]) -> i32 {
    match writer.write_all(bytes) {
        Ok(()) => 0,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(_) => 5,
    }
}

fn render_human(report: &DoctorReport, verbose: bool) -> String {
    let mut output = String::from("AXE Doctor\n==========\n");
    fact(&mut output, "Version", &report.axe.version);
    fact(&mut output, "Edition", report.axe.edition);
    fact(&mut output, "Target", report.axe.target);
    fact(&mut output, "Process", &observation_text(&report.axe.pid));
    if verbose {
        fact(
            &mut output,
            "Parent process",
            &observation_text(&report.axe.ppid),
        );
    }

    section(&mut output, "Summary");
    fact(
        &mut output,
        "Self-exec",
        &self_exec_text(&report.launch.self_exec),
    );
    fact(
        &mut output,
        "Process creation",
        &probe_text(&report.capabilities.process.fork),
    );
    fact(
        &mut output,
        "Anonymous executable mapping",
        &executable_mapping_text(&report.capabilities.memory),
    );
    fact(
        &mut output,
        "MFD_EXEC flag",
        &mfd_exec_flag_text(&report.capabilities.memory.mfd_exec_flag),
    );
    fact(
        &mut output,
        "Filesystem",
        &filesystem_summary_text(&report.filesystem),
    );
    fact(
        &mut output,
        "Sandboxing",
        &isolation_text(&report.isolation.sandbox),
    );
    fact(
        &mut output,
        "Degradations",
        &match report.degradations.len() {
            0 => "none".into(),
            count => format!("{count} recorded"),
        },
    );

    section(&mut output, "Attention");
    let attention = attention_lines(report);
    if attention.is_empty() {
        output.push_str("  None detected by active checks.\n");
    } else {
        for line in attention {
            output.push_str("  - ");
            output.push_str(&line);
            output.push('\n');
        }
    }

    section(&mut output, "Launch");
    fact(
        &mut output,
        "argv[0]",
        &observation_text(&report.launch.argv0),
    );
    fact(
        &mut output,
        "Current executable",
        &observation_text(&report.launch.current_executable),
    );
    fact(
        &mut output,
        "Self-exec",
        &self_exec_text(&report.launch.self_exec),
    );
    fact(
        &mut output,
        "PATH bridge",
        &bridge_text(&report.launch.bridge),
    );
    if verbose {
        fact(
            &mut output,
            "Dispatch source",
            &observation_text(&report.launch.dispatch_source),
        );
        fact(
            &mut output,
            "Startup directory",
            &observation_text(&report.launch.startup_cwd),
        );
        fact(
            &mut output,
            "Filesystem relay",
            &observation_text(&report.launch.relay),
        );
        output.push_str("  Candidates\n");
        for candidate in &report.launch.candidates {
            let name = candidate.kind.replace('_', " ");
            let mut detail = format!(
                "{} [{}]: {}",
                name,
                candidate.source,
                observation_text(&candidate.usable)
            );
            if candidate.path.status == ObservationStatus::Available {
                detail.push_str(" — ");
                detail.push_str(&observation_text(&candidate.path));
            }
            output.push_str("    - ");
            output.push_str(&detail);
            output.push('\n');
        }
    }

    section(&mut output, "Shell");
    fact(
        &mut output,
        "Working directory",
        &observation_text(&report.shell.current_cwd),
    );
    fact(&mut output, "Name", &observation_text(&report.shell.name));
    fact(
        &mut output,
        "Interactive",
        &observation_text(&report.shell.interactive),
    );
    if verbose {
        fact(
            &mut output,
            "Command source",
            &observation_text(&report.shell.command_source),
        );
        fact(
            &mut output,
            "Login shell",
            &observation_text(&report.shell.login),
        );
        fact(
            &mut output,
            "Restricted",
            &observation_text(&report.shell.restricted),
        );
        fact(
            &mut output,
            "Job control",
            &observation_text(&report.shell.job_control),
        );
        fact(&mut output, "Depth", &observation_text(&report.shell.depth));
    }

    section(&mut output, "Host");
    fact(&mut output, "OS", &observation_text(&report.host.os));
    fact(
        &mut output,
        "Architecture",
        &observation_text(&report.host.architecture),
    );
    fact(
        &mut output,
        "Distribution",
        &distro_text(&report.host.distro),
    );
    fact(
        &mut output,
        "Kernel",
        &format!(
            "{} {}",
            observation_text(&report.host.kernel.name),
            observation_text(&report.host.kernel.release)
        ),
    );
    fact(
        &mut output,
        "Available memory",
        &bytes_text(&report.host.memory.available_bytes),
    );
    if verbose {
        fact(
            &mut output,
            "Hostname",
            &observation_text(&report.host.hostname),
        );
        fact(
            &mut output,
            "Physical memory",
            &bytes_text(&report.host.memory.physical_bytes),
        );
    }

    section(&mut output, "Isolation indicators");
    fact(
        &mut output,
        "Virtual machine indicator",
        &isolation_text(&report.isolation.virtual_machine),
    );
    fact(
        &mut output,
        "Container indicator",
        &isolation_text(&report.isolation.container),
    );
    fact(
        &mut output,
        "Sandboxing",
        &isolation_text(&report.isolation.sandbox),
    );

    section(&mut output, "Observed restrictions");
    fact(
        &mut output,
        "No new privileges",
        &enabled_text(&report.restrictions.no_new_privileges),
    );
    fact(
        &mut output,
        "Seccomp",
        &seccomp_text(
            &report.restrictions.seccomp_mode,
            &report.restrictions.seccomp_filter_count,
        ),
    );
    let effective_capabilities = if verbose {
        capability_set_text(&report.restrictions.capability_sets.effective)
    } else {
        capability_set_summary_text(&report.restrictions.capability_sets.effective)
    };
    fact(
        &mut output,
        "Effective capabilities",
        &effective_capabilities,
    );
    fact(
        &mut output,
        "LSM profile",
        &observation_text(&report.restrictions.lsm_profile),
    );
    if verbose {
        fact(
            &mut output,
            "Bounding capabilities",
            &capability_set_text(&report.restrictions.capability_sets.bounding),
        );
        fact(
            &mut output,
            "UID mapping",
            &observation_text(&report.restrictions.uid_map),
        );
        fact(
            &mut output,
            "GID mapping",
            &observation_text(&report.restrictions.gid_map),
        );
    }

    if verbose {
        section(&mut output, "Active checks");
        fact(
            &mut output,
            "Process creation",
            &probe_text(&report.capabilities.process.fork),
        );
        fact(
            &mut output,
            "Self-exec AXE",
            &self_exec_text(&report.capabilities.process.self_exec),
        );
        fact(
            &mut output,
            "procfs mounted",
            &mounted_text(&report.capabilities.kernel.procfs),
        );
        fact(
            &mut output,
            "memfd_create syscall",
            &probe_text(&report.capabilities.memory.memfd_create),
        );
        fact(
            &mut output,
            "MFD_EXEC flag",
            &mfd_exec_flag_text(&report.capabilities.memory.mfd_exec_flag),
        );
        fact(
            &mut output,
            "Anonymous RW to RX mapping",
            &probe_text(&report.capabilities.memory.rw_to_rx),
        );
        fact(
            &mut output,
            "MAP_JIT RX mapping",
            &probe_text(&report.capabilities.memory.map_jit_rx),
        );
    }

    section(&mut output, "Filesystem probe");
    fact(
        &mut output,
        "Directory",
        &observation_text(&report.filesystem.path),
    );
    fact(
        &mut output,
        "Writable",
        &filesystem_writable_text(&report.filesystem),
    );
    fact(
        &mut output,
        "Noexec",
        &noexec_text(&report.filesystem.mount_execution_policy),
    );
    fact(
        &mut output,
        "Cleanup",
        &observation_text(&report.filesystem.cleanup),
    );
    if verbose {
        fact(
            &mut output,
            "Free space",
            &bytes_text(&report.filesystem.available_bytes),
        );
        fact(
            &mut output,
            "Create directory",
            &observation_text(&report.filesystem.create_directory),
        );
        fact(
            &mut output,
            "Create file",
            &observation_text(&report.filesystem.create_file),
        );
        fact(
            &mut output,
            "Write file",
            &observation_text(&report.filesystem.write_file),
        );
        fact(
            &mut output,
            "Sync file",
            &observation_text(&report.filesystem.sync_file),
        );
    }

    section(&mut output, "Environment");
    for variable in &report.environment.variables {
        if !verbose && variable.value.status == ObservationStatus::Absent {
            continue;
        }
        let name = encoded_text(&variable.name);
        if name == "PATH"
            && let Observation {
                status: ObservationStatus::Available,
                value: Some(EncodedOsValue::Utf8(value)),
                ..
            } = &variable.value
        {
            output.push_str("  PATH:\n");
            for component in std::env::split_paths(value) {
                output.push_str("    - ");
                output.push_str(&escape_human(&component.to_string_lossy()));
                output.push('\n');
            }
            continue;
        }

        let value = observation_text(&variable.value);
        output.push_str("  ");
        output.push_str(&name);
        output.push('=');
        output.push_str(&value);
        output.push('\n');
    }

    if verbose || !report.degradations.is_empty() {
        section(&mut output, "Degradations");
        if report.degradations.is_empty() {
            output.push_str("  None\n");
        } else {
            for degradation in &report.degradations {
                output.push_str("  - ");
                output.push_str(&degradation.component);
                output.push_str(": ");
                output.push_str(&degradation.operation);
                output.push('\n');
                if let Some(command) = &degradation.command {
                    fact(&mut output, "Command", command);
                }
                fact(
                    &mut output,
                    "Failure",
                    &format!(
                        "{} — {}",
                        degradation.failure.class, degradation.failure.message
                    ),
                );
                fact(
                    &mut output,
                    "Fallback",
                    &observation_text(&degradation.fallback),
                );
                fact(
                    &mut output,
                    "Recovered",
                    if degradation.recovered { "yes" } else { "no" },
                );
            }
        }
    }

    output
}

fn attention_lines(report: &DoctorReport) -> Vec<String> {
    let mut lines = Vec::new();

    if !self_exec_works(&report.capabilities.process.self_exec) {
        lines.push(format!(
            "Self-exec: {}. Child AXE processes may not start.",
            self_exec_text(&report.capabilities.process.self_exec)
        ));
    }
    if !probe_works(&report.capabilities.process.fork) {
        lines.push(format!(
            "Process creation: {}. External child processes may not start.",
            probe_text(&report.capabilities.process.fork)
        ));
    }
    if !executable_mapping_works(&report.capabilities.memory) {
        lines.push(
            "Anonymous executable mapping: no working method was observed. JIT-style executable mappings may be unavailable."
                .into(),
        );
    }
    if !filesystem_write_works(&report.filesystem) {
        lines.push(format!(
            "Filesystem write: {}. Temporary files and scripts may not be created.",
            filesystem_writable_text(&report.filesystem)
        ));
    }
    if mount_is_noexec(&report.filesystem.mount_execution_policy) {
        lines.push(
            "Filesystem mount: noexec is set. Direct execution from the probe directory is blocked."
                .into(),
        );
    }
    if !observation_works(&report.filesystem.cleanup) {
        lines.push(format!(
            "Probe cleanup: {}. Probe artifacts may remain in the selected directory.",
            observation_text(&report.filesystem.cleanup)
        ));
    }
    for degradation in &report.degradations {
        let impact = if degradation.recovered {
            "the fallback recovered the operation"
        } else {
            "the operation remains unavailable"
        };
        lines.push(format!(
            "{} / {}: {} — {}; {impact}.",
            degradation.component,
            degradation.operation,
            degradation.failure.class,
            escape_human(&degradation.failure.message)
        ));
    }

    lines
}

fn observation_works(observation: &Observation<bool>) -> bool {
    observation.status == ObservationStatus::Available && observation.value == Some(true)
}

fn probe_works(probe: &ProbeState<bool>) -> bool {
    matches!(probe, ProbeState::Complete(observation) if observation_works(observation))
}

fn self_exec_works(probe: &ProbeState<SelfExecProbe>) -> bool {
    matches!(
        probe,
        ProbeState::Complete(Observation {
            status: ObservationStatus::Available,
            value: Some(SelfExecProbe { success: true, .. }),
            ..
        })
    )
}

fn executable_mapping_works(memory: &MemoryCapabilities) -> bool {
    probe_works(&memory.rw_to_rx) || probe_works(&memory.map_jit_rx)
}

fn executable_mapping_text(memory: &MemoryCapabilities) -> String {
    let mut methods = Vec::new();
    if probe_works(&memory.rw_to_rx) {
        methods.push("RW to RX");
    }
    if probe_works(&memory.map_jit_rx) {
        methods.push("MAP_JIT RX");
    }
    if methods.is_empty() {
        "not proven".into()
    } else {
        format!("works ({})", methods.join(", "))
    }
}

fn mfd_exec_flag_text(probe: &ProbeState<bool>) -> String {
    match probe {
        ProbeState::Complete(observation) if observation_works(observation) => "accepted".into(),
        ProbeState::Complete(Observation {
            status: ObservationStatus::Available,
            value: Some(false),
            ..
        }) => "rejected".into(),
        _ => probe_text(probe),
    }
}

fn filesystem_write_works(filesystem: &FilesystemProbe) -> bool {
    observation_works(&filesystem.create_directory)
        && observation_works(&filesystem.create_file)
        && observation_works(&filesystem.write_file)
        && observation_works(&filesystem.sync_file)
}

fn filesystem_writable_text(filesystem: &FilesystemProbe) -> String {
    if filesystem_write_works(filesystem) {
        "yes".into()
    } else {
        "not proven; inspect the detailed probe stages with --verbose".into()
    }
}

fn filesystem_summary_text(filesystem: &FilesystemProbe) -> String {
    let writable = if filesystem_write_works(filesystem) {
        "writable"
    } else {
        "writability not proven"
    };
    format!(
        "{writable}; noexec {}",
        noexec_text(&filesystem.mount_execution_policy)
    )
}

fn mount_is_noexec(observation: &Observation<String>) -> bool {
    observation.status == ObservationStatus::Available
        && observation.value.as_deref() == Some("noexec")
}

fn noexec_text(observation: &Observation<String>) -> String {
    match (&observation.status, observation.value.as_deref()) {
        (ObservationStatus::Available, Some("supported")) => "not set".into(),
        (ObservationStatus::Available, Some("noexec")) => "set".into(),
        _ => observation_text(observation),
    }
}

fn enabled_text(observation: &Observation<bool>) -> String {
    match (&observation.status, observation.value) {
        (ObservationStatus::Available, Some(true)) => "enabled".into(),
        (ObservationStatus::Available, Some(false)) => "disabled".into(),
        _ => observation_text(observation),
    }
}

fn mounted_text(observation: &Observation<bool>) -> String {
    match (&observation.status, observation.value) {
        (ObservationStatus::Available, Some(true)) => "mounted".into(),
        (ObservationStatus::Available, Some(false)) => "not mounted".into(),
        _ => observation_text(observation),
    }
}

fn seccomp_text(mode: &Observation<u32>, filters: &Observation<u32>) -> String {
    match (&mode.status, mode.value) {
        (ObservationStatus::Available, Some(0)) => "disabled".into(),
        (ObservationStatus::Available, Some(1)) => "strict mode".into(),
        (ObservationStatus::Available, Some(2)) => match (&filters.status, filters.value) {
            (ObservationStatus::Available, Some(count)) => {
                format!("filtering enabled ({count} filters)")
            }
            _ => "filtering enabled".into(),
        },
        (ObservationStatus::Available, Some(other)) => format!("unknown mode {other}"),
        _ => observation_text(mode),
    }
}

fn capability_set_text(capabilities: &Observation<Vec<String>>) -> String {
    match (&capabilities.status, &capabilities.value) {
        (ObservationStatus::Available, Some(capabilities)) if capabilities.is_empty() => {
            "none".into()
        }
        (ObservationStatus::Available, Some(capabilities)) => capabilities.join(", "),
        _ => observation_text(capabilities),
    }
}

fn capability_set_summary_text(capabilities: &Observation<Vec<String>>) -> String {
    match (&capabilities.status, &capabilities.value) {
        (ObservationStatus::Available, Some(capabilities)) if capabilities.is_empty() => {
            "none".into()
        }
        (ObservationStatus::Available, Some(capabilities)) => {
            format!("{} enabled; use --verbose to list", capabilities.len())
        }
        _ => observation_text(capabilities),
    }
}

fn section(output: &mut String, title: &str) {
    output.push('\n');
    output.push_str(title);
    output.push('\n');
    output.push_str(&"-".repeat(title.chars().count()));
    output.push('\n');
}

fn fact(output: &mut String, name: &str, value: &str) {
    output.push_str("  ");
    output.push_str(name);
    output.push_str(": ");
    output.push_str(value);
    output.push('\n');
}

fn observation_text<T: Serialize>(observation: &Observation<T>) -> String {
    match (&observation.status, &observation.value) {
        (ObservationStatus::Available, Some(value)) => match serde_json::to_value(value) {
            Ok(serde_json::Value::Bool(true)) => "yes".into(),
            Ok(serde_json::Value::Bool(false)) => "no".into(),
            Ok(serde_json::Value::String(value)) => escape_human(&value),
            Ok(value) => value.to_string(),
            Err(error) => format!("unavailable — {error}"),
        },
        (status, _) => {
            let status = serde_json::to_string(status)
                .unwrap_or_else(|_| "\"unknown\"".into())
                .trim_matches('"')
                .replace('_', " ");
            if let Some(reason) = &observation.reason {
                format!("{status} — {}", escape_human(reason))
            } else if let Some(error) = &observation.error {
                format!("{status} — {}", escape_human(&error.message))
            } else {
                status
            }
        }
    }
}

fn self_exec_text(probe: &ProbeState<SelfExecProbe>) -> String {
    match probe {
        ProbeState::NotRun => "not run".into(),
        ProbeState::Complete(Observation {
            status: ObservationStatus::Available,
            value: Some(result),
            ..
        }) if result.success => "works".into(),
        ProbeState::Complete(Observation {
            status: ObservationStatus::Available,
            value: Some(result),
            ..
        }) => result.exit_code.map_or_else(
            || "failed without an exit status".into(),
            |code| format!("failed (exit status {code})"),
        ),
        ProbeState::Complete(observation) => observation_text(observation),
    }
}

fn distro_text(observation: &Observation<vzik::environment::DistroObservation>) -> String {
    match (&observation.status, &observation.value) {
        (ObservationStatus::Available, Some(distro)) => distro
            .pretty_name
            .as_deref()
            .or(distro.name.as_deref())
            .or(distro.id.as_deref())
            .map_or_else(|| "available".into(), escape_human),
        _ => observation_text(observation),
    }
}

fn isolation_text(layer: &vzik::environment::IsolationLayer) -> String {
    let mut value = match layer.verdict.status {
        ObservationStatus::Available => "heuristic match".into(),
        ObservationStatus::Unknown => "unknown".into(),
        _ => observation_text(&layer.verdict),
    };
    if layer.provider.status == ObservationStatus::Available {
        value.push_str(" — ");
        value.push_str(&observation_text(&layer.provider));
    }
    if layer.confidence.status == ObservationStatus::Available {
        value.push_str(" (");
        value.push_str(&observation_text(&layer.confidence));
        value.push_str(" confidence)");
    }
    value
}

fn bytes_text(observation: &Observation<u64>) -> String {
    match (&observation.status, observation.value) {
        (ObservationStatus::Available, Some(bytes)) => {
            const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
            format!("{:.1} GiB ({bytes} bytes)", bytes as f64 / GIB)
        }
        _ => observation_text(observation),
    }
}

fn bridge_text(bridge: &BridgeObservation) -> String {
    if bridge.state == "failed"
        && let Some(failure) = &bridge.failure
    {
        return format!("failed — {}", escape_human(&failure.message));
    }
    if bridge.path.status == ObservationStatus::Available {
        return observation_text(&bridge.path);
    }
    bridge.state.replace('_', " ")
}

fn probe_text<T: Serialize>(probe: &ProbeState<T>) -> String {
    match probe {
        ProbeState::NotRun => "not run".into(),
        ProbeState::Complete(observation) => observation_text(observation),
    }
}

fn encoded_text(value: &EncodedOsValue) -> String {
    match value {
        EncodedOsValue::Utf8(value) => escape_human(value),
        EncodedOsValue::Base64 { base64 } => format!("base64:{base64}"),
    }
}

fn escape_human(value: &str) -> String {
    value.chars().flat_map(char::escape_debug).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_reports_only_selected_values() {
        let variables = environment_variables(vec![
            (
                OsString::from("SAFE"),
                OsString::from("secret-value"),
                "test",
            ),
            (
                OsString::from("PATH"),
                OsString::from("/custom/bin"),
                "test",
            ),
            (OsString::from("SHELL"), OsString::from("/bin/sh"), "test"),
        ]);
        let json = serde_json::to_string(&variables).expect("serialize environment");

        assert_eq!(variables.len(), REPORTED_ENVIRONMENT_NAMES.len());
        assert!(!json.contains("SAFE"));
        assert!(!json.contains("secret-value"));
        assert!(json.contains("/custom/bin"));
        assert!(json.contains("/bin/sh"));
        assert!(!json.contains("redacted"));
    }

    #[test]
    fn utf8_os_values_serialize_as_strings() {
        let encoded = encode_os(OsStr::new("./dist/axe-x86_64-unknown-linux-musl"));
        assert_eq!(
            serde_json::to_value(encoded).expect("serialize encoded value"),
            serde_json::json!("./dist/axe-x86_64-unknown-linux-musl")
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_os_values_are_base64_encoded() {
        use std::os::unix::ffi::OsStrExt as _;

        let encoded = encode_os(OsStr::from_bytes(b"a\xffb"));
        assert_eq!(
            serde_json::to_value(encoded).expect("serialize encoded value"),
            serde_json::json!({"base64":"Yf9i"})
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unsupported_mfd_exec_flag_is_not_a_failed_memfd_probe() {
        let observation = memfd_error(true, io::Error::from_raw_os_error(libc::EINVAL));

        assert_eq!(observation.status, ObservationStatus::Unsupported);
        assert!(observation.error.is_none());
        assert!(
            observation
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("legacy executable memfds"))
        );
    }
}
