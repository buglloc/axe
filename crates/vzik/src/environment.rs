use std::collections::BTreeMap;
#[cfg(any(target_os = "linux", test))]
use std::fs;
use std::io;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(any(target_os = "linux", test))]
use std::path::PathBuf;

use serde::Serialize;
#[cfg(target_os = "linux")]
const TEXT_SOURCE_LIMIT: u64 = 1 << 20;
#[cfg(target_os = "linux")]
const SUMMARY_SOURCE_LIMIT: u64 = 16 << 10;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationStatus {
    Available,
    Absent,
    Unavailable,
    Unsupported,
    Unknown,
    NotApplicable,
    Redacted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Failure {
    pub operation: String,
    pub class: String,
    pub code: Option<String>,
    pub errno: Option<i32>,
    pub message: String,
}

impl Failure {
    #[must_use]
    pub fn from_io(operation: impl Into<String>, error: &io::Error) -> Self {
        Self {
            operation: operation.into(),
            class: failure_class(error).to_owned(),
            code: Some(format!("{:?}", error.kind()).to_ascii_lowercase()),
            errno: error.raw_os_error(),
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub source: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Observation<T> {
    pub status: ObservationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

impl<T> Observation<T> {
    #[must_use]
    pub fn available(value: T) -> Self {
        Self {
            status: ObservationStatus::Available,
            value: Some(value),
            reason: None,
            error: None,
            evidence: Vec::new(),
        }
    }

    #[must_use]
    pub fn absent(reason: impl Into<String>) -> Self {
        Self::without_value(ObservationStatus::Absent, reason)
    }

    #[must_use]
    pub fn unavailable(failure: Failure) -> Self {
        Self {
            status: ObservationStatus::Unavailable,
            value: None,
            reason: None,
            error: Some(failure),
            evidence: Vec::new(),
        }
    }

    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::without_value(ObservationStatus::Unsupported, reason)
    }

    #[must_use]
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self::without_value(ObservationStatus::Unknown, reason)
    }

    #[must_use]
    pub fn not_applicable(reason: impl Into<String>) -> Self {
        Self::without_value(ObservationStatus::NotApplicable, reason)
    }

    #[must_use]
    pub fn redacted(reason: impl Into<String>) -> Self {
        Self::without_value(ObservationStatus::Redacted, reason)
    }

    #[must_use]
    pub fn with_evidence(mut self, source: impl Into<String>, detail: impl Into<String>) -> Self {
        self.evidence.push(Evidence {
            source: source.into(),
            detail: detail.into(),
        });
        self
    }

    fn without_value(status: ObservationStatus, reason: impl Into<String>) -> Self {
        Self {
            status,
            value: None,
            reason: Some(reason.into()),
            error: None,
            evidence: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnvironmentObservation {
    pub host: HostObservation,
    pub kernel: KernelObservation,
    pub memory: MemoryObservation,
    pub resources: ResourceObservation,
    pub subsystems: SubsystemObservation,
    pub process: ProcessObservation,
    pub restrictions: RestrictionObservation,
    pub isolation: IsolationObservation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HostObservation {
    pub os: Observation<String>,
    pub architecture: Observation<String>,
    pub hostname: Observation<String>,
    pub distro: Observation<DistroObservation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DistroObservation {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version_id: Option<String>,
    pub pretty_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct KernelObservation {
    pub name: Observation<String>,
    pub release: Observation<String>,
    pub version: Observation<String>,
    pub command_line: Observation<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemoryObservation {
    pub physical_bytes: Observation<u64>,
    pub available_bytes: Observation<u64>,
    pub swap_total_bytes: Observation<u64>,
    pub swap_free_bytes: Observation<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResourceObservation {
    pub cpu_online: Observation<u32>,
    pub cpu_allowed_list: Observation<String>,
    pub uptime_seconds: Observation<f64>,
    pub load_average: Observation<[f64; 3]>,
    pub pressure: PressureObservation,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PressureObservation {
    pub scope: PressureScope,
    pub cpu: Observation<PressureResource>,
    pub memory: Observation<PressureResource>,
    pub io: Observation<PressureResource>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureScope {
    VisibleSystem,
    VisibleCurrentCgroup,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PressureResource {
    pub some: PressureStall,
    pub full: Option<PressureStall>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PressureStall {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total_us: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SubsystemObservation {
    pub procfs: Observation<bool>,
    pub sysfs: Observation<bool>,
    pub cgroup_v2: Observation<bool>,
    pub systemd: Observation<bool>,
    pub dbus: Observation<bool>,
    pub porto: Observation<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessObservation {
    pub pid: Observation<u32>,
    pub ppid: Observation<u32>,
    pub uid: Observation<u32>,
    pub effective_uid: Observation<u32>,
    pub gid: Observation<u32>,
    pub effective_gid: Observation<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RestrictionObservation {
    pub capability_sets: CapabilitySetsObservation,
    pub no_new_privileges: Observation<bool>,
    pub seccomp_mode: Observation<u32>,
    pub seccomp_filter_count: Observation<u32>,
    pub lsm_profile: Observation<String>,
    pub uid_map: Observation<Vec<IdMapRange>>,
    pub gid_map: Observation<Vec<IdMapRange>>,
    pub namespaces: NamespaceObservation,
    pub cgroups: CgroupObservation,
}

/// A self UID/GID range mapped from the collector's user namespace to its parent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IdMapRange {
    pub namespace_start: u32,
    pub parent_namespace_start: u32,
    pub length: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilitySetsObservation {
    pub inheritable: Observation<Vec<String>>,
    pub permitted: Observation<Vec<String>>,
    pub effective: Observation<Vec<String>>,
    pub bounding: Observation<Vec<String>>,
    pub ambient: Observation<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NamespaceObservation {
    pub cgroup: Observation<String>,
    pub ipc: Observation<String>,
    pub mnt: Observation<String>,
    pub net: Observation<String>,
    pub pid: Observation<String>,
    pub time: Observation<String>,
    pub user: Observation<String>,
    pub uts: Observation<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CgroupObservation {
    pub membership: Observation<Vec<CgroupMembership>>,
    pub limits: CgroupLimitObservation,
    pub accounting: CgroupAccountingObservation,
    pub pressure: PressureObservation,
}

/// Membership in the observed process's visible cgroup namespace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CgroupMembership {
    #[serde(flatten)]
    pub hierarchy: CgroupHierarchy,
    /// Linux byte string with display text and optional lossless `raw_base64`.
    pub namespace_relative_path: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "hierarchy_kind", rename_all = "snake_case")]
pub enum CgroupHierarchy {
    Unified,
    Legacy {
        hierarchy_id: u32,
        controllers: Vec<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CgroupAccountingObservation {
    pub cpu_stat: Observation<BTreeMap<String, u64>>,
    pub memory_events: Observation<BTreeMap<String, u64>>,
    pub pids_events: Observation<BTreeMap<String, u64>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CgroupLimitObservation {
    #[serde(rename = "cpu.max")]
    pub cpu_max: Observation<String>,
    #[serde(rename = "memory.current")]
    pub memory_current: Observation<String>,
    #[serde(rename = "memory.max")]
    pub memory_max: Observation<String>,
    #[serde(rename = "pids.current")]
    pub pids_current: Observation<String>,
    #[serde(rename = "pids.max")]
    pub pids_max: Observation<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IsolationObservation {
    pub interpretation: &'static str,
    pub virtual_machine: Observation<IsolationDetection>,
    pub container: Observation<IsolationDetection>,
    pub sandbox: Observation<IsolationDetection>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IsolationDetection {
    pub verdict: IsolationVerdict,
    pub provider: String,
    pub confidence: Confidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationVerdict {
    IndicatorPresent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[must_use]
pub fn observe() -> EnvironmentObservation {
    #[cfg(target_os = "linux")]
    {
        observe_linux(Path::new("/"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        observe_portable()
    }
}

#[cfg(not(target_os = "linux"))]
fn linux_only<T>() -> Observation<T> {
    Observation::unsupported("Linux-specific observation")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn portable_unknown<T>() -> Observation<T> {
    Observation::unknown("no passive platform evidence source is implemented")
}

#[cfg(not(target_os = "linux"))]
fn unsupported_capability_sets(reason: &str) -> CapabilitySetsObservation {
    CapabilitySetsObservation {
        inheritable: Observation::unsupported(reason),
        permitted: Observation::unsupported(reason),
        effective: Observation::unsupported(reason),
        bounding: Observation::unsupported(reason),
        ambient: Observation::unsupported(reason),
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_namespaces(reason: &str) -> NamespaceObservation {
    NamespaceObservation {
        cgroup: Observation::unsupported(reason),
        ipc: Observation::unsupported(reason),
        mnt: Observation::unsupported(reason),
        net: Observation::unsupported(reason),
        pid: Observation::unsupported(reason),
        time: Observation::unsupported(reason),
        user: Observation::unsupported(reason),
        uts: Observation::unsupported(reason),
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_cgroups(reason: &str) -> CgroupObservation {
    CgroupObservation {
        membership: Observation::unsupported(reason),
        limits: CgroupLimitObservation {
            cpu_max: Observation::unsupported(reason),
            memory_current: Observation::unsupported(reason),
            memory_max: Observation::unsupported(reason),
            pids_current: Observation::unsupported(reason),
            pids_max: Observation::unsupported(reason),
        },
        accounting: CgroupAccountingObservation {
            cpu_stat: Observation::unsupported(reason),
            memory_events: Observation::unsupported(reason),
            pids_events: Observation::unsupported(reason),
        },
        pressure: unsupported_pressure(reason, PressureScope::VisibleCurrentCgroup),
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_pressure(reason: &str, scope: PressureScope) -> PressureObservation {
    PressureObservation {
        scope,
        cpu: Observation::unsupported(reason),
        memory: Observation::unsupported(reason),
        io: Observation::unsupported(reason),
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_resources() -> ResourceObservation {
    ResourceObservation {
        cpu_online: linux_only(),
        cpu_allowed_list: linux_only(),
        uptime_seconds: linux_only(),
        load_average: linux_only(),
        pressure: unsupported_pressure("Linux-specific observation", PressureScope::VisibleSystem),
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_subsystems() -> SubsystemObservation {
    SubsystemObservation {
        procfs: linux_only(),
        sysfs: linux_only(),
        cgroup_v2: linux_only(),
        systemd: linux_only(),
        dbus: linux_only(),
        porto: linux_only(),
    }
}

#[cfg(target_os = "linux")]
fn observe_linux(root: &Path) -> EnvironmentObservation {
    let proc = root.join("proc");
    let etc = root.join("etc");
    let sys = root.join("sys");
    let status = read_text(proc.join("self/status"));
    let status_fields = status
        .as_ref()
        .map(|text| parse_colon_fields(text))
        .unwrap_or_default();

    let hostname = read_observation(proc.join("sys/kernel/hostname"));
    let distro = observe_distro(&etc);
    let memory = observe_memory(&proc);
    let resources = observe_resources(&proc, &sys, &status_fields, status.as_ref().err());
    let subsystems = observe_subsystems(root, &proc, &sys);
    let process = ProcessObservation {
        pid: Observation::available(std::process::id()),
        ppid: numeric_status(&status_fields, "PPid"),
        uid: indexed_status(&status_fields, "Uid", 0),
        effective_uid: indexed_status(&status_fields, "Uid", 1),
        gid: indexed_status(&status_fields, "Gid", 0),
        effective_gid: indexed_status(&status_fields, "Gid", 1),
    };

    let restrictions = observe_restrictions(&proc, &sys, &status_fields);
    let isolation = observe_isolation(
        root,
        &proc,
        &sys,
        &status_fields,
        restrictions
            .cgroups
            .membership
            .value
            .as_deref()
            .unwrap_or_default(),
    );

    EnvironmentObservation {
        host: HostObservation {
            os: Observation::available(std::env::consts::OS.to_owned()),
            architecture: Observation::available(std::env::consts::ARCH.to_owned()),
            hostname,
            distro,
        },
        kernel: KernelObservation {
            name: read_observation(proc.join("sys/kernel/ostype")),
            release: read_observation(proc.join("sys/kernel/osrelease")),
            version: read_observation(proc.join("version")),
            command_line: read_observation(proc.join("cmdline")),
        },
        memory,
        resources,
        subsystems,
        process,
        restrictions,
        isolation,
    }
}

#[cfg(target_os = "macos")]
fn observe_portable() -> EnvironmentObservation {
    let unknown_memory =
        || Observation::unknown("no passive memory evidence source is implemented for macOS");
    let uname = nix::sys::utsname::uname();
    let uname_field = |field: fn(&nix::sys::utsname::UtsName) -> &std::ffi::OsStr| match &uname {
        Ok(value) => Observation::available(field(value).to_string_lossy().into_owned())
            .with_evidence("uname", "kernel supplied value"),
        Err(error) => Observation::unavailable(Failure {
            operation: "read uname".into(),
            class: "os".into(),
            code: Some(error.to_string()),
            errno: None,
            message: error.to_string(),
        }),
    };

    EnvironmentObservation {
        host: HostObservation {
            os: Observation::available(std::env::consts::OS.to_owned()),
            architecture: Observation::available(std::env::consts::ARCH.to_owned()),
            hostname: uname_field(nix::sys::utsname::UtsName::nodename),
            distro: Observation::not_applicable("distribution metadata is not defined for macOS"),
        },
        kernel: KernelObservation {
            name: uname_field(nix::sys::utsname::UtsName::sysname),
            release: uname_field(nix::sys::utsname::UtsName::release),
            version: uname_field(nix::sys::utsname::UtsName::version),
            command_line: linux_only(),
        },
        memory: MemoryObservation {
            physical_bytes: unknown_memory(),
            available_bytes: unknown_memory(),
            swap_total_bytes: linux_only(),
            swap_free_bytes: linux_only(),
        },
        resources: unsupported_resources(),
        subsystems: unsupported_subsystems(),
        process: ProcessObservation {
            pid: Observation::available(std::process::id()),
            ppid: Observation::available(nix::unistd::getppid().as_raw() as u32),
            uid: Observation::available(nix::unistd::getuid().as_raw()),
            effective_uid: Observation::available(nix::unistd::geteuid().as_raw()),
            gid: Observation::available(nix::unistd::getgid().as_raw()),
            effective_gid: Observation::available(nix::unistd::getegid().as_raw()),
        },
        restrictions: RestrictionObservation {
            capability_sets: unsupported_capability_sets("Linux-specific observation"),
            no_new_privileges: linux_only(),
            seccomp_mode: linux_only(),
            seccomp_filter_count: linux_only(),
            lsm_profile: linux_only(),
            uid_map: linux_only(),
            gid_map: linux_only(),
            namespaces: unsupported_namespaces("Linux-specific observation"),
            cgroups: unsupported_cgroups("Linux-specific observation"),
        },
        isolation: IsolationObservation {
            interpretation: "heuristic_indicator",
            virtual_machine: unknown_isolation("no direct virtual-machine evidence was available"),
            container: unknown_isolation("no direct container evidence was available"),
            sandbox: unknown_isolation("no direct sandbox evidence was available"),
        },
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn observe_portable() -> EnvironmentObservation {
    EnvironmentObservation {
        host: HostObservation {
            os: Observation::available(std::env::consts::OS.to_owned()),
            architecture: Observation::available(std::env::consts::ARCH.to_owned()),
            hostname: portable_unknown(),
            distro: Observation::not_applicable("distribution metadata is not defined for this OS"),
        },
        kernel: KernelObservation {
            name: portable_unknown(),
            release: portable_unknown(),
            version: portable_unknown(),
            command_line: linux_only(),
        },
        memory: MemoryObservation {
            physical_bytes: portable_unknown(),
            available_bytes: portable_unknown(),
            swap_total_bytes: linux_only(),
            swap_free_bytes: linux_only(),
        },
        resources: unsupported_resources(),
        subsystems: unsupported_subsystems(),
        process: ProcessObservation {
            pid: Observation::available(std::process::id()),
            ppid: portable_unknown(),
            uid: portable_unknown(),
            effective_uid: portable_unknown(),
            gid: portable_unknown(),
            effective_gid: portable_unknown(),
        },
        restrictions: RestrictionObservation {
            capability_sets: unsupported_capability_sets("Linux-specific observation"),
            no_new_privileges: linux_only(),
            seccomp_mode: linux_only(),
            seccomp_filter_count: linux_only(),
            lsm_profile: linux_only(),
            uid_map: linux_only(),
            gid_map: linux_only(),
            namespaces: unsupported_namespaces("Linux-specific observation"),
            cgroups: unsupported_cgroups("Linux-specific observation"),
        },
        isolation: IsolationObservation {
            interpretation: "heuristic_indicator",
            virtual_machine: unknown_isolation("no direct virtual-machine evidence was available"),
            container: unknown_isolation("no direct container evidence was available"),
            sandbox: unknown_isolation("no direct sandbox evidence was available"),
        },
    }
}

#[cfg(target_os = "linux")]
fn observe_distro(etc: &Path) -> Observation<DistroObservation> {
    for path in [etc.join("os-release"), etc.join("../usr/lib/os-release")] {
        match read_text(&path) {
            Ok(text) => {
                let fields = parse_equals_fields(&text);
                return Observation::available(DistroObservation {
                    id: fields.get("ID").cloned(),
                    name: fields.get("NAME").cloned(),
                    version_id: fields.get("VERSION_ID").cloned(),
                    pretty_name: fields.get("PRETTY_NAME").cloned(),
                })
                .with_evidence(path.display().to_string(), "parsed os-release");
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return unavailable_read(path, error),
        }
    }

    Observation::absent("neither /etc/os-release nor /usr/lib/os-release exists")
}

#[cfg(target_os = "linux")]
fn observe_memory(proc: &Path) -> MemoryObservation {
    let path = proc.join("meminfo");
    let text = match read_text(&path) {
        Ok(text) => text,
        Err(error) => {
            return MemoryObservation {
                physical_bytes: unavailable_read(&path, clone_io_error(&error)),
                available_bytes: unavailable_read(&path, clone_io_error(&error)),
                swap_total_bytes: unavailable_read(&path, clone_io_error(&error)),
                swap_free_bytes: unavailable_read(path, error),
            };
        }
    };
    let fields = parse_colon_fields(&text);

    MemoryObservation {
        physical_bytes: memory_field(&fields, "MemTotal"),
        available_bytes: memory_field(&fields, "MemAvailable"),
        swap_total_bytes: swap_field(&fields, "SwapTotal"),
        swap_free_bytes: swap_field(&fields, "SwapFree"),
    }
}

#[cfg(target_os = "linux")]
fn observe_resources(
    proc: &Path,
    sys: &Path,
    status: &BTreeMap<String, String>,
    status_error: Option<&io::Error>,
) -> ResourceObservation {
    let cpu_allowed_list = if let Some(error) = status_error {
        unavailable_read(proc.join("self/status"), clone_io_error(error))
    } else {
        match status.get("Cpus_allowed_list") {
            Some(raw)
                if raw.len() <= SUMMARY_SOURCE_LIMIT as usize && cpu_list_count(raw).is_some() =>
            {
                Observation::available(raw.clone())
                    .with_evidence("/proc/self/status", "Cpus_allowed_list")
            }
            Some(_) => {
                Observation::unknown("Cpus_allowed_list is malformed or exceeds the source limit")
            }
            None => Observation::unknown("Cpus_allowed_list is missing from /proc/self/status"),
        }
    };

    ResourceObservation {
        cpu_online: parse_observation(
            read_required_summary(sys.join("devices/system/cpu/online")),
            |text| cpu_list_count(text),
            "online CPU list",
        ),
        cpu_allowed_list,
        uptime_seconds: parse_observation(
            read_required_summary(proc.join("uptime")),
            |text| nonnegative_number(text.split_ascii_whitespace().next()?),
            "uptime",
        ),
        load_average: parse_observation(
            read_required_summary(proc.join("loadavg")),
            |text| {
                let mut fields = text.split_ascii_whitespace();
                Some([
                    nonnegative_number(fields.next()?)?,
                    nonnegative_number(fields.next()?)?,
                    nonnegative_number(fields.next()?)?,
                ])
            },
            "load averages",
        ),
        pressure: observe_pressure(&proc.join("pressure")),
    }
}

#[cfg(target_os = "linux")]
fn observe_pressure(root: &Path) -> PressureObservation {
    let source = |resource: &str| {
        parse_observation(
            read_observation_with_limit(root.join(resource), SUMMARY_SOURCE_LIMIT),
            |text| parse_pressure(text),
            "pressure stalls",
        )
    };
    PressureObservation {
        scope: PressureScope::VisibleSystem,
        cpu: source("cpu"),
        memory: source("memory"),
        io: source("io"),
    }
}

#[cfg(target_os = "linux")]
fn observe_subsystems(root: &Path, proc: &Path, sys: &Path) -> SubsystemObservation {
    use std::os::unix::fs::FileTypeExt;

    SubsystemObservation {
        procfs: observe_presence(proc, fs::FileType::is_dir, "visible proc directory"),
        sysfs: observe_presence(sys, fs::FileType::is_dir, "visible sys directory"),
        cgroup_v2: observe_presence(
            &sys.join("fs/cgroup/cgroup.controllers"),
            fs::FileType::is_file,
            "default cgroup v2 controllers marker",
        ),
        systemd: observe_presence(
            &root.join("run/systemd/system"),
            fs::FileType::is_dir,
            "systemd runtime directory marker; manager reachability is not tested",
        ),
        dbus: observe_presence(
            &root.join("run/dbus/system_bus_socket"),
            fs::FileType::is_socket,
            "default system bus socket; reachability is not tested",
        ),
        porto: observe_presence(
            &root.join(crate::cli::PORTO_SOCKET_PATH.trim_start_matches('/')),
            fs::FileType::is_socket,
            "default Porto socket; reachability is not tested",
        ),
    }
}

#[cfg(target_os = "linux")]
fn observe_presence(
    path: &Path,
    expected: fn(&fs::FileType) -> bool,
    detail: &str,
) -> Observation<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Observation::available(expected(&metadata.file_type()))
            .with_evidence(path.display().to_string(), detail),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Observation::absent(format!(
            "{} is not present in the collector view",
            path.display()
        ))
        .with_evidence(path.display().to_string(), detail),
        Err(error) => unavailable_read(path, error),
    }
}

#[cfg(target_os = "linux")]
fn observe_restrictions(
    proc: &Path,
    sys: &Path,
    status: &BTreeMap<String, String>,
) -> RestrictionObservation {
    RestrictionObservation {
        capability_sets: CapabilitySetsObservation {
            inheritable: capability_set(status, "CapInh"),
            permitted: capability_set(status, "CapPrm"),
            effective: capability_set(status, "CapEff"),
            bounding: capability_set(status, "CapBnd"),
            ambient: capability_set(status, "CapAmb"),
        },
        no_new_privileges: bool_status(status, "NoNewPrivs"),
        seccomp_mode: numeric_status(status, "Seccomp"),
        seccomp_filter_count: numeric_status(status, "Seccomp_filters"),
        lsm_profile: first_read_observation(&[
            proc.join("self/attr/current"),
            sys.join("kernel/security/lsm"),
        ]),
        uid_map: parse_observation(
            read_observation(proc.join("self/uid_map")),
            |text| parse_id_map(text),
            "UID mapping",
        ),
        gid_map: parse_observation(
            read_observation(proc.join("self/gid_map")),
            |text| parse_id_map(text),
            "GID mapping",
        ),
        namespaces: observe_namespaces(proc),
        cgroups: observe_cgroups(proc, sys),
    }
}

#[cfg(target_os = "linux")]
fn capability_set(status: &BTreeMap<String, String>, field: &str) -> Observation<Vec<String>> {
    let Some(raw) = status.get(field) else {
        return Observation::unknown(format!("{field} is missing from /proc/self/status"));
    };
    let Ok(bits) = u64::from_str_radix(raw, 16) else {
        return Observation::unknown(format!("{field} is malformed in /proc/self/status"));
    };

    Observation::available(decode_capabilities(bits))
        .with_evidence("/proc/self/status", format!("decoded {field}"))
}

#[cfg(target_os = "linux")]
fn observe_namespaces(proc: &Path) -> NamespaceObservation {
    let root = proc.join("self/ns");
    NamespaceObservation {
        cgroup: namespace_observation(&root, "cgroup"),
        ipc: namespace_observation(&root, "ipc"),
        mnt: namespace_observation(&root, "mnt"),
        net: namespace_observation(&root, "net"),
        pid: namespace_observation(&root, "pid"),
        time: namespace_observation(&root, "time"),
        user: namespace_observation(&root, "user"),
        uts: namespace_observation(&root, "uts"),
    }
}

#[cfg(target_os = "linux")]
fn namespace_observation(root: &Path, name: &str) -> Observation<String> {
    let path = root.join(name);
    match fs::read_link(&path) {
        Ok(target) => Observation::available(target.to_string_lossy().into_owned())
            .with_evidence(path.display().to_string(), "read namespace ID"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Observation::absent(format!("{} does not exist", path.display()))
        }
        Err(error) => unavailable_read(path, error),
    }
}

#[cfg(target_os = "linux")]
fn observe_cgroups(proc: &Path, sys: &Path) -> CgroupObservation {
    let membership_path = proc.join("self/cgroup");
    let raw_membership = match read_bytes_with_limit(&membership_path, TEXT_SOURCE_LIMIT) {
        Ok(bytes) => Observation::available(bytes).with_evidence(
            membership_path.display().to_string(),
            "read cgroup membership",
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Observation::absent(format!("{} does not exist", membership_path.display()))
        }
        Err(error) => unavailable_read(&membership_path, error),
    };
    let Observation {
        status,
        value: bytes,
        reason,
        error,
        evidence,
    } = raw_membership;
    // Mount lookup borrows the original path bytes, not its encoded wire representation.
    let membership = parse_observation(
        Observation {
            status,
            value: bytes.as_deref(),
            reason,
            error,
            evidence,
        },
        |bytes| parse_cgroup_membership(bytes).ok(),
        "cgroup membership",
    );
    let unified_root = visible_cgroup_root(
        proc,
        &membership,
        bytes.as_deref().unwrap_or_default(),
        sys.parent().unwrap_or_else(|| Path::new("/")),
    );
    let accounting = |name: &str| {
        parse_observation(
            cgroup_file(&unified_root, name, SUMMARY_SOURCE_LIMIT),
            |text| parse_accounting(text),
            "cgroup counters",
        )
    };
    let pressure = |name: &str| {
        parse_observation(
            cgroup_file(&unified_root, name, SUMMARY_SOURCE_LIMIT),
            |text| parse_pressure(text),
            "cgroup pressure stalls",
        )
    };

    CgroupObservation {
        membership,
        limits: CgroupLimitObservation {
            cpu_max: cgroup_file(&unified_root, "cpu.max", TEXT_SOURCE_LIMIT),
            memory_current: cgroup_file(&unified_root, "memory.current", TEXT_SOURCE_LIMIT),
            memory_max: cgroup_file(&unified_root, "memory.max", TEXT_SOURCE_LIMIT),
            pids_current: cgroup_file(&unified_root, "pids.current", TEXT_SOURCE_LIMIT),
            pids_max: cgroup_file(&unified_root, "pids.max", TEXT_SOURCE_LIMIT),
        },
        accounting: CgroupAccountingObservation {
            cpu_stat: accounting("cpu.stat"),
            memory_events: accounting("memory.events"),
            pids_events: accounting("pids.events"),
        },
        pressure: PressureObservation {
            scope: PressureScope::VisibleCurrentCgroup,
            cpu: pressure("cpu.pressure"),
            memory: pressure("memory.pressure"),
            io: pressure("io.pressure"),
        },
    }
}

#[cfg(target_os = "linux")]
fn visible_cgroup_root(
    proc: &Path,
    membership: &Observation<Vec<CgroupMembership>>,
    bytes: &[u8],
    collector_root: &Path,
) -> Observation<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let Some(entries) = membership.value.as_deref() else {
        return missing_observation(membership);
    };
    let mut unified = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(&entry.hierarchy, CgroupHierarchy::Unified));
    let Some((index, _)) = unified.next() else {
        return Observation::not_applicable("cgroup v2 membership was not observed");
    };
    let raw_path = bytes
        .split(|byte| *byte == b'\n')
        .nth(index)
        .and_then(|line| line.splitn(3, |byte| *byte == b':').nth(2))
        .expect("parsed cgroup membership contains its original path");
    let current = Path::new(std::ffi::OsStr::from_bytes(raw_path));
    if unified.next().is_some() || !safe_absolute_path(current) {
        return Observation::unknown(
            "cgroup v2 membership is malformed or outside the visible namespace",
        )
        .with_evidence(
            proc.join("self/cgroup").display().to_string(),
            "membership is not a safe absolute path",
        );
    }
    let mountinfo_path = proc.join("self/mountinfo");
    let mountinfo = match read_bytes_with_limit(&mountinfo_path, TEXT_SOURCE_LIMIT) {
        Ok(text) => text,
        Err(error) => return unavailable_read(mountinfo_path, error),
    };
    let mut selected = None;
    let mut unified_mount_seen = false;
    for line in mountinfo.split(|byte| *byte == b'\n') {
        let Some(separator) = line.windows(3).position(|bytes| bytes == b" - ") else {
            continue;
        };
        let (mount, filesystem) = (&line[..separator], &line[separator + 3..]);
        if filesystem
            .split(u8::is_ascii_whitespace)
            .find(|field| !field.is_empty())
            != Some(b"cgroup2".as_slice())
        {
            continue;
        }
        unified_mount_seen = true;
        let mut fields = mount
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty());
        let mount_root = fields.nth(3).and_then(decode_mount_path);
        let mountpoint = fields.next().and_then(decode_mount_path);
        let (Some(mount_root), Some(mountpoint)) = (mount_root, mountpoint) else {
            continue;
        };
        if !safe_absolute_path(&mount_root) || !safe_absolute_path(&mountpoint) {
            continue;
        }
        let Ok(relative) = current.strip_prefix(&mount_root) else {
            continue;
        };
        let depth = mount_root.components().count();
        if selected
            .as_ref()
            .is_none_or(|(selected_depth, _)| depth >= *selected_depth)
        {
            selected = Some((depth, mountpoint.join(relative)));
        }
    }
    match selected {
        Some((_, path)) if safe_absolute_path(&path) => {
            let visible =
                collector_root.join(path.strip_prefix("/").expect("absolute cgroup path"));
            Observation::available(visible).with_evidence(
                mountinfo_path.display().to_string(),
                "current visible cgroup v2 directory; ancestor limits are not evaluated",
            )
        }
        _ if !unified_mount_seen => {
            Observation::absent("no cgroup v2 mount is visible to the collector")
        }
        _ => Observation::unknown(
            "current cgroup v2 membership cannot be located within a visible mount",
        )
        .with_evidence(
            mountinfo_path.display().to_string(),
            "no safe containing cgroup v2 mount matched",
        ),
    }
}

#[cfg(target_os = "linux")]
fn safe_absolute_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    path.is_absolute()
        && path.as_os_str().as_bytes().len() <= 4096
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

#[cfg(target_os = "linux")]
fn decode_mount_path(bytes: &[u8]) -> Option<std::borrow::Cow<'_, Path>> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    if bytes.len() > 4 * 4096 {
        return None;
    }
    if !bytes.contains(&b'\\') {
        return Some(std::borrow::Cow::Borrowed(Path::new(
            std::ffi::OsStr::from_bytes(bytes),
        )));
    }
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let escaped = bytes.get(index + 1..index + 4)?;
            decoded.push(match escaped {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => return None,
            });
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(std::borrow::Cow::Owned(PathBuf::from(
        std::ffi::OsString::from_vec(decoded),
    )))
}

#[cfg(target_os = "linux")]
fn missing_observation<T, U>(source: &Observation<U>) -> Observation<T> {
    Observation {
        status: source.status.clone(),
        value: None,
        reason: source.reason.clone(),
        error: source.error.clone(),
        evidence: source.evidence.clone(),
    }
}

#[cfg(target_os = "linux")]
fn cgroup_file(root: &Observation<PathBuf>, name: &str, limit: u64) -> Observation<String> {
    let Some(path) = root.value.as_deref() else {
        return missing_observation(root);
    };
    let mut observation = read_observation_with_limit(path.join(name), limit);
    if let Some(evidence) = observation.evidence.first_mut() {
        evidence.detail = "current visible cgroup value; ancestor limits are not evaluated".into();
    }
    observation
}

#[cfg(target_os = "linux")]
fn observe_isolation(
    root: &Path,
    proc: &Path,
    sys: &Path,
    status: &BTreeMap<String, String>,
    cgroup: &[CgroupMembership],
) -> IsolationObservation {
    let ancestry = process_ancestry(proc);
    let ancestry_text = ancestry.join(" ").to_ascii_lowercase();
    let apparmor = read_text(proc.join("self/attr/current")).unwrap_or_default();
    let container_env = std::env::var("container").ok();
    let container = observe_container_isolation(
        root,
        proc,
        cgroup,
        container_env.as_deref(),
        std::env::var_os("PORTO_NAME").is_some(),
        std::env::var_os("PORTO_HOST").is_some(),
    );

    let mut sandbox_evidence = Vec::new();
    let sandbox_provider =
        if ancestry_text.contains("bubblewrap") || ancestry_text.contains("bwrap") {
            sandbox_evidence.push(Evidence {
                source: "/proc ancestry".into(),
                detail: ancestry.join(" -> "),
            });
            Some(("bubblewrap", Confidence::High))
        } else if std::env::var_os("FLATPAK_ID").is_some() {
            sandbox_evidence.push(Evidence {
                source: "FLATPAK_ID".into(),
                detail: "exported marker is set".into(),
            });
            Some(("flatpak", Confidence::High))
        } else if namespace_differs(proc, "mnt") || namespace_differs(proc, "user") {
            sandbox_evidence.push(Evidence {
                source: "/proc/*/ns".into(),
                detail: "self namespace differs from PID 1".into(),
            });
            Some(("generic_namespace_sandbox", Confidence::Medium))
        } else if status.get("Seccomp").is_some_and(|mode| mode != "0") {
            sandbox_evidence.push(Evidence {
                source: "/proc/self/status".into(),
                detail: format!(
                    "Seccomp={}",
                    status.get("Seccomp").unwrap_or(&String::new())
                ),
            });
            Some(("generic_namespace_sandbox", Confidence::Low))
        } else if !apparmor.trim().is_empty() && apparmor.trim() != "unconfined" {
            sandbox_evidence.push(Evidence {
                source: "/proc/self/attr/current".into(),
                detail: apparmor.trim().to_owned(),
            });
            Some(("linux_lsm_sandbox", Confidence::Medium))
        } else {
            None
        };

    let mut vm_sources = [
        sys.join("class/dmi/id/sys_vendor"),
        sys.join("class/dmi/id/product_name"),
        sys.join("hypervisor/type"),
        proc.join("cpuinfo"),
    ]
    .into_iter()
    .filter_map(|path| {
        read_text(&path)
            .ok()
            .map(|value| (path.display().to_string(), value.to_ascii_lowercase()))
    })
    .collect::<Vec<_>>();
    vm_sources.sort_by(|left, right| left.0.cmp(&right.0));

    let vm_match = [
        ("kvm_qemu", ["kvm", "qemu"].as_slice()),
        ("vmware", ["vmware"].as_slice()),
        ("hyper_v", ["microsoft hv", "hyper-v"].as_slice()),
        ("xen", ["xen"].as_slice()),
        ("parallels", ["parallels"].as_slice()),
    ]
    .into_iter()
    .find_map(|(provider, needles)| {
        vm_sources.iter().find_map(|(source, value)| {
            needles
                .iter()
                .find(|needle| value.contains(**needle))
                .map(|needle| {
                    (
                        provider,
                        Evidence {
                            source: source.clone(),
                            detail: format!("matched marker {needle}"),
                        },
                    )
                })
        })
    });

    let (vm_provider, vm_evidence) = vm_match.map_or_else(
        || (None, Vec::new()),
        |(provider, evidence)| (Some((provider, Confidence::High)), vec![evidence]),
    );

    IsolationObservation {
        interpretation: "heuristic_indicator",
        virtual_machine: isolation_observation(
            vm_provider,
            vm_evidence,
            "no strong VM signature matched",
        ),
        container,
        sandbox: isolation_observation(
            sandbox_provider,
            sandbox_evidence,
            "no matching sandbox indicators",
        ),
    }
}

#[cfg(target_os = "linux")]
fn observe_container_isolation(
    root: &Path,
    proc: &Path,
    cgroup: &[CgroupMembership],
    container_env: Option<&str>,
    porto_name: bool,
    porto_host: bool,
) -> Observation<IsolationDetection> {
    let portoinit = read_text(proc.join("1/comm"))
        .is_ok_and(|comm| comm.trim().eq_ignore_ascii_case("portoinit"));
    let portod_socket = root.join("run/portod.socket").exists();
    let mut evidence = Vec::new();
    let cgroup_contains = |marker: &str| {
        cgroup.iter().any(|membership| {
            membership.namespace_relative_path["display"]
                .as_str()
                .is_some_and(|path| path.contains(marker))
        })
    };

    if portoinit {
        evidence.push(Evidence {
            source: "/proc/1/comm".into(),
            detail: "portoinit".into(),
        });
    }
    if porto_name {
        evidence.push(Evidence {
            source: "PORTO_NAME".into(),
            detail: "environment variable is set".into(),
        });
    }
    if porto_host {
        evidence.push(Evidence {
            source: "PORTO_HOST".into(),
            detail: "environment variable is set".into(),
        });
    }
    if portoinit || porto_name || porto_host {
        if portod_socket {
            evidence.push(Evidence {
                source: "/run/portod.socket".into(),
                detail: "Porto daemon socket is present".into(),
            });
        }
        return isolation_observation(
            Some(("porto", Confidence::High)),
            evidence,
            "no strong container signature matched",
        );
    }

    let provider = if root.join(".dockerenv").exists() || cgroup_contains("docker") {
        evidence.push(Evidence {
            source: "marker/cgroup".into(),
            detail: "Docker marker or cgroup component".into(),
        });
        Some(("docker", Confidence::High))
    } else if root.join("run/.containerenv").exists() || cgroup_contains("libpod") {
        evidence.push(Evidence {
            source: "marker/cgroup".into(),
            detail: "Podman marker or libpod cgroup component".into(),
        });
        Some(("podman", Confidence::High))
    } else if cgroup_contains("kubepods") {
        evidence.push(Evidence {
            source: "/proc/self/cgroup".into(),
            detail: "kubepods cgroup component".into(),
        });
        Some(("kubernetes", Confidence::High))
    } else if cgroup_contains("lxc") {
        evidence.push(Evidence {
            source: "/proc/self/cgroup".into(),
            detail: "LXC cgroup component".into(),
        });
        Some(("lxc", Confidence::High))
    } else if let Some(provider) = container_env.filter(|value| !value.is_empty()) {
        evidence.push(Evidence {
            source: "container environment variable".into(),
            detail: provider.to_owned(),
        });
        Some((provider, Confidence::High))
    } else {
        None
    };

    let unknown_reason = if provider.is_none() && portod_socket {
        evidence.push(Evidence {
            source: "/run/portod.socket".into(),
            detail: "Porto daemon socket is present on both hosts and containers".into(),
        });
        "Porto socket alone does not distinguish a host from a container"
    } else {
        "no strong container signature matched"
    };

    isolation_observation(provider, evidence, unknown_reason)
}

#[cfg(target_os = "linux")]
fn isolation_observation(
    provider: Option<(&str, Confidence)>,
    evidence: Vec<Evidence>,
    unknown_reason: &str,
) -> Observation<IsolationDetection> {
    let mut observation = match provider {
        Some((provider, confidence)) => Observation::available(IsolationDetection {
            verdict: IsolationVerdict::IndicatorPresent,
            provider: provider.to_owned(),
            confidence,
        }),
        None => Observation::unknown(unknown_reason),
    };
    observation.evidence = evidence;
    observation
}

#[cfg(not(target_os = "linux"))]
fn unknown_isolation(reason: &str) -> Observation<IsolationDetection> {
    Observation::unknown(reason)
}

#[cfg(target_os = "linux")]
fn process_ancestry(proc: &Path) -> Vec<String> {
    let mut result = Vec::new();
    let mut pid = std::process::id();
    for _ in 0..32 {
        let root = proc.join(pid.to_string());
        if let Ok(comm) = read_text(root.join("comm")) {
            result.push(comm.trim().to_owned());
        }

        let Ok(status) = read_text(root.join("status")) else {
            break;
        };
        let fields = parse_colon_fields(&status);
        let Some(parent) = fields
            .get("PPid")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            break;
        };

        if parent == 0 || parent == pid {
            break;
        }
        pid = parent;
    }
    result
}

#[cfg(target_os = "linux")]
fn namespace_differs(proc: &Path, name: &str) -> bool {
    match (
        fs::read_link(proc.join("self/ns").join(name)),
        fs::read_link(proc.join("1/ns").join(name)),
    ) {
        (Ok(current), Ok(init)) => current != init,
        _ => false,
    }
}

#[cfg(target_os = "linux")]
fn decode_capabilities(bits: u64) -> Vec<String> {
    const NAMES: &[&str] = &[
        "chown",
        "dac_override",
        "dac_read_search",
        "fowner",
        "fsetid",
        "kill",
        "setgid",
        "setuid",
        "setpcap",
        "linux_immutable",
        "net_bind_service",
        "net_broadcast",
        "net_admin",
        "net_raw",
        "ipc_lock",
        "ipc_owner",
        "sys_module",
        "sys_rawio",
        "sys_chroot",
        "sys_ptrace",
        "sys_pacct",
        "sys_admin",
        "sys_boot",
        "sys_nice",
        "sys_resource",
        "sys_time",
        "sys_tty_config",
        "mknod",
        "lease",
        "audit_write",
        "audit_control",
        "setfcap",
        "mac_override",
        "mac_admin",
        "syslog",
        "wake_alarm",
        "block_suspend",
        "audit_read",
        "perfmon",
        "bpf",
        "checkpoint_restore",
    ];
    let mut result = Vec::new();
    for bit in 0..64 {
        if bits & (1_u64 << bit) != 0 {
            result.push(
                NAMES
                    .get(bit)
                    .map_or_else(|| format!("cap_{bit}"), |name| (*name).to_owned()),
            );
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn memory_field(fields: &BTreeMap<String, String>, name: &str) -> Observation<u64> {
    let mut observation = memory_kibibytes(fields, name);
    observation.value = observation.value.map(|value| value.saturating_mul(1024));
    observation
}

#[cfg(target_os = "linux")]
fn swap_field(fields: &BTreeMap<String, String>, name: &str) -> Observation<u64> {
    let mut observation = memory_kibibytes(fields, name);
    if let Some(kibibytes) = observation.value {
        match kibibytes.checked_mul(1024) {
            Some(bytes) => observation.value = Some(bytes),
            None => {
                observation.status = ObservationStatus::Unknown;
                observation.value = None;
                observation.reason = Some(format!("{name} overflows bytes in /proc/meminfo"));
            }
        }
    }
    observation
}

#[cfg(target_os = "linux")]
fn memory_kibibytes(fields: &BTreeMap<String, String>, name: &str) -> Observation<u64> {
    let Some(value) = fields.get(name) else {
        return Observation::unknown(format!("{name} is missing from /proc/meminfo"));
    };
    let mut parts = value.split_ascii_whitespace();

    let Some(kibibytes) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return Observation::unknown(format!("{name} is malformed in /proc/meminfo"));
    };

    if parts.next() != Some("kB") {
        return Observation::unknown(format!("{name} has an unsupported unit"));
    }

    Observation::available(kibibytes).with_evidence("/proc/meminfo", name.to_owned())
}

#[cfg(target_os = "linux")]
fn bool_status(fields: &BTreeMap<String, String>, name: &str) -> Observation<bool> {
    match fields.get(name).map(String::as_str) {
        Some("0") => Observation::available(false),
        Some("1") => Observation::available(true),
        Some(_) => Observation::unknown(format!("{name} is not 0 or 1")),
        None => Observation::unknown(format!("{name} is missing from /proc/self/status")),
    }
}

#[cfg(target_os = "linux")]
fn indexed_status(fields: &BTreeMap<String, String>, name: &str, index: usize) -> Observation<u32> {
    let Some(value) = fields
        .get(name)
        .and_then(|value| value.split_ascii_whitespace().nth(index))
        .and_then(|value| value.parse::<u32>().ok())
    else {
        return Observation::unknown(format!("{name}[{index}] is missing or malformed"));
    };

    Observation::available(value).with_evidence("/proc/self/status", name.to_owned())
}

#[cfg(target_os = "linux")]
fn numeric_status(fields: &BTreeMap<String, String>, name: &str) -> Observation<u32> {
    let Some(value) = fields.get(name).and_then(|value| value.parse::<u32>().ok()) else {
        return Observation::unknown(format!("{name} is missing or malformed"));
    };

    Observation::available(value).with_evidence("/proc/self/status", name.to_owned())
}

#[cfg(target_os = "linux")]
fn read_observation(path: PathBuf) -> Observation<String> {
    read_observation_with_limit(path, TEXT_SOURCE_LIMIT)
}

#[cfg(target_os = "linux")]
fn read_observation_with_limit(path: PathBuf, limit: u64) -> Observation<String> {
    match read_text_with_limit(&path, limit) {
        Ok(value) => Observation::available(value.trim().to_owned())
            .with_evidence(path.display().to_string(), "read text value"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Observation::absent(format!("{} does not exist", path.display()))
        }
        Err(error) => unavailable_read(path, error),
    }
}

#[cfg(target_os = "linux")]
fn read_required_summary(path: PathBuf) -> Observation<String> {
    match read_text_with_limit(&path, SUMMARY_SOURCE_LIMIT) {
        Ok(value) => Observation::available(value.trim().to_owned()).with_evidence(
            path.display().to_string(),
            "read required measurement source",
        ),
        Err(error) => unavailable_read(path, error),
    }
}

#[cfg(target_os = "linux")]
fn first_read_observation(paths: &[PathBuf]) -> Observation<String> {
    for path in paths {
        match read_text(path) {
            Ok(value) => {
                return Observation::available(value.trim().to_owned())
                    .with_evidence(path.display().to_string(), "read text value");
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return unavailable_read(path, error),
        }
    }
    Observation::absent("none of the observation sources exist")
}

#[cfg(target_os = "linux")]
fn unavailable_read<T>(path: impl AsRef<Path>, error: io::Error) -> Observation<T> {
    Observation::unavailable(Failure::from_io(
        format!("read {}", path.as_ref().display()),
        &error,
    ))
}

#[cfg(target_os = "linux")]
fn read_text(path: impl AsRef<Path>) -> io::Result<String> {
    read_text_with_limit(path.as_ref(), TEXT_SOURCE_LIMIT)
}

#[cfg(target_os = "linux")]
fn read_text_with_limit(path: &Path, limit: u64) -> io::Result<String> {
    String::from_utf8(read_bytes_with_limit(path, limit)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(target_os = "linux")]
fn read_bytes_with_limit(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(limit.min(64 << 10) as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;

    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("observation source exceeds {limit} byte limit"),
        ));
    }

    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn parse_observation<T, U>(
    source: Observation<U>,
    parse: impl FnOnce(&U) -> Option<T>,
    kind: &str,
) -> Observation<T> {
    let Observation {
        status,
        value,
        reason,
        error,
        evidence,
    } = source;
    match value {
        Some(source) => match parse(&source) {
            Some(value) => Observation {
                status: ObservationStatus::Available,
                value: Some(value),
                reason: None,
                error: None,
                evidence,
            },
            None => Observation {
                status: ObservationStatus::Unknown,
                value: None,
                reason: Some(format!("{kind} source is malformed")),
                error: None,
                evidence,
            },
        },
        None => Observation {
            status,
            value: None,
            reason,
            error,
            evidence,
        },
    }
}

#[cfg(target_os = "linux")]
fn decimal_u32(raw: &str) -> Option<u32> {
    raw.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| raw.parse().ok())
        .flatten()
}

#[cfg(target_os = "linux")]
fn parse_id_map(text: &str) -> Option<Vec<IdMapRange>> {
    text.lines()
        .map(|line| {
            let mut fields = line.split_ascii_whitespace();
            let namespace_start = decimal_u32(fields.next()?)?;
            let parent_namespace_start = decimal_u32(fields.next()?)?;
            let length = decimal_u32(fields.next()?)?;
            if fields.next().is_some()
                || length == 0
                || namespace_start.checked_add(length).is_none()
                || parent_namespace_start.checked_add(length).is_none()
            {
                return None;
            }
            Some(IdMapRange {
                namespace_start,
                parent_namespace_start,
                length,
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
pub(crate) fn parse_cgroup_membership(bytes: &[u8]) -> io::Result<Vec<CgroupMembership>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    bytes
        .strip_suffix(b"\n")
        .unwrap_or(bytes)
        .split(|byte| *byte == b'\n')
        .map(|line| {
            let malformed = || {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed cgroup membership entry",
                )
            };
            let mut fields = line.splitn(3, |byte| *byte == b':');
            let hierarchy_id = std::str::from_utf8(fields.next().ok_or_else(malformed)?)
                .ok()
                .and_then(decimal_u32)
                .ok_or_else(malformed)?;
            let controllers = std::str::from_utf8(fields.next().ok_or_else(malformed)?)
                .map_err(|_| malformed())?;
            let controllers = if controllers.is_empty() {
                Vec::new()
            } else {
                controllers
                    .split(',')
                    .map(|controller| {
                        if controller.is_empty()
                            || controller
                                .bytes()
                                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
                        {
                            return Err(malformed());
                        }
                        Ok(controller.to_owned())
                    })
                    .collect::<io::Result<Vec<_>>>()?
            };
            let path = fields.next().ok_or_else(malformed)?;
            if !path.starts_with(b"/") || path.contains(&0) {
                return Err(malformed());
            }
            Ok(CgroupMembership {
                hierarchy: if hierarchy_id == 0 && controllers.is_empty() {
                    CgroupHierarchy::Unified
                } else {
                    CgroupHierarchy::Legacy {
                        hierarchy_id,
                        controllers,
                    }
                },
                namespace_relative_path: crate::protocol::linux_bytes(path),
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn cpu_list_count(raw: &str) -> Option<u32> {
    let mut count = 0_u32;
    let mut previous = None;
    for range in raw.split(',') {
        let (first, last) = match range.split_once('-') {
            Some((first, last)) => (first.parse::<u32>().ok()?, last.parse::<u32>().ok()?),
            None => {
                let cpu = range.parse::<u32>().ok()?;
                (cpu, cpu)
            }
        };
        if last < first || previous.is_some_and(|previous| first <= previous) {
            return None;
        }
        count = count.checked_add(last.checked_sub(first)?.checked_add(1)?)?;
        previous = Some(last);
    }
    Some(count)
}

#[cfg(target_os = "linux")]
fn nonnegative_number(raw: &str) -> Option<f64> {
    let number = raw.parse::<f64>().ok()?;
    (number.is_finite() && number >= 0.0).then_some(number)
}

#[cfg(target_os = "linux")]
fn parse_pressure(text: &str) -> Option<PressureResource> {
    let mut some = None;
    let mut full = None;
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let kind = fields.next()?;
        let mut avg10 = None;
        let mut avg60 = None;
        let mut avg300 = None;
        let mut total_us = None;
        for field in fields {
            let (name, value) = field.split_once('=')?;
            match name {
                "avg10" if avg10.is_none() => avg10 = Some(nonnegative_number(value)?),
                "avg60" if avg60.is_none() => avg60 = Some(nonnegative_number(value)?),
                "avg300" if avg300.is_none() => avg300 = Some(nonnegative_number(value)?),
                "total" if total_us.is_none() => total_us = Some(value.parse::<u64>().ok()?),
                _ => return None,
            }
        }
        let stall = PressureStall {
            avg10: avg10?,
            avg60: avg60?,
            avg300: avg300?,
            total_us: total_us?,
        };
        match kind {
            "some" if some.is_none() => some = Some(stall),
            "full" if full.is_none() => full = Some(stall),
            _ => return None,
        }
    }
    Some(PressureResource { some: some?, full })
}

#[cfg(target_os = "linux")]
fn parse_accounting(text: &str) -> Option<BTreeMap<String, u64>> {
    let mut counters = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let name = fields.next()?;
        let value = fields.next()?.parse::<u64>().ok()?;
        if fields.next().is_some()
            || name.len() > 64
            || counters.len() >= 64
            || counters.insert(name.to_owned(), value).is_some()
        {
            return None;
        }
    }
    (!counters.is_empty()).then_some(counters)
}

#[cfg(target_os = "linux")]
fn parse_colon_fields(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_owned(), value.trim().to_owned()))
        .collect()
}

#[cfg(target_os = "linux")]
fn parse_equals_fields(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            (
                key.to_owned(),
                value
                    .trim()
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(value.trim())
                    .replace("\\\"", "\""),
            )
        })
        .collect()
}

fn failure_class(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(11 | 12) => "transient",
        Some(1 | 13 | 38) => "stable_restriction",
        _ => "io",
    }
}

#[cfg(target_os = "linux")]
fn clone_io_error(error: &io::Error) -> io::Error {
    error
        .raw_os_error()
        .map(io::Error::from_raw_os_error)
        .unwrap_or_else(|| io::Error::new(error.kind(), error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "vzik-environment-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after epoch")
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("create scratch directory");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn observed_false_zero_and_empty_values_are_distinct_from_unknown() {
        let values = serde_json::json!([
            Observation::available(false),
            Observation::available(0_u64),
            Observation::available(String::new()),
            Observation::available(Vec::<String>::new()),
            Observation::<bool>::unknown("missing evidence"),
        ]);

        assert_eq!(
            values,
            serde_json::json!([
                {"status": "available", "value": false},
                {"status": "available", "value": 0},
                {"status": "available", "value": ""},
                {"status": "available", "value": []},
                {"status": "unknown", "reason": "missing evidence"},
            ])
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn capability_decoder_keeps_named_and_future_bits() {
        assert_eq!(
            decode_capabilities((1 << 0) | (1 << 63)),
            ["chown", "cap_63"]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_capability_field_does_not_hide_observed_sets() {
        let status = parse_colon_fields("CapEff:\t0000000000000001\n");

        assert_eq!(
            capability_set(&status, "CapEff").value,
            Some(vec!["chown".into()])
        );
        assert_eq!(
            capability_set(&status, "CapBnd").status,
            ObservationStatus::Unknown
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_namespace_does_not_hide_observed_namespaces() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("partial-namespace");
        let proc = scratch.0.join("proc");
        fs::create_dir_all(proc.join("self/ns")).expect("create namespace fixture");
        symlink("cgroup:[4026531835]", proc.join("self/ns/cgroup"))
            .expect("create cgroup namespace link");

        let namespaces = observe_namespaces(&proc);

        assert_eq!(
            namespaces.cgroup.value.as_deref(),
            Some("cgroup:[4026531835]")
        );
        assert_eq!(namespaces.ipc.status, ObservationStatus::Absent);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn self_id_maps_preserve_namespace_parent_ranges_and_empty_mapping() {
        let scratch = Scratch::new("id-maps");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(proc.join("self")).expect("create proc fixture");
        fs::write(
            proc.join("self/uid_map"),
            "         0     100000      65536\n     65536     200000          1\n",
        )
        .expect("write UID map");
        fs::write(proc.join("self/gid_map"), "0 0 4294967295\n").expect("write GID map");

        let restrictions = observe_restrictions(&proc, &sys, &BTreeMap::new());

        assert_eq!(
            serde_json::to_value(restrictions.uid_map).expect("serialize UID map")["value"],
            serde_json::json!([
                {"namespace_start": 0, "parent_namespace_start": 100000, "length": 65536},
                {"namespace_start": 65536, "parent_namespace_start": 200000, "length": 1},
            ])
        );
        assert_eq!(
            restrictions.gid_map.value,
            Some(vec![IdMapRange {
                namespace_start: 0,
                parent_namespace_start: 0,
                length: u32::MAX,
            }])
        );

        fs::write(proc.join("self/uid_map"), "").expect("write empty UID map");
        let empty = observe_restrictions(&proc, &sys, &BTreeMap::new()).uid_map;
        assert_eq!(empty.status, ObservationStatus::Available);
        assert_eq!(empty.value, Some(Vec::new()));
        assert_eq!(
            parse_id_map("4294967294 4294967294 1\n"),
            Some(vec![IdMapRange {
                namespace_start: u32::MAX - 1,
                parent_namespace_start: u32::MAX - 1,
                length: 1,
            }])
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn malformed_id_map_ranges_invalidate_the_whole_observation() {
        let scratch = Scratch::new("malformed-id-maps");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(proc.join("self")).expect("create proc fixture");
        for invalid in [
            "1 200000",
            "1 200000 1 extra",
            "1 200000 0",
            "-1 200000 1",
            "+1 200000 1",
            "4294967294 200000 2",
            "1 4294967294 2",
            "1 200000 4294967296",
        ] {
            fs::write(
                proc.join("self/uid_map"),
                format!("0 100000 1\n{invalid}\n"),
            )
            .expect("write malformed UID map");
            let restrictions = observe_restrictions(&proc, &sys, &BTreeMap::new());
            assert_eq!(
                restrictions.uid_map.status,
                ObservationStatus::Unknown,
                "{invalid}"
            );
            assert!(restrictions.uid_map.value.is_none(), "{invalid}");
            assert_eq!(restrictions.gid_map.status, ObservationStatus::Absent);
        }

        fs::write(
            proc.join("self/uid_map"),
            vec![b' '; TEXT_SOURCE_LIMIT as usize + 1],
        )
        .expect("write oversized UID map");
        let oversized = observe_restrictions(&proc, &sys, &BTreeMap::new()).uid_map;
        assert_eq!(oversized.status, ObservationStatus::Unavailable);
        assert_eq!(
            oversized
                .error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("invaliddata")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_membership_preserves_hierarchies_controllers_and_path_bytes() {
        use base64::Engine as _;

        let membership = parse_cgroup_membership(
            b"9:cpu,cpuacct:/parent:child\n4:name=systemd:/service\n0::/byte\xff:tail \n",
        )
        .expect("parse mixed membership");
        assert_eq!(
            serde_json::to_value(&membership[..2]).expect("serialize v1 memberships"),
            serde_json::json!([
                {
                    "hierarchy_kind": "legacy",
                    "hierarchy_id": 9,
                    "controllers": ["cpu", "cpuacct"],
                    "namespace_relative_path": {"display": "/parent:child"},
                },
                {
                    "hierarchy_kind": "legacy",
                    "hierarchy_id": 4,
                    "controllers": ["name=systemd"],
                    "namespace_relative_path": {"display": "/service"},
                },
            ])
        );
        assert_eq!(membership[2].hierarchy, CgroupHierarchy::Unified);
        assert_eq!(
            membership[2].namespace_relative_path["display"],
            "/byte\\xff:tail "
        );
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(
                    membership[2].namespace_relative_path["raw_base64"]
                        .as_str()
                        .expect("raw path encoding")
                )
                .expect("decode raw path"),
            b"/byte\xff:tail "
        );
        assert_eq!(
            parse_cgroup_membership(b"").expect("parse empty membership"),
            Vec::new()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn malformed_cgroup_entry_does_not_keep_a_partial_membership() {
        for invalid in [
            b"invalid:cpu:/path".as_slice(),
            b"4294967296:cpu:/path".as_slice(),
            b"+1:cpu:/path".as_slice(),
            b"1:cpu".as_slice(),
            b"1:cpu,,memory:/path".as_slice(),
            b"1:\xff:/path".as_slice(),
            b"1:cpu:relative".as_slice(),
            b"1:cpu:".as_slice(),
            b"0::/nul\0path".as_slice(),
            b"".as_slice(),
        ] {
            let mut bytes = b"0::/valid\n".to_vec();
            bytes.extend_from_slice(invalid);
            bytes.push(b'\n');
            let error = parse_cgroup_membership(&bytes).expect_err("reject malformed entry");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{invalid:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn membership_absence_failure_and_malformed_data_remain_distinct() {
        let scratch = Scratch::new("cgroup-source-states");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(proc.join("self")).expect("create proc fixture");
        let missing = observe_cgroups(&proc, &sys);
        assert_eq!(missing.membership.status, ObservationStatus::Absent);
        assert_eq!(missing.pressure.cpu.status, ObservationStatus::Absent);

        fs::write(proc.join("self/cgroup"), b"").expect("write empty membership");
        let empty = observe_cgroups(&proc, &sys);
        assert_eq!(empty.membership.status, ObservationStatus::Available);
        assert_eq!(empty.membership.value, Some(Vec::new()));
        assert_eq!(empty.pressure.cpu.status, ObservationStatus::NotApplicable);

        fs::write(proc.join("self/cgroup"), b"0::/valid\nbad\n").expect("write bad membership");
        let malformed = observe_cgroups(&proc, &sys);
        assert_eq!(malformed.membership.status, ObservationStatus::Unknown);
        assert!(malformed.membership.value.is_none());
        assert_eq!(
            malformed.limits.memory_max.status,
            ObservationStatus::Unknown
        );
        assert_eq!(malformed.pressure.cpu.status, ObservationStatus::Unknown);

        fs::write(proc.join("self/cgroup"), b"0::/\n").expect("write unified membership");
        let mount_missing = observe_cgroups(&proc, &sys);
        assert_eq!(
            mount_missing.membership.status,
            ObservationStatus::Available
        );
        assert_eq!(
            mount_missing.pressure.cpu.status,
            ObservationStatus::Unavailable
        );
        assert_eq!(
            mount_missing
                .pressure
                .cpu
                .error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("notfound")
        );

        fs::write(proc.join("self/mountinfo"), b"").expect("write empty mountinfo");
        let mount_absent = observe_cgroups(&proc, &sys);
        assert_eq!(mount_absent.pressure.cpu.status, ObservationStatus::Absent);

        fs::remove_file(proc.join("self/cgroup")).expect("remove membership fixture");
        fs::create_dir(proc.join("self/cgroup")).expect("make unreadable membership source");
        let failed = observe_cgroups(&proc, &sys);
        assert_eq!(failed.membership.status, ObservationStatus::Unavailable);
        assert_eq!(failed.pressure.cpu.status, ObservationStatus::Unavailable);
        assert!(failed.membership.error.is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn byte_paths_resolve_local_cgroup_pressure_separately_from_system_pressure() {
        use base64::Engine as _;
        use std::os::unix::ffi::OsStrExt;

        let scratch = Scratch::new("byte-cgroup-pressure");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        let mount = scratch
            .0
            .join(std::ffi::OsStr::from_bytes(b"visible\xfe cg"));
        let current = mount.join("docker:job ");
        fs::create_dir_all(proc.join("self")).expect("create proc fixture");
        fs::create_dir_all(proc.join("pressure")).expect("create system pressure fixture");
        fs::create_dir_all(&current).expect("create byte-named cgroup fixture");
        fs::write(
            proc.join("self/cgroup"),
            b"9:cpu:/other\n0::/tenant\xff:alpha/docker:job \n",
        )
        .expect("write byte membership");
        fs::write(
            proc.join("self/mountinfo"),
            b"20 1 0:20 /tenant\xff:alpha /visible\xfe\\040cg rw - cgroup2 cgroup rw\n",
        )
        .expect("write byte mountinfo");
        fs::write(current.join("memory.current"), b"77\n").expect("write local memory usage");
        fs::write(
            current.join("cpu.pressure"),
            b"some avg10=1 avg60=2 avg300=3 total=7\n",
        )
        .expect("write local pressure");
        fs::write(
            proc.join("pressure/cpu"),
            b"some avg10=4 avg60=5 avg300=6 total=101\n",
        )
        .expect("write system pressure");

        let cgroups = observe_cgroups(&proc, &sys);
        let resources = observe_resources(&proc, &sys, &BTreeMap::new(), None);
        let membership = cgroups
            .membership
            .value
            .as_deref()
            .expect("observed membership");
        let container =
            observe_container_isolation(&scratch.0, &proc, membership, None, false, false);

        assert_eq!(cgroups.limits.memory_current.value.as_deref(), Some("77"));
        assert_eq!(cgroups.pressure.scope, PressureScope::VisibleCurrentCgroup);
        assert_eq!(resources.pressure.scope, PressureScope::VisibleSystem);
        assert_eq!(
            cgroups
                .pressure
                .cpu
                .value
                .as_ref()
                .map(|pressure| pressure.some.total_us),
            Some(7)
        );
        assert_eq!(
            resources
                .pressure
                .cpu
                .value
                .as_ref()
                .map(|pressure| pressure.some.total_us),
            Some(101)
        );
        assert_eq!(cgroups.pressure.memory.status, ObservationStatus::Absent);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(
                    membership[1].namespace_relative_path["raw_base64"]
                        .as_str()
                        .expect("raw path encoding")
                )
                .expect("decode raw path"),
            b"/tenant\xff:alpha/docker:job "
        );
        assert_eq!(
            container.value,
            Some(IsolationDetection {
                verdict: IsolationVerdict::IndicatorPresent,
                provider: "docker".into(),
                confidence: Confidence::High,
            })
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_cgroup_limit_does_not_hide_observed_limits() {
        let scratch = Scratch::new("partial-cgroup");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(proc.join("self")).expect("create proc fixture");
        fs::create_dir_all(sys.join("fs/cgroup/workload")).expect("create cgroup fixture");
        fs::write(proc.join("self/cgroup"), "0::/workload\n").expect("write membership");
        fs::write(
            proc.join("self/mountinfo"),
            "20 1 0:20 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
        )
        .expect("write visible cgroup mount");
        fs::write(sys.join("fs/cgroup/workload/memory.max"), "1048576\n")
            .expect("write memory limit");

        let cgroups = observe_cgroups(&proc, &sys);

        assert_eq!(cgroups.limits.memory_max.value.as_deref(), Some("1048576"));
        assert_eq!(cgroups.limits.cpu_max.status, ObservationStatus::Absent);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn delegated_cgroup_mount_uses_local_counters_and_limits_only() {
        let scratch = Scratch::new("delegated-cgroup");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        let mount = scratch.0.join("delegated cg");
        let current = mount.join("service");
        fs::create_dir_all(proc.join("self")).expect("create proc fixture");
        fs::create_dir_all(&current).expect("create delegated cgroup fixture");
        fs::write(proc.join("self/cgroup"), "0::/workload/service\n").expect("write membership");
        fs::write(
            proc.join("self/mountinfo"),
            "20 1 0:20 /workload /delegated\\040cg rw - cgroup2 cgroup rw\n",
        )
        .expect("write delegated cgroup mount");
        fs::write(mount.join("memory.max"), "1234\n").expect("write ancestor limit");
        fs::write(current.join("memory.current"), "0\n").expect("write current memory usage");
        fs::write(current.join("cpu.stat"), "usage_usec 0\nnr_periods 42\n")
            .expect("write CPU counters");
        fs::write(current.join("memory.events"), "oom 0\noom_kill 0\n")
            .expect("write memory counters");
        fs::write(current.join("pids.events"), "max 0\n").expect("write PID counters");
        fs::write(
            current.join("cpu.pressure"),
            "some avg10=0 avg60=0 avg300=0 total=0\n",
        )
        .expect("write CPU pressure");

        let cgroups = observe_cgroups(&proc, &sys);

        assert_eq!(cgroups.limits.memory_current.value.as_deref(), Some("0"));
        assert_eq!(cgroups.limits.memory_max.status, ObservationStatus::Absent);
        assert_eq!(
            cgroups.accounting.cpu_stat.value,
            Some(BTreeMap::from([
                ("nr_periods".into(), 42),
                ("usage_usec".into(), 0)
            ]))
        );
        assert_eq!(
            cgroups
                .accounting
                .memory_events
                .value
                .as_ref()
                .and_then(|values| values.get("oom")),
            Some(&0)
        );
        assert_eq!(
            cgroups
                .accounting
                .pids_events
                .value
                .as_ref()
                .and_then(|values| values.get("max")),
            Some(&0)
        );
        let pressure = cgroups.pressure.cpu.value.expect("observed CPU pressure");
        assert_eq!(pressure.some.total_us, 0);
        assert_eq!(pressure.full, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn namespace_relative_cgroup_parent_path_cannot_escape_visible_mount() {
        let scratch = Scratch::new("cgroup-parent-path");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(proc.join("self")).expect("create proc fixture");
        fs::create_dir_all(sys.join("fs/outside")).expect("create outside fixture");
        fs::write(proc.join("self/cgroup"), "0::/../outside\n")
            .expect("write namespace-relative membership");
        fs::write(
            proc.join("self/mountinfo"),
            "20 1 0:20 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
        )
        .expect("write visible cgroup mount");
        fs::write(sys.join("fs/outside/memory.max"), "1234\n").expect("write inaccessible limit");

        let cgroups = observe_cgroups(&proc, &sys);

        assert_eq!(
            serde_json::to_value(cgroups.membership).expect("serialize namespace-relative path")["value"],
            serde_json::json!([{
                "hierarchy_kind": "unified",
                "namespace_relative_path": {"display": "/../outside"},
            }])
        );
        assert_eq!(cgroups.limits.memory_max.status, ObservationStatus::Unknown);
        assert!(cgroups.limits.memory_max.value.is_none());
        assert_eq!(
            cgroups.accounting.cpu_stat.status,
            ObservationStatus::Unknown
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn passive_resources_preserve_zero_and_sparse_cpu_ranges() {
        let scratch = Scratch::new("resources-zero");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(proc.join("pressure")).expect("create pressure fixture");
        fs::create_dir_all(sys.join("devices/system/cpu")).expect("create CPU fixture");
        fs::write(sys.join("devices/system/cpu/online"), "0-3,8,10-11\n")
            .expect("write online CPUs");
        fs::write(proc.join("uptime"), "0 0\n").expect("write uptime");
        fs::write(proc.join("loadavg"), "0 0 0 1/1 1\n").expect("write load averages");
        fs::write(
            proc.join("pressure/cpu"),
            "some avg10=0 avg60=0 avg300=0 total=0\n",
        )
        .expect("write CPU pressure");
        fs::write(
            proc.join("meminfo"),
            "MemTotal: 2048 kB\nMemAvailable: 0 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
        )
        .expect("write memory");
        let status = parse_colon_fields("Cpus_allowed_list:\t0,2-3\n");

        let resources = observe_resources(&proc, &sys, &status, None);
        let memory = observe_memory(&proc);
        let value = serde_json::to_value(resources).expect("serialize resources");

        assert_eq!(value["cpu_online"]["value"], 7);
        assert_eq!(value["cpu_allowed_list"]["value"], "0,2-3");
        assert_eq!(value["uptime_seconds"]["status"], "available");
        assert_eq!(value["uptime_seconds"]["value"], 0.0);
        assert_eq!(
            value["load_average"]["value"],
            serde_json::json!([0.0, 0.0, 0.0])
        );
        assert_eq!(value["pressure"]["cpu"]["value"]["some"]["total_us"], 0);
        assert!(value["pressure"]["cpu"]["value"]["full"].is_null());
        assert_eq!(value["pressure"]["memory"]["status"], "absent");
        assert_eq!(memory.physical_bytes.value, Some(2_097_152));
        assert_eq!(memory.available_bytes.value, Some(0));
        assert_eq!(memory.swap_total_bytes.value, Some(0));
        assert_eq!(memory.swap_free_bytes.value, Some(0));

        fs::remove_file(proc.join("uptime")).expect("remove uptime fixture");
        let missing = observe_resources(&proc, &sys, &status, None).uptime_seconds;
        assert_eq!(missing.status, ObservationStatus::Unavailable);
        assert!(missing.value.is_none());
        assert_eq!(
            missing
                .error
                .as_ref()
                .and_then(|error| error.code.as_deref()),
            Some("notfound")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn malformed_resource_sources_are_not_available_numbers() {
        for list in ["", "3-1", "0-3,3", "0,,2", "0-4294967295"] {
            let observation = parse_observation(
                Observation::available(list.to_owned()),
                |text| cpu_list_count(text),
                "CPU list",
            );
            assert_eq!(observation.status, ObservationStatus::Unknown, "{list}");
            assert!(observation.value.is_none(), "{list}");
        }
        for average in ["NaN", "inf", "-0.1", "1e999"] {
            let text = format!("some avg10={average} avg60=0 avg300=0 total=0\n");
            let observation = parse_observation(
                Observation::available(text),
                |text| parse_pressure(text),
                "pressure",
            );
            assert_eq!(observation.status, ObservationStatus::Unknown, "{average}");
            assert!(observation.value.is_none(), "{average}");
        }
        for text in [
            "full avg10=0 avg60=0 avg300=0 total=0\n",
            "some avg10=0 avg10=1 avg60=0 avg300=0 total=0\n",
            "some avg10=0 avg60=0 avg300=0 total=-1\n",
            "some avg10=0 avg60=0 total=0\n",
        ] {
            assert!(parse_pressure(text).is_none(), "{text}");
        }
        for text in ["oom 0\noom 1\n", "oom -1\n", "oom 0 extra\n", ""] {
            assert!(parse_accounting(text).is_none(), "{text}");
        }
        let fields = parse_colon_fields("SwapTotal: 18446744073709551615 kB\n");
        assert_eq!(
            swap_field(&fields, "SwapTotal").status,
            ObservationStatus::Unknown
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn subsystem_socket_presence_does_not_treat_regular_marker_files_as_sockets() {
        use std::os::unix::net::UnixListener;

        let scratch = Scratch::new("bus");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(&proc).expect("create proc fixture");
        fs::create_dir_all(sys.join("fs/cgroup")).expect("create cgroup fixture");
        fs::create_dir_all(scratch.0.join("run/dbus")).expect("create bus fixture");
        fs::create_dir_all(scratch.0.join("run/systemd/system")).expect("create systemd fixture");
        fs::write(sys.join("fs/cgroup/cgroup.controllers"), "").expect("write cgroup marker");
        fs::write(scratch.0.join("run/portod.socket"), "").expect("write non-socket marker");
        let _listener = UnixListener::bind(scratch.0.join("run/dbus/system_bus_socket"))
            .expect("bind system bus socket fixture");

        let subsystems = observe_subsystems(&scratch.0, &proc, &sys);

        assert_eq!(subsystems.procfs.value, Some(true));
        assert_eq!(subsystems.sysfs.value, Some(true));
        assert_eq!(subsystems.cgroup_v2.value, Some(true));
        assert_eq!(subsystems.systemd.value, Some(true));
        assert_eq!(subsystems.dbus.value, Some(true));
        assert_eq!(subsystems.porto.value, Some(false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn oversized_observation_source_is_rejected() {
        let scratch = Scratch::new("bounded-read");
        let source = scratch.0.join("source");
        fs::write(&source, vec![b'x'; TEXT_SOURCE_LIMIT as usize + 1])
            .expect("write oversized source");

        let error = read_text(source).expect_err("reject oversized observation");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_signature_reports_cpuinfo_as_vm_evidence() {
        let scratch = Scratch::new("cpu-evidence");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(&proc).expect("create proc fixture");
        fs::create_dir_all(&sys).expect("create sys fixture");
        fs::write(proc.join("cpuinfo"), "Hypervisor vendor: KVM\n").expect("write cpuinfo");

        let isolation = observe_isolation(&scratch.0, &proc, &sys, &BTreeMap::new(), &[]);
        let evidence = &isolation.virtual_machine.evidence;

        assert_eq!(
            isolation.virtual_machine.value,
            Some(IsolationDetection {
                verdict: IsolationVerdict::IndicatorPresent,
                provider: "kvm_qemu".into(),
                confidence: Confidence::High,
            })
        );
        assert_eq!(evidence.len(), 1);
        assert!(evidence[0].source.ends_with("/proc/cpuinfo"));
        assert_eq!(evidence[0].detail, "matched marker kvm");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn portoinit_overrides_portos_lxc_environment_marker() {
        let scratch = Scratch::new("portoinit");
        let proc = scratch.0.join("proc");
        fs::create_dir_all(proc.join("1")).expect("create PID 1 fixture");
        fs::write(proc.join("1/comm"), "portoinit\n").expect("write PID 1 comm");

        let container =
            observe_container_isolation(&scratch.0, &proc, &[], Some("lxc"), false, false);

        assert_eq!(
            serde_json::to_value(&container).expect("serialize container indicator"),
            serde_json::json!({
                "status": "available",
                "value": {
                    "verdict": "indicator_present",
                    "provider": "porto",
                    "confidence": "high",
                },
                "evidence": [{
                    "source": "/proc/1/comm",
                    "detail": "portoinit",
                }],
            })
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn either_porto_environment_marker_overrides_lxc() {
        let scratch = Scratch::new("porto-env");
        let proc = scratch.0.join("proc");
        for (porto_name, porto_host, source) in
            [(true, false, "PORTO_NAME"), (false, true, "PORTO_HOST")]
        {
            let container = observe_container_isolation(
                &scratch.0,
                &proc,
                &[],
                Some("lxc"),
                porto_name,
                porto_host,
            );

            assert_eq!(
                container.value,
                Some(IsolationDetection {
                    verdict: IsolationVerdict::IndicatorPresent,
                    provider: "porto".into(),
                    confidence: Confidence::High,
                })
            );
            assert_eq!(container.evidence[0].source, source);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn portod_socket_is_evidence_without_a_container_verdict() {
        let scratch = Scratch::new("portod-socket");
        let proc = scratch.0.join("proc");
        fs::create_dir_all(scratch.0.join("run")).expect("create run fixture");
        fs::write(scratch.0.join("run/portod.socket"), b"").expect("create socket marker");

        let container = observe_container_isolation(&scratch.0, &proc, &[], None, false, false);

        assert_eq!(
            serde_json::to_value(&container).expect("serialize inconclusive container evidence"),
            serde_json::json!({
                "status": "unknown",
                "reason": "Porto socket alone does not distinguish a host from a container",
                "evidence": [{
                    "source": "/run/portod.socket",
                    "detail": "Porto daemon socket is present on both hosts and containers",
                }],
            })
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lxc_environment_marker_remains_lxc_without_porto_evidence() {
        let scratch = Scratch::new("lxc-env");
        let proc = scratch.0.join("proc");

        let container =
            observe_container_isolation(&scratch.0, &proc, &[], Some("lxc"), false, false);

        assert_eq!(
            container.value,
            Some(IsolationDetection {
                verdict: IsolationVerdict::IndicatorPresent,
                provider: "lxc".into(),
                confidence: Confidence::High,
            })
        );
    }
}
