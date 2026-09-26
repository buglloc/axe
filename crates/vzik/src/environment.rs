#[cfg(target_os = "linux")]
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
    pub value: Option<T>,
    pub reason: Option<String>,
    pub error: Option<Failure>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnvironmentObservation {
    pub host: HostObservation,
    pub kernel: KernelObservation,
    pub memory: MemoryObservation,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RestrictionObservation {
    pub capability_sets: CapabilitySetsObservation,
    pub no_new_privileges: Observation<bool>,
    pub seccomp_mode: Observation<u32>,
    pub seccomp_filter_count: Observation<u32>,
    pub lsm_profile: Observation<String>,
    pub uid_map: Observation<String>,
    pub gid_map: Observation<String>,
    pub namespaces: NamespaceObservation,
    pub cgroups: CgroupObservation,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CgroupObservation {
    pub membership: Observation<String>,
    pub limits: CgroupLimitObservation,
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
    pub virtual_machine: IsolationLayer,
    pub container: IsolationLayer,
    pub sandbox: IsolationLayer,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IsolationLayer {
    pub interpretation: &'static str,
    pub verdict: Observation<IsolationVerdict>,
    pub provider: Observation<String>,
    pub confidence: Observation<Confidence>,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationVerdict {
    Detected,
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
    let process = ProcessObservation {
        pid: Observation::available(std::process::id()),
        ppid: numeric_status(&status_fields, "PPid"),
        uid: indexed_status(&status_fields, "Uid", 0),
        effective_uid: indexed_status(&status_fields, "Uid", 1),
        gid: indexed_status(&status_fields, "Gid", 0),
        effective_gid: indexed_status(&status_fields, "Gid", 1),
    };

    let restrictions = observe_restrictions(&proc, &sys, &status_fields);
    let isolation = observe_isolation(root, &proc, &sys, &status_fields);

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
        },
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
        },
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
                available_bytes: unavailable_read(path, error),
            };
        }
    };
    let fields = parse_colon_fields(&text);

    MemoryObservation {
        physical_bytes: memory_field(&fields, "MemTotal"),
        available_bytes: memory_field(&fields, "MemAvailable"),
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
        uid_map: read_observation(proc.join("self/uid_map")),
        gid_map: read_observation(proc.join("self/gid_map")),
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
    let membership = read_observation(proc.join("self/cgroup"));
    let unified_root = membership
        .value
        .as_deref()
        .and_then(|membership| membership.lines().find_map(|line| line.strip_prefix("0::")))
        .map(|relative| sys.join("fs/cgroup").join(relative.trim_start_matches('/')));

    CgroupObservation {
        membership,
        limits: CgroupLimitObservation {
            cpu_max: cgroup_limit_observation(unified_root.as_deref(), "cpu.max"),
            memory_current: cgroup_limit_observation(unified_root.as_deref(), "memory.current"),
            memory_max: cgroup_limit_observation(unified_root.as_deref(), "memory.max"),
            pids_current: cgroup_limit_observation(unified_root.as_deref(), "pids.current"),
            pids_max: cgroup_limit_observation(unified_root.as_deref(), "pids.max"),
        },
    }
}

#[cfg(target_os = "linux")]
fn cgroup_limit_observation(root: Option<&Path>, name: &str) -> Observation<String> {
    let Some(root) = root else {
        return Observation::not_applicable("cgroup v2 membership was not observed");
    };
    read_observation(root.join(name))
}

#[cfg(target_os = "linux")]
fn observe_isolation(
    root: &Path,
    proc: &Path,
    sys: &Path,
    status: &BTreeMap<String, String>,
) -> IsolationObservation {
    let ancestry = process_ancestry(proc);
    let ancestry_text = ancestry.join(" ").to_ascii_lowercase();
    let apparmor = read_text(proc.join("self/attr/current")).unwrap_or_default();
    let cgroup = read_text(proc.join("self/cgroup")).unwrap_or_default();
    let container_env = std::env::var("container").ok();
    let container = observe_container_isolation(
        root,
        proc,
        &cgroup,
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
        virtual_machine: isolation_layer(
            vm_provider,
            vm_evidence,
            "no strong VM signature matched",
        ),
        container,
        sandbox: isolation_layer(
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
    cgroup: &str,
    container_env: Option<&str>,
    porto_name: bool,
    porto_host: bool,
) -> IsolationLayer {
    let portoinit = read_text(proc.join("1/comm"))
        .is_ok_and(|comm| comm.trim().eq_ignore_ascii_case("portoinit"));
    let portod_socket = root.join("run/portod.socket").exists();
    let mut evidence = Vec::new();

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
        return isolation_layer(
            Some(("porto", Confidence::High)),
            evidence,
            "no strong container signature matched",
        );
    }

    let provider = if root.join(".dockerenv").exists() || cgroup.contains("docker") {
        evidence.push(Evidence {
            source: "marker/cgroup".into(),
            detail: "Docker marker or cgroup component".into(),
        });
        Some(("docker", Confidence::High))
    } else if root.join("run/.containerenv").exists() || cgroup.contains("libpod") {
        evidence.push(Evidence {
            source: "marker/cgroup".into(),
            detail: "Podman marker or libpod cgroup component".into(),
        });
        Some(("podman", Confidence::High))
    } else if cgroup.contains("kubepods") {
        evidence.push(Evidence {
            source: "/proc/self/cgroup".into(),
            detail: "kubepods cgroup component".into(),
        });
        Some(("kubernetes", Confidence::High))
    } else if cgroup.contains("lxc") {
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

    isolation_layer(provider, evidence, unknown_reason)
}

#[cfg(target_os = "linux")]
fn isolation_layer(
    provider: Option<(&str, Confidence)>,
    evidence: Vec<Evidence>,
    unknown_reason: &str,
) -> IsolationLayer {
    match provider {
        Some((provider, confidence)) => IsolationLayer {
            interpretation: "heuristic_indicator",
            verdict: Observation::available(IsolationVerdict::Detected),
            provider: Observation::available(provider.to_owned()),
            confidence: Observation::available(confidence),
            evidence,
        },
        None => IsolationLayer {
            interpretation: "heuristic_indicator",
            verdict: Observation::unknown(unknown_reason),
            provider: Observation::unknown(unknown_reason),
            confidence: Observation::unknown(unknown_reason),
            evidence,
        },
    }
}

#[cfg(not(target_os = "linux"))]
fn unknown_isolation(reason: &str) -> IsolationLayer {
    IsolationLayer {
        interpretation: "heuristic_indicator",
        verdict: Observation::unknown(reason),
        provider: Observation::unknown(reason),
        confidence: Observation::unknown(reason),
        evidence: Vec::new(),
    }
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

    Observation::available(kibibytes.saturating_mul(1024))
        .with_evidence("/proc/meminfo", name.to_owned())
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
    match read_text(&path) {
        Ok(value) => Observation::available(value.trim().to_owned())
            .with_evidence(path.display().to_string(), "read text value"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Observation::absent(format!("{} does not exist", path.display()))
        }
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
    let file = fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(64 << 10);
    file.take(TEXT_SOURCE_LIMIT + 1).read_to_end(&mut bytes)?;

    if bytes.len() as u64 > TEXT_SOURCE_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "observation source exceeds 1 MiB limit",
        ));
    }

    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
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
    fn available_false_is_distinct_from_unknown() {
        let available = serde_json::to_value(Observation::available(false)).expect("serialize");
        let unknown = serde_json::to_value(Observation::<bool>::unknown("missing evidence"))
            .expect("serialize");

        assert_eq!(available["status"], "available");
        assert_eq!(available["value"], false);
        assert_eq!(unknown["status"], "unknown");
        assert!(unknown["value"].is_null());
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
    fn missing_cgroup_limit_does_not_hide_observed_limits() {
        let scratch = Scratch::new("partial-cgroup");
        let proc = scratch.0.join("proc");
        let sys = scratch.0.join("sys");
        fs::create_dir_all(proc.join("self")).expect("create proc fixture");
        fs::create_dir_all(sys.join("fs/cgroup/workload")).expect("create cgroup fixture");
        fs::write(proc.join("self/cgroup"), "0::/workload\n").expect("write membership");
        fs::write(sys.join("fs/cgroup/workload/memory.max"), "1048576\n")
            .expect("write memory limit");

        let cgroups = observe_cgroups(&proc, &sys);

        assert_eq!(cgroups.limits.memory_max.value.as_deref(), Some("1048576"));
        assert_eq!(cgroups.limits.cpu_max.status, ObservationStatus::Absent);
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

        let isolation = observe_isolation(&scratch.0, &proc, &sys, &BTreeMap::new());
        let evidence = &isolation.virtual_machine.evidence;

        assert_eq!(
            isolation.virtual_machine.provider.value.as_deref(),
            Some("kvm_qemu")
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
            observe_container_isolation(&scratch.0, &proc, "", Some("lxc"), false, false);

        assert_eq!(container.provider.value.as_deref(), Some("porto"));
        assert_eq!(container.confidence.value, Some(Confidence::High));
        assert_eq!(
            container.evidence,
            [Evidence {
                source: "/proc/1/comm".into(),
                detail: "portoinit".into(),
            }]
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
                "",
                Some("lxc"),
                porto_name,
                porto_host,
            );

            assert_eq!(container.provider.value.as_deref(), Some("porto"));
            assert_eq!(container.confidence.value, Some(Confidence::High));
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

        let container = observe_container_isolation(&scratch.0, &proc, "", None, false, false);

        assert_eq!(container.verdict.status, ObservationStatus::Unknown);
        assert_eq!(container.provider.status, ObservationStatus::Unknown);
        assert_eq!(container.evidence.len(), 1);
        assert_eq!(container.evidence[0].source, "/run/portod.socket");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lxc_environment_marker_remains_lxc_without_porto_evidence() {
        let scratch = Scratch::new("lxc-env");
        let proc = scratch.0.join("proc");

        let container =
            observe_container_isolation(&scratch.0, &proc, "", Some("lxc"), false, false);

        assert_eq!(container.provider.value.as_deref(), Some("lxc"));
        assert_eq!(container.confidence.value, Some(Confidence::High));
    }
}
