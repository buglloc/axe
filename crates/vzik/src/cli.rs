use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;

use clap_builder::builder::{NonEmptyStringValueParser, OsStringValueParser, ValueParser};
use clap_builder::error::ErrorKind;
use clap_builder::parser::ValueSource;
use clap_builder::{Arg, ArgAction, ArgMatches, Command};
use serde::Serialize;
use serde_json::{Value, json};

const SAFE_DEADLINE_SECONDS: u64 = 2400;
const TARGETED_DEADLINE_SECONDS: u64 = 600;
const TARGETED_DEADLINE_HARD: u64 = 2400;
const SAFE_OUTPUT_BYTES: u64 = 256 << 20;
const TARGETED_OUTPUT_BYTES: u64 = 256 << 20;
const SAFE_RECORDS: u64 = 250_000;
const TARGETED_RECORDS: u64 = 1_000_000;
pub const MAX_LINE_BYTES: u64 = 128 << 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityId {
    HostInfo,
    KernelInfo,
    KernelModules,
    KernelSysctls,
    SecurityPosture,
    ProcessList,
    NetworkInterfaces,
    NetworkAddresses,
    NetworkResolvers,
    NetworkRoutes,
    NetworkNeighbors,
    NetworkSockets,
    NetworkListeners,
    NetworkFirewall,
    MountList,
    CgroupInspect,
    UserList,
    GroupList,
    AuthPosture,
    SudoRules,
    PackageList,
    ServiceList,
    ScheduleList,
    SystemctlList,
    SystemctlInspect,
    DbusList,
    DbusInspect,
    ContainerList,
    ContainerInspect,
    PortoList,
    PortoInspect,
    SshServerConfig,
    SshAuthorizedKeys,
    FilesystemPrivilegeSurfaces,
    FilesystemUnixSockets,
    FileStat,
    FileRead,
    Overview,
    ProcessInspect,
}

#[derive(Clone, Copy)]
pub(crate) struct NumberSpec {
    pub name: &'static str,
    pub value_name: &'static str,
    pub description: &'static str,
    pub default: u64,
    pub default_display: &'static str,
    pub minimum: u64,
    pub maximum: u64,
}

impl NumberSpec {
    pub(crate) fn contract(self) -> Value {
        json!({
            "name":self.name,
            "value_name":self.value_name,
            "description":self.description,
            "type":"integer",
            "default":self.default,
            "minimum":self.minimum,
            "maximum":self.maximum,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PositionalKind {
    Utf8,
    LinuxPath,
    Pid,
}

impl PositionalKind {
    const fn id(self) -> &'static str {
        match self {
            Self::Utf8 => "utf8_string",
            Self::LinuxPath => "linux_path",
            Self::Pid => "integer",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PositionalSpec {
    pub name: &'static str,
    pub value_name: &'static str,
    pub description: &'static str,
    pub kind: PositionalKind,
    pub max_bytes: usize,
}

impl PositionalSpec {
    pub(crate) fn contract(self) -> Value {
        let mut contract = json!({
            "name":self.name,
            "value_name":self.value_name,
            "description":self.description,
            "type":self.kind.id(),
            "required":true,
            "max_bytes":self.max_bytes,
        });
        if matches!(self.kind, PositionalKind::Pid) {
            contract["minimum"] = json!(1);
            contract["maximum"] = json!(i32::MAX);
        }
        contract
    }
}

#[derive(Clone, Copy)]
pub(crate) enum RequestSpec {
    Empty,
    Overview,
    Items(NumberSpec),
    ContainerInspect,
    ProcessInspect,
    PortoList(NumberSpec),
    PortoInspect,
    SystemctlList(NumberSpec),
    SystemctlInspect,
    DbusList(NumberSpec),
    DbusInspect,
    FilesystemScan(NumberSpec),
    FileStat,
    FileRead,
}

/// One capability contract row; CLI parsing, help, profiles, and discovery derive from it.
struct CapabilitySpec {
    capability: CapabilityId,
    id: &'static str,
    command: &'static [&'static str],
    description: &'static str,
    request: RequestSpec,
    data_kinds: &'static [&'static str],
    baseline: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum OptionKind {
    Number(NumberSpec),
    Flag,
    LinuxPath { default: &'static str },
    OptionalLinuxPath,
    OptionalUtf8,
}

#[derive(Clone, Copy)]
pub(crate) struct OptionSpec {
    pub name: &'static str,
    pub value_name: &'static str,
    pub description: &'static str,
    pub kind: OptionKind,
}

impl OptionSpec {
    pub(crate) fn contract(self) -> Value {
        match self.kind {
            OptionKind::Number(number) => number.contract(),
            OptionKind::Flag => json!({
                "name":self.name,
                "description":self.description,
                "type":"boolean",
                "default":false,
            }),
            OptionKind::LinuxPath { default } => json!({
                "name":self.name,
                "value_name":self.value_name,
                "description":self.description,
                "type":"linux_path",
                "required":false,
                "default":default,
                "max_bytes":SOCKET_PATH_BYTES,
            }),
            OptionKind::OptionalLinuxPath => json!({
                "name":self.name,
                "value_name":self.value_name,
                "description":self.description,
                "type":"linux_path",
                "required":false,
                "default":Value::Null,
                "max_bytes":SOCKET_PATH_BYTES,
            }),
            OptionKind::OptionalUtf8 => json!({
                "name":self.name,
                "value_name":self.value_name,
                "description":self.description,
                "type":"utf8_string",
                "required":false,
                "default":Value::Null,
                "max_bytes":BUS_USER_BYTES,
            }),
        }
    }
}

const PROCESS_PID: PositionalSpec = PositionalSpec {
    name: "pid",
    value_name: "PID",
    description: "Visible process ID",
    kind: PositionalKind::Pid,
    max_bytes: 10,
};
const CONTAINER_NAME: PositionalSpec = PositionalSpec {
    name: "name",
    value_name: "NAME",
    description: "Native container runtime ID or self",
    kind: PositionalKind::Utf8,
    max_bytes: 4096,
};
const PORTO_NAME: PositionalSpec = PositionalSpec {
    name: "name",
    value_name: "NAME",
    description: "Porto container name or alias",
    kind: PositionalKind::Utf8,
    max_bytes: 4096,
};
const SYSTEMD_UNIT: PositionalSpec = PositionalSpec {
    name: "unit",
    value_name: "UNIT",
    description: "Systemd unit name",
    kind: PositionalKind::Utf8,
    max_bytes: 255,
};
const DBUS_NAME: PositionalSpec = PositionalSpec {
    name: "name",
    value_name: "NAME",
    description: "D-Bus well-known or unique bus name",
    kind: PositionalKind::Utf8,
    max_bytes: 255,
};
const TARGET_PATH: PositionalSpec = PositionalSpec {
    name: "path",
    value_name: "PATH",
    description: "Target filesystem path",
    kind: PositionalKind::LinuxPath,
    max_bytes: 4096,
};
const SOCKET_PATH_BYTES: usize = 4096;
pub(crate) const PORTO_SOCKET_PATH: &str = "/run/portod.socket";
const BUS_USER_BYTES: usize = 256;

const fn item_limit(default: u64, default_display: &'static str, maximum: u64) -> NumberSpec {
    NumberSpec {
        name: "max-items",
        value_name: "N",
        description: "Maximum number of emitted data records",
        default,
        default_display,
        minimum: 1,
        maximum,
    }
}

const fn items(default: u64, default_display: &'static str, maximum: u64) -> RequestSpec {
    RequestSpec::Items(item_limit(default, default_display, maximum))
}

const fn filesystem_scan(default: u64, default_display: &'static str, maximum: u64) -> RequestSpec {
    RequestSpec::FilesystemScan(item_limit(default, default_display, maximum))
}

/// Capabilities in discovery order; the baseline profile runs its members in this order.
static CAPABILITIES: [CapabilitySpec; 39] = [
    CapabilitySpec {
        capability: CapabilityId::HostInfo,
        id: "host.info",
        command: &["host", "info"],
        description: "Operating-system and host identity facts",
        request: RequestSpec::Empty,
        data_kinds: &["host"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::KernelInfo,
        id: "kernel.info",
        command: &["kernel", "info"],
        description: "Kernel identity and boot command-line facts",
        request: RequestSpec::Empty,
        data_kinds: &["kernel"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::KernelModules,
        id: "kernel.modules",
        command: &["kernel", "modules"],
        description: "Loaded kernel module inventory",
        request: items(8192, "8192", 32_768),
        data_kinds: &["kernel_module"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::KernelSysctls,
        id: "kernel.sysctls",
        command: &["kernel", "sysctls"],
        description: "Pentest-relevant kernel and network sysctls",
        request: RequestSpec::Empty,
        data_kinds: &["kernel_sysctl"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::SecurityPosture,
        id: "security.posture",
        command: &["security", "posture"],
        description: "LSM, boot, platform, and CPU security posture",
        request: items(1024, "1024", 4096),
        data_kinds: &["security_control"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::ProcessList,
        id: "process.list",
        command: &["process", "list"],
        description: "Process summaries with identity and executable paths",
        request: items(4096, "4096", 32_768),
        data_kinds: &["process"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::NetworkInterfaces,
        id: "network.interfaces",
        command: &["network", "interfaces"],
        description: "Network interface metadata",
        request: items(1024, "1024", 4096),
        data_kinds: &["network_interface"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::NetworkAddresses,
        id: "network.addresses",
        command: &["network", "addresses"],
        description: "IPv4 and IPv6 interface addresses",
        request: items(1024, "1024", 4096),
        data_kinds: &["network_address"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::NetworkResolvers,
        id: "network.resolvers",
        command: &["network", "resolvers"],
        description: "Static resolver directives",
        request: RequestSpec::Empty,
        data_kinds: &["resolver_directive"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::NetworkRoutes,
        id: "network.routes",
        command: &["network", "routes"],
        description: "IPv4 and IPv6 route inventory",
        request: items(16_384, "16384", 65_536),
        data_kinds: &["network_route"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::NetworkNeighbors,
        id: "network.neighbors",
        command: &["network", "neighbors"],
        description: "IPv4 neighbor cache inventory",
        request: items(8192, "8192", 32_768),
        data_kinds: &["network_neighbor"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::NetworkSockets,
        id: "network.sockets",
        command: &["network", "sockets"],
        description: "Internet and Unix socket inventory with bounded ownership",
        request: items(32_768, "32768", 100_000),
        data_kinds: &["network_socket"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::NetworkListeners,
        id: "network.listeners",
        command: &["network", "listeners"],
        description: "Listening Internet and Unix sockets with bounded ownership",
        request: items(16_384, "16384", 65_536),
        data_kinds: &["network_socket"],
        baseline: false,
    },
    CapabilitySpec {
        capability: CapabilityId::NetworkFirewall,
        id: "network.firewall",
        command: &["network", "firewall"],
        description: "Runtime table names and static firewall rule evidence",
        request: items(10_000, "10000", 50_000),
        data_kinds: &["firewall_evidence"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::MountList,
        id: "mount.list",
        command: &["mount", "list"],
        description: "Current mount namespace inventory",
        request: items(4096, "4096", 16_384),
        data_kinds: &["mount"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::CgroupInspect,
        id: "cgroup.inspect",
        command: &["cgroup", "inspect"],
        description: "Current process cgroup memberships and limits",
        request: RequestSpec::Empty,
        data_kinds: &["cgroup_membership"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::UserList,
        id: "user.list",
        command: &["user", "list"],
        description: "Local passwd user inventory",
        request: items(10_000, "10000", 50_000),
        data_kinds: &["user"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::GroupList,
        id: "group.list",
        command: &["group", "list"],
        description: "Local group inventory",
        request: items(10_000, "10000", 50_000),
        data_kinds: &["group"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::AuthPosture,
        id: "auth.posture",
        command: &["auth", "posture"],
        description: "Local password state and authentication policy directives",
        request: items(10_000, "10000", 50_000),
        data_kinds: &["auth_control"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::SudoRules,
        id: "sudo.rules",
        command: &["sudo", "rules"],
        description: "Static sudo policy directives",
        request: items(10_000, "10000", 50_000),
        data_kinds: &["sudo_directive"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::PackageList,
        id: "package.list",
        command: &["package", "list"],
        description: "Installed packages from native package databases",
        request: items(50_000, "50000", 200_000),
        data_kinds: &["package"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::ServiceList,
        id: "service.list",
        command: &["service", "list"],
        description: "Static systemd unit and SysV service inventory",
        request: items(20_000, "20000", 100_000),
        data_kinds: &["service"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::ScheduleList,
        id: "schedule.list",
        command: &["schedule", "list"],
        description: "Static cron and systemd timer inventory",
        request: items(10_000, "10000", 100_000),
        data_kinds: &["schedule"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::SystemctlList,
        id: "systemctl.list",
        command: &["systemctl", "list"],
        description: "Runtime systemd unit state from system and user managers",
        request: RequestSpec::SystemctlList(item_limit(20_000, "20000", 100_000)),
        data_kinds: &["systemd_unit_runtime"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::SystemctlInspect,
        id: "systemctl.inspect",
        command: &["systemctl", "inspect"],
        description: "Detailed runtime state for one systemd unit",
        request: RequestSpec::SystemctlInspect,
        data_kinds: &["systemd_unit_runtime"],
        baseline: false,
    },
    CapabilitySpec {
        capability: CapabilityId::DbusList,
        id: "dbus.list",
        command: &["dbus", "list"],
        description: "D-Bus names, activation state, owners, and peer credentials",
        request: RequestSpec::DbusList(item_limit(20_000, "20000", 100_000)),
        data_kinds: &["dbus_record"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::DbusInspect,
        id: "dbus.inspect",
        command: &["dbus", "inspect"],
        description: "Detailed ownership and credentials for one D-Bus name",
        request: RequestSpec::DbusInspect,
        data_kinds: &["dbus_record"],
        baseline: false,
    },
    CapabilitySpec {
        capability: CapabilityId::ContainerList,
        id: "container.list",
        command: &["container", "list"],
        description: "Runtime-agnostic running-container discovery from process cgroups",
        request: items(10_000, "10000", 50_000),
        data_kinds: &["container"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::ContainerInspect,
        id: "container.inspect",
        command: &["container", "inspect"],
        description: "Named runtime-agnostic container context from procfs",
        request: RequestSpec::ContainerInspect,
        data_kinds: &["container_context"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::PortoList,
        id: "porto.list",
        command: &["portoctl", "list"],
        description: "Porto container inventory with complete property evidence",
        request: RequestSpec::PortoList(item_limit(10_000, "10000", 50_000)),
        data_kinds: &[
            "porto_container",
            "porto_property_catalog",
            "porto_property",
        ],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::PortoInspect,
        id: "porto.inspect",
        command: &["portoctl", "inspect"],
        description: "Named Porto snapshot with complete property evidence",
        request: RequestSpec::PortoInspect,
        data_kinds: &[
            "porto_container_context",
            "porto_property_catalog",
            "porto_property",
        ],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::SshServerConfig,
        id: "ssh.server_config",
        command: &["ssh", "server-config"],
        description: "Static OpenSSH server directives",
        request: items(10_000, "10000", 50_000),
        data_kinds: &["ssh_server_directive"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::SshAuthorizedKeys,
        id: "ssh.authorized_keys",
        command: &["ssh", "authorized-keys"],
        description: "Authorized-key fingerprints and options",
        request: items(10_000, "10000", 50_000),
        data_kinds: &["ssh_authorized_key"],
        baseline: true,
    },
    CapabilitySpec {
        capability: CapabilityId::FilesystemPrivilegeSurfaces,
        id: "filesystem.privilege_surfaces",
        command: &["filesystem", "privilege-surfaces"],
        description: "Explicit bounded scan for privilege-relevant filesystem metadata",
        request: filesystem_scan(100_000, "100000", 1_000_000),
        data_kinds: &["filesystem_privilege_surface"],
        baseline: false,
    },
    CapabilitySpec {
        capability: CapabilityId::FilesystemUnixSockets,
        id: "filesystem.unix_sockets",
        command: &["filesystem", "unix-sockets"],
        description: "Explicit bounded scan for filesystem Unix sockets",
        request: filesystem_scan(100_000, "100000", 1_000_000),
        data_kinds: &["filesystem_unix_socket"],
        baseline: false,
    },
    CapabilitySpec {
        capability: CapabilityId::FileStat,
        id: "file.stat",
        command: &["file", "stat"],
        description: "Metadata for one explicitly selected path",
        request: RequestSpec::FileStat,
        data_kinds: &["file_metadata"],
        baseline: false,
    },
    CapabilitySpec {
        capability: CapabilityId::FileRead,
        id: "file.read",
        command: &["file", "read"],
        description: "Chunked content from one explicitly selected regular file",
        request: RequestSpec::FileRead,
        data_kinds: &["file_chunk"],
        baseline: false,
    },
    CapabilitySpec {
        capability: CapabilityId::Overview,
        id: "host.overview",
        command: &["overview"],
        description: "Compact passive host, observer, resource, and subsystem context",
        request: RequestSpec::Overview,
        data_kinds: &["host_overview"],
        baseline: false,
    },
    CapabilitySpec {
        capability: CapabilityId::ProcessInspect,
        id: "process.inspect",
        command: &["process", "inspect"],
        description: "Bounded security context, resources, and limits for one visible process",
        request: RequestSpec::ProcessInspect,
        data_kinds: &["process_detail"],
        baseline: false,
    },
];

const _: () = {
    let mut index = 0;
    while index < CAPABILITIES.len() {
        assert!(CAPABILITIES[index].capability as usize == index);
        index += 1;
    }
};

impl CapabilityId {
    pub fn all() -> impl Iterator<Item = Self> {
        CAPABILITIES.iter().map(|spec| spec.capability)
    }

    pub fn baseline() -> impl Iterator<Item = Self> {
        CAPABILITIES
            .iter()
            .filter(|spec| spec.baseline)
            .map(|spec| spec.capability)
    }

    fn spec(self) -> &'static CapabilitySpec {
        &CAPABILITIES[self as usize]
    }

    pub fn id(self) -> &'static str {
        self.spec().id
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::all().find(|capability| capability.id() == id)
    }

    pub fn command(self) -> &'static [&'static str] {
        self.spec().command
    }

    pub fn description(self) -> &'static str {
        self.spec().description
    }

    pub(crate) fn request_spec(self) -> RequestSpec {
        self.spec().request
    }

    pub fn data_kind(self) -> &'static str {
        self.spec().data_kinds[0]
    }

    pub fn data_kinds(self) -> &'static [&'static str] {
        self.spec().data_kinds
    }

    pub fn safety_class(self) -> &'static str {
        match self.request_spec() {
            RequestSpec::ProcessInspect
            | RequestSpec::FilesystemScan(_)
            | RequestSpec::FileStat
            | RequestSpec::FileRead => "passive_targeted",
            _ => "passive_native",
        }
    }

    pub fn baseline_position(self) -> Option<usize> {
        self.spec().baseline.then(|| {
            CAPABILITIES[..self as usize]
                .iter()
                .filter(|spec| spec.baseline)
                .count()
        })
    }

    pub(crate) fn request_contract(self) -> Value {
        let positionals = self
            .positional()
            .into_iter()
            .map(PositionalSpec::contract)
            .collect::<Vec<_>>();
        let options = self
            .option_specs()
            .into_iter()
            .map(|option| {
                let mut contract = option.contract();
                if option.name == "max-stream-bytes" {
                    contract["requires"] = json!(["include-streams"]);
                }
                if option.name == "all-users" {
                    contract["conflicts_with"] = json!(["socket", "user"]);
                }
                contract
            })
            .collect::<Vec<_>>();
        json!({
            "positionals":positionals,
            "options":options,
        })
    }

    pub(crate) fn access_contract(self) -> Value {
        let request = self.request_spec();
        let scope = match request {
            RequestSpec::ProcessInspect => "explicit_pid",
            RequestSpec::FilesystemScan(_) => "explicit_tree",
            RequestSpec::FileStat | RequestSpec::FileRead => "explicit_path",
            _ => "visible_namespace",
        };
        json!({
            "read_only":true,
            "scope":scope,
            "executes_host_programs":false,
            "opens_inet_connections":false,
            "may_open_unix_socket":matches!(
                request,
                RequestSpec::SystemctlList(_)
                    | RequestSpec::SystemctlInspect
                    | RequestSpec::DbusList(_)
                    | RequestSpec::DbusInspect
                    | RequestSpec::PortoList(_)
                    | RequestSpec::PortoInspect
            ),
        })
    }

    pub(crate) fn positional(self) -> Option<PositionalSpec> {
        match self.request_spec() {
            RequestSpec::ContainerInspect => Some(CONTAINER_NAME),
            RequestSpec::ProcessInspect => Some(PROCESS_PID),
            RequestSpec::PortoInspect => Some(PORTO_NAME),
            RequestSpec::SystemctlInspect => Some(SYSTEMD_UNIT),
            RequestSpec::DbusInspect => Some(DBUS_NAME),
            RequestSpec::FilesystemScan(_) | RequestSpec::FileStat | RequestSpec::FileRead => {
                Some(TARGET_PATH)
            }
            _ => None,
        }
    }

    pub(crate) fn option_specs(self) -> Vec<OptionSpec> {
        let mut options = Vec::with_capacity(6);
        match self.request_spec() {
            RequestSpec::Items(limit)
            | RequestSpec::PortoList(limit)
            | RequestSpec::SystemctlList(limit)
            | RequestSpec::DbusList(limit)
            | RequestSpec::FilesystemScan(limit) => options.push(OptionSpec {
                name: limit.name,
                value_name: limit.value_name,
                description: limit.description,
                kind: OptionKind::Number(limit),
            }),
            _ => {}
        }

        if matches!(self.request_spec(), RequestSpec::Overview) {
            options.push(OptionSpec {
                name: "details",
                value_name: "",
                description: "Include the full passive environment observations",
                kind: OptionKind::Flag,
            });
        }

        if matches!(
            self.request_spec(),
            RequestSpec::PortoList(_) | RequestSpec::PortoInspect
        ) {
            options.extend([
                OptionSpec {
                    name: "socket",
                    value_name: "PATH",
                    description: "Porto daemon Unix socket",
                    kind: OptionKind::LinuxPath {
                        default: PORTO_SOCKET_PATH,
                    },
                },
                OptionSpec {
                    name: "show-sensitive",
                    value_name: "",
                    description: "Collect Porto env properties instead of redacting them",
                    kind: OptionKind::Flag,
                },
                OptionSpec {
                    name: "include-streams",
                    value_name: "",
                    description: "Collect bounded Porto stdout and stderr properties",
                    kind: OptionKind::Flag,
                },
                OptionSpec {
                    name: "max-stream-bytes",
                    value_name: "N",
                    description: "Maximum bytes collected per Porto stream property",
                    kind: OptionKind::Number(NumberSpec {
                        name: "max-stream-bytes",
                        value_name: "N",
                        description: "Maximum bytes collected per Porto stream property",
                        default: 65_536,
                        default_display: "65536",
                        minimum: 1,
                        maximum: 1_048_576,
                    }),
                },
            ]);
        }
        if matches!(
            self.request_spec(),
            RequestSpec::SystemctlList(_)
                | RequestSpec::SystemctlInspect
                | RequestSpec::DbusList(_)
                | RequestSpec::DbusInspect
        ) {
            options.extend([
                OptionSpec {
                    name: "socket",
                    value_name: "PATH",
                    description: "Explicit D-Bus Unix socket",
                    kind: OptionKind::OptionalLinuxPath,
                },
                OptionSpec {
                    name: "user",
                    value_name: "USER",
                    description: "User bus selected by local user name or numeric UID",
                    kind: OptionKind::OptionalUtf8,
                },
            ]);
        }
        if matches!(self, Self::SystemctlList | Self::DbusList) {
            options.push(OptionSpec {
                name: "all-users",
                value_name: "",
                description: "Inspect the system manager and discoverable user managers",
                kind: OptionKind::Flag,
            });
        }

        if self == Self::FileRead {
            options.push(OptionSpec {
                name: "max-bytes",
                value_name: "N",
                description: "Maximum file content bytes to collect",
                kind: OptionKind::Number(NumberSpec {
                    name: "max-bytes",
                    value_name: "N",
                    description: "Maximum file content bytes to collect",
                    default: 262_144,
                    default_display: "262144",
                    minimum: 1,
                    maximum: 16_777_216,
                }),
            });
        }

        if matches!(
            self,
            Self::FilesystemPrivilegeSurfaces | Self::FilesystemUnixSockets
        ) {
            options.extend([
                OptionSpec {
                    name: "max-entries",
                    value_name: "N",
                    description: "Maximum filesystem entries to examine",
                    kind: OptionKind::Number(NumberSpec {
                        name: "max-entries",
                        value_name: "N",
                        description: "Maximum filesystem entries to examine",
                        default: 1_000_000,
                        default_display: "1000000",
                        minimum: 1,
                        maximum: 10_000_000,
                    }),
                },
                OptionSpec {
                    name: "max-depth",
                    value_name: "N",
                    description: "Maximum filesystem traversal depth",
                    kind: OptionKind::Number(NumberSpec {
                        name: "max-depth",
                        value_name: "N",
                        description: "Maximum filesystem traversal depth",
                        default: 64,
                        default_display: "64",
                        minimum: 1,
                        maximum: 128,
                    }),
                },
            ]);
            options.push(OptionSpec {
                name: "include-visible-mounts",
                value_name: "",
                description: "Cross visible mount boundaries encountered below the selected root",
                kind: OptionKind::Flag,
            });
        }

        options
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct GlobalLimits {
    pub deadline_seconds: u64,
    pub max_output_bytes: u64,
    pub max_records: u64,
    pub max_line_bytes: u64,
}

impl GlobalLimits {
    pub const fn safe() -> Self {
        Self {
            deadline_seconds: SAFE_DEADLINE_SECONDS,
            max_output_bytes: SAFE_OUTPUT_BYTES,
            max_records: SAFE_RECORDS,
            max_line_bytes: MAX_LINE_BYTES,
        }
    }

    pub const fn targeted() -> Self {
        Self {
            deadline_seconds: TARGETED_DEADLINE_SECONDS,
            max_output_bytes: TARGETED_OUTPUT_BYTES,
            max_records: TARGETED_RECORDS,
            max_line_bytes: MAX_LINE_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketSelection {
    All,
    Listeners,
    InetAndUnixListeners,
}

impl SocketSelection {
    const fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Listeners => "listeners",
            Self::InetAndUnixListeners => "inet_all_unix_listeners",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemdUnitSelection {
    All,
    SecurityBaseline,
}

impl SystemdUnitSelection {
    const fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::SecurityBaseline => "security_baseline",
        }
    }

    const fn excluded_unit_types(self) -> &'static [&'static str] {
        match self {
            Self::All => &[],
            Self::SecurityBaseline => &["device"],
        }
    }
}

#[derive(Debug)]
pub enum Request {
    Empty,
    Overview {
        details: bool,
    },
    ItemLimit {
        max_items: usize,
    },
    SocketList {
        max_items: usize,
        selection: SocketSelection,
    },
    ContainerInspect {
        name: String,
    },
    ProcessInspect {
        pid: u32,
    },
    PortoList {
        max_items: usize,
        socket: PathBuf,
        show_sensitive: bool,
        include_streams: bool,
        max_stream_bytes: usize,
    },
    PortoInspect {
        name: String,
        socket: PathBuf,
        show_sensitive: bool,
        include_streams: bool,
        max_stream_bytes: usize,
    },
    SystemctlList {
        max_items: usize,
        socket: Option<PathBuf>,
        user: Option<String>,
        all_users: bool,
        unit_selection: SystemdUnitSelection,
    },
    SystemctlInspect {
        unit: String,
        socket: Option<PathBuf>,
        user: Option<String>,
    },
    DbusList {
        max_items: usize,
        socket: Option<PathBuf>,
        user: Option<String>,
        all_users: bool,
    },
    DbusInspect {
        name: String,
        socket: Option<PathBuf>,
        user: Option<String>,
    },
    FilesystemScan {
        path: PathBuf,
        max_items: usize,
        max_entries: usize,
        max_depth: usize,
        include_visible_mounts: bool,
    },
    FileStat {
        path: PathBuf,
    },
    FileRead {
        path: PathBuf,
        max_bytes: u64,
    },
}

impl Request {
    pub fn normalized(&self) -> Value {
        match self {
            Self::Empty => json!({}),
            Self::Overview { details } => json!({"details":details}),
            Self::ItemLimit { max_items } => json!({"max_items": max_items}),
            Self::SocketList {
                max_items,
                selection,
            } => json!({
                "max_items":max_items,
                "socket_selection":selection.id(),
            }),
            Self::ContainerInspect { name } => json!({"name":name}),
            Self::ProcessInspect { pid } => json!({"pid":pid}),
            Self::PortoList {
                max_items,
                socket,
                show_sensitive,
                include_streams,
                max_stream_bytes,
            } => json!({
                "max_items":max_items,
                "socket":linux_path(socket),
                "show_sensitive":show_sensitive,
                "include_streams":include_streams,
                "max_stream_bytes":max_stream_bytes,
            }),
            Self::PortoInspect {
                name,
                socket,
                show_sensitive,
                include_streams,
                max_stream_bytes,
            } => json!({
                "name":name,
                "socket":linux_path(socket),
                "show_sensitive":show_sensitive,
                "include_streams":include_streams,
                "max_stream_bytes":max_stream_bytes,
            }),
            Self::SystemctlList {
                max_items,
                socket,
                user,
                all_users,
                unit_selection,
            } => json!({
                "max_items":max_items,
                "socket":socket.as_deref().map(linux_path),
                "user":user,
                "all_users":all_users,
                "unit_selection":unit_selection.id(),
                "excluded_unit_types":unit_selection.excluded_unit_types(),
            }),
            Self::SystemctlInspect { unit, socket, user } => json!({
                "unit":unit,
                "socket":socket.as_deref().map(linux_path),
                "user":user,
            }),
            Self::DbusList {
                max_items,
                socket,
                user,
                all_users,
            } => json!({
                "max_items":max_items,
                "socket":socket.as_deref().map(linux_path),
                "user":user,
                "all_users":all_users,
            }),
            Self::DbusInspect { name, socket, user } => json!({
                "name":name,
                "socket":socket.as_deref().map(linux_path),
                "user":user,
            }),
            Self::FilesystemScan {
                path,
                max_items,
                max_entries,
                max_depth,
                include_visible_mounts,
            } => json!({
                "path":linux_path(path),
                "max_items":max_items,
                "max_entries":max_entries,
                "max_depth":max_depth,
                "include_visible_mounts":include_visible_mounts,
            }),
            Self::FileStat { path } => json!({"path": linux_path(path)}),
            Self::FileRead { path, max_bytes } => {
                json!({"path": linux_path(path), "max_bytes": max_bytes})
            }
        }
    }
}

#[derive(Debug)]
pub struct Invocation {
    pub capability: CapabilityId,
    pub request: Request,
}

#[derive(Debug)]
pub struct Execution {
    pub invocation_kind: &'static str,
    pub command_id: &'static str,
    pub invocations: Vec<Invocation>,
    pub limits: GlobalLimits,
}

#[derive(Debug)]
pub struct CaptureExecution {
    pub execution: Execution,
    pub destination: CaptureDestination,
}

#[derive(Debug)]
pub enum CaptureDestination {
    Directory(PathBuf),
    Files {
        output: PathBuf,
        receipt: PathBuf,
        stderr: Option<PathBuf>,
    },
}

#[derive(Debug)]
pub struct CliError {
    message: String,
    details: Value,
}

impl CliError {
    fn clap(error: clap_builder::Error) -> Self {
        let kind = match error.kind() {
            ErrorKind::InvalidValue => "invalid_value",
            ErrorKind::UnknownArgument => "unknown_argument",
            ErrorKind::InvalidSubcommand => "invalid_subcommand",
            ErrorKind::NoEquals => "missing_equals",
            ErrorKind::ValueValidation => "value_validation",
            ErrorKind::TooFewValues => "too_few_values",
            ErrorKind::TooManyValues => "too_many_values",
            ErrorKind::WrongNumberOfValues => "wrong_number_of_values",
            ErrorKind::ArgumentConflict => "argument_conflict",
            ErrorKind::MissingRequiredArgument => "missing_required_argument",
            ErrorKind::MissingSubcommand => "missing_subcommand",
            _ => "invalid_request",
        };
        Self {
            message: error.to_string().trim_end().to_owned(),
            details: json!({"kind":kind}),
        }
    }

    fn unknown_capability(id: &str) -> Self {
        Self {
            message: format!("unknown capability ID: {id}"),
            details: json!({"kind":"unknown_capability", "capability":id}),
        }
    }

    pub fn details(&self) -> Value {
        self.details.clone()
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self {
            message,
            details: json!({"kind":"request_validation"}),
        }
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

pub enum Action {
    Help(String),
    Execute(Execution),
    Capture(CaptureExecution),
    Validate(PathBuf),
    Summarize(PathBuf),
    Capabilities(Option<CapabilityId>),
}

pub fn parse(mut argv: Vec<OsString>) -> Result<Action, CliError> {
    let program = if argv.is_empty() {
        OsString::from("vzik")
    } else {
        argv.remove(0)
    };
    let argv = strip_capture_prefix(argv).map_err(CliError::from)?;
    if argv.first().and_then(|value| value.to_str()) == Some("capture") {
        return parse_capture(program, &argv[1..]);
    }

    parse_core(program, argv)
}

fn parse_core(program: OsString, argv: Vec<OsString>) -> Result<Action, CliError> {
    if argv.is_empty() {
        return Ok(Action::Help(render_root_help()?));
    }

    let mut clap_argv = Vec::with_capacity(argv.len() + 1);
    clap_argv.push(program);
    clap_argv.extend(argv);
    let matches = match root_command().try_get_matches_from(clap_argv) {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) =>
        {
            return Ok(Action::Help(error.to_string()));
        }
        Err(error) => return Err(CliError::clap(error)),
    };

    let (command, matches) = matches
        .subcommand()
        .ok_or_else(|| CliError::from("a command is required"))?;
    match command {
        "validate" => Ok(Action::Validate(path_value(matches, TARGET_PATH)?)),
        "summarize" => Ok(Action::Summarize(path_value(matches, TARGET_PATH)?)),
        "capabilities" => {
            let selected = matches
                .get_one::<String>("capability")
                .map(|id| CapabilityId::from_id(id).ok_or_else(|| CliError::unknown_capability(id)))
                .transpose()?;
            Ok(Action::Capabilities(selected))
        }
        "collect" => parse_collect(matches).map_err(CliError::from),
        group => parse_capability(group, matches).map_err(CliError::from),
    }
}

fn parse_capture(program: OsString, arguments: &[OsString]) -> Result<Action, CliError> {
    const HELP: &str = "Usage: vzik capture <COLLECTION COMMAND> --output-dir DIR\n\
                       \x20      vzik capture <COLLECTION COMMAND> --output PATH --receipt PATH [--stderr PATH]\n\
                       Output options may appear before or after collection arguments, but before --.\n\
                       --output-dir creates a new private directory containing capture.jsonl, receipt.json, and stderr.jsonl.\n\
                       It cannot be combined with --output, --receipt, or --stderr. Existing paths are never reused.\n\
                       The command validates the complete stream, seals and verifies the receipt, then writes a JSON coverage summary.\n\
                       Files are written directly; interruption can leave capture or stderr without a receipt.\n\
                       Examples:\n\
                         vzik capture overview --output-dir evidence\n\
                         vzik capture process inspect 1234 --output-dir process-evidence\n\
                         vzik capture collect --output baseline.jsonl --receipt baseline.receipt.json\n";
    if arguments.is_empty() {
        return Ok(Action::Help(HELP.to_owned()));
    }

    let mut output_dir = None;
    let mut output = None;
    let mut receipt = None;
    let mut stderr = None;
    let mut nested = Vec::with_capacity(arguments.len());
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--" {
            nested.extend_from_slice(&arguments[index..]);
            break;
        }
        let destination = match arguments[index].to_str() {
            Some("-h" | "--help") => return Ok(Action::Help(HELP.to_owned())),
            Some("--output-dir") => Some((&mut output_dir, "--output-dir")),
            Some("--output") => Some((&mut output, "--output")),
            Some("--receipt") => Some((&mut receipt, "--receipt")),
            Some("--stderr") => Some((&mut stderr, "--stderr")),
            _ => None,
        };
        if let Some((slot, name)) = destination {
            if slot.is_some() {
                return Err(format!("{name} may be specified only once").into());
            }
            let value = arguments
                .get(index + 1)
                .ok_or_else(|| format!("{name} requires a path"))?;
            *slot = Some(bounded_path(value, 4096, name)?);
            index += 2;
        } else {
            nested.push(arguments[index].clone());
            index += 1;
        }
    }

    let destination = if let Some(directory) = output_dir {
        if output.is_some() || receipt.is_some() || stderr.is_some() {
            return Err(
                "--output-dir cannot be combined with --output, --receipt, or --stderr".into(),
            );
        }
        CaptureDestination::Directory(directory)
    } else {
        CaptureDestination::Files {
            output: output
                .ok_or_else(|| "capture requires --output-dir DIR or --output PATH".to_string())?,
            receipt: receipt
                .ok_or_else(|| "capture requires --receipt PATH with --output".to_string())?,
            stderr,
        }
    };
    let action = parse_core(program, nested)?;
    let Action::Execute(execution) = action else {
        return Err("capture requires a collection command".into());
    };

    Ok(Action::Capture(CaptureExecution {
        execution,
        destination,
    }))
}

fn parse_collect(matches: &ArgMatches) -> Result<Action, String> {
    Ok(Action::Execute(Execution {
        invocation_kind: "profile",
        command_id: "baseline-v3",
        invocations: CapabilityId::baseline()
            .map(|capability| {
                Ok(Invocation {
                    capability,
                    request: baseline_request(capability)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        limits: limits_from_matches(matches, true),
    }))
}

fn parse_capability(group: &str, matches: &ArgMatches) -> Result<Action, String> {
    let (capability, matches) = if let Some(capability) =
        CapabilityId::all().find(|capability| capability.command() == [group])
    {
        (capability, matches)
    } else {
        let (leaf, matches) = matches
            .subcommand()
            .ok_or_else(|| format!("a {group} command is required"))?;
        let capability = CapabilityId::all()
            .find(|capability| capability.command() == [group, leaf])
            .ok_or_else(|| format!("unknown capability command: {group} {leaf}"))?;
        (capability, matches)
    };

    Ok(Action::Execute(Execution {
        invocation_kind: "capability",
        command_id: capability.id(),
        invocations: vec![Invocation {
            capability,
            request: request_from_matches(capability, matches)?,
        }],
        limits: limits_from_matches(matches, false),
    }))
}

fn request_from_matches(capability: CapabilityId, matches: &ArgMatches) -> Result<Request, String> {
    Ok(match capability.request_spec() {
        RequestSpec::Empty => Request::Empty,
        RequestSpec::Overview => Request::Overview {
            details: matches.get_flag("details"),
        },
        RequestSpec::Items(limit) => {
            let max_items = usize_value(matches, limit.name)?;
            match capability {
                CapabilityId::NetworkSockets => Request::SocketList {
                    max_items,
                    selection: SocketSelection::All,
                },
                CapabilityId::NetworkListeners => Request::SocketList {
                    max_items,
                    selection: SocketSelection::Listeners,
                },
                _ => Request::ItemLimit { max_items },
            }
        }
        RequestSpec::ContainerInspect => Request::ContainerInspect {
            name: string_value(matches, CONTAINER_NAME)?,
        },
        RequestSpec::ProcessInspect => Request::ProcessInspect {
            pid: u32::try_from(
                *matches
                    .get_one::<u64>(PROCESS_PID.name)
                    .ok_or_else(|| "PID is required".to_owned())?,
            )
            .map_err(|_| "PID exceeds supported range".to_owned())?,
        },
        RequestSpec::PortoList(limit) => {
            let (socket, show_sensitive, include_streams, max_stream_bytes) =
                porto_values(matches)?;
            Request::PortoList {
                max_items: usize_value(matches, limit.name)?,
                socket,
                show_sensitive,
                include_streams,
                max_stream_bytes,
            }
        }
        RequestSpec::PortoInspect => {
            let (socket, show_sensitive, include_streams, max_stream_bytes) =
                porto_values(matches)?;
            Request::PortoInspect {
                name: string_value(matches, PORTO_NAME)?,
                socket,
                show_sensitive,
                include_streams,
                max_stream_bytes,
            }
        }
        RequestSpec::SystemctlList(limit) => {
            let (socket, user, all_users) = systemctl_values(matches)?;
            Request::SystemctlList {
                max_items: usize_value(matches, limit.name)?,
                socket,
                user,
                all_users,
                unit_selection: SystemdUnitSelection::All,
            }
        }
        RequestSpec::SystemctlInspect => {
            let (socket, user, _) = systemctl_values(matches)?;
            Request::SystemctlInspect {
                unit: string_value(matches, SYSTEMD_UNIT)?,
                socket,
                user,
            }
        }
        RequestSpec::DbusList(limit) => {
            let (socket, user, all_users) = systemctl_values(matches)?;
            Request::DbusList {
                max_items: usize_value(matches, limit.name)?,
                socket,
                user,
                all_users,
            }
        }
        RequestSpec::DbusInspect => {
            let (socket, user, _) = systemctl_values(matches)?;
            Request::DbusInspect {
                name: string_value(matches, DBUS_NAME)?,
                socket,
                user,
            }
        }
        RequestSpec::FilesystemScan(_) => Request::FilesystemScan {
            path: path_value(matches, TARGET_PATH)?,
            max_items: usize_value(matches, "max-items")?,
            max_entries: usize_value(matches, "max-entries")?,
            max_depth: usize_value(matches, "max-depth")?,
            include_visible_mounts: matches.get_flag("include-visible-mounts"),
        },
        RequestSpec::FileStat => Request::FileStat {
            path: path_value(matches, TARGET_PATH)?,
        },
        RequestSpec::FileRead => Request::FileRead {
            path: path_value(matches, TARGET_PATH)?,
            max_bytes: number_value(matches, "max-bytes"),
        },
    })
}

fn baseline_request(capability: CapabilityId) -> Result<Request, String> {
    Ok(match capability.request_spec() {
        RequestSpec::Empty => Request::Empty,
        RequestSpec::Items(limit) => {
            let max_items = usize::try_from(limit.default)
                .map_err(|_| format!("{} does not fit this platform", limit.name))?;
            match capability {
                CapabilityId::NetworkSockets => Request::SocketList {
                    max_items,
                    selection: SocketSelection::InetAndUnixListeners,
                },
                CapabilityId::NetworkListeners => Request::SocketList {
                    max_items,
                    selection: SocketSelection::Listeners,
                },
                _ => Request::ItemLimit { max_items },
            }
        }
        RequestSpec::ContainerInspect => Request::ContainerInspect {
            name: "self".into(),
        },
        RequestSpec::PortoList(limit) => Request::PortoList {
            max_items: usize::try_from(limit.default)
                .map_err(|_| format!("{} does not fit this platform", limit.name))?,
            socket: PathBuf::from(PORTO_SOCKET_PATH),
            show_sensitive: false,
            include_streams: false,
            max_stream_bytes: 0,
        },
        RequestSpec::PortoInspect => Request::PortoInspect {
            name: "self".into(),
            socket: PathBuf::from(PORTO_SOCKET_PATH),
            show_sensitive: false,
            include_streams: false,
            max_stream_bytes: 0,
        },
        RequestSpec::SystemctlList(limit) => Request::SystemctlList {
            max_items: usize::try_from(limit.default)
                .map_err(|_| format!("{} does not fit this platform", limit.name))?,
            socket: None,
            user: None,
            all_users: true,
            unit_selection: SystemdUnitSelection::SecurityBaseline,
        },
        RequestSpec::DbusList(limit) => Request::DbusList {
            max_items: usize::try_from(limit.default)
                .map_err(|_| format!("{} does not fit this platform", limit.name))?,
            socket: None,
            user: None,
            all_users: true,
        },
        RequestSpec::SystemctlInspect
        | RequestSpec::Overview
        | RequestSpec::ProcessInspect
        | RequestSpec::DbusInspect
        | RequestSpec::FilesystemScan(_)
        | RequestSpec::FileStat
        | RequestSpec::FileRead => {
            return Err(format!("{} has no safe baseline request", capability.id()));
        }
    })
}

fn porto_values(matches: &ArgMatches) -> Result<(PathBuf, bool, bool, usize), String> {
    let socket = path_option_value(matches, "socket", SOCKET_PATH_BYTES)?;
    let show_sensitive = matches.get_flag("show-sensitive");
    let include_streams = matches.get_flag("include-streams");
    let max_stream_requested =
        matches.value_source("max-stream-bytes") == Some(ValueSource::CommandLine);
    if !include_streams && max_stream_requested {
        return Err("--max-stream-bytes requires --include-streams".into());
    }

    let max_stream_bytes = if include_streams {
        usize_value(matches, "max-stream-bytes")?
    } else {
        0
    };
    Ok((socket, show_sensitive, include_streams, max_stream_bytes))
}

fn systemctl_values(
    matches: &ArgMatches,
) -> Result<(Option<PathBuf>, Option<String>, bool), String> {
    let socket = optional_path_value(matches, "socket", SOCKET_PATH_BYTES)?;
    let user = optional_string_value(matches, "user", BUS_USER_BYTES)?;
    let all_users = matches
        .try_get_one::<bool>("all-users")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    Ok((socket, user, all_users))
}

fn root_command() -> Command {
    let mut command = Command::new("vzik")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Bounded agent-first Linux host collector")
        .after_help(
            "Collection commands write protocol-v4 JSONL and never execute host programs or open \
             INET connections. capture writes only its explicit output paths or a new private \
             directory selected by --output-dir. portoctl, systemctl, and dbus may connect only \
             to configured Unix sockets.\n\n\
             Start with vzik overview for passive host context; use vzik process inspect PID \
             for one process. collect remains the security baseline.\n\n\
             Machine-readable discovery:\n  \
               vzik capabilities\n  \
               vzik capabilities porto.list",
        )
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("capture")
                .about("Collect, validate, seal, verify, and summarize evidence"),
        )
        .subcommand(add_global_limit_args(
            Command::new("collect").about("Collect the bounded baseline-v3 security profile"),
            true,
        ))
        .subcommand(stream_command(
            "validate",
            "Validate a protocol-v4 JSONL stream",
        ))
        .subcommand(stream_command(
            "summarize",
            "Summarize stream outcomes and diagnostics",
        ))
        .subcommand(
            Command::new("capabilities")
                .about("List capabilities or describe one exact capability ID")
                .arg(
                    Arg::new("capability")
                        .value_name("CAPABILITY_ID")
                        .help("Exact capability ID from the compact index")
                        .required(false)
                        .value_parser(NonEmptyStringValueParser::new()),
                ),
        );

    for capability in CapabilityId::all() {
        let group = capability.command()[0];
        if capability.command().len() == 1 {
            command = command.subcommand(capability_command(capability));
            continue;
        }
        if command.find_subcommand(group).is_none() {
            command = command.subcommand(
                Command::new(group)
                    .about(group_description(group))
                    .subcommand_required(true)
                    .arg_required_else_help(true),
            );
        }

        command = command.mut_subcommand(group, |group_command| {
            group_command.subcommand(capability_command(capability))
        });
    }
    command
}

fn stream_command(name: &'static str, description: &'static str) -> Command {
    Command::new(name).about(description).arg(
        Arg::new(TARGET_PATH.name)
            .value_name(TARGET_PATH.value_name)
            .help(TARGET_PATH.description)
            .required(true)
            .value_parser(OsStringValueParser::new()),
    )
}

fn capability_command(capability: CapabilityId) -> Command {
    let mut command = Command::new(
        *capability
            .command()
            .last()
            .expect("capability has a command"),
    )
    .about(capability.description());
    if let Some(positional) = capability.positional() {
        let parser = match positional.kind {
            PositionalKind::Utf8 => ValueParser::new(NonEmptyStringValueParser::new()),
            PositionalKind::LinuxPath => ValueParser::new(OsStringValueParser::new()),
            PositionalKind::Pid => ValueParser::new(
                clap_builder::builder::RangedU64ValueParser::<u64>::new()
                    .range(1..=i32::MAX as u64),
            ),
        };
        command = command.arg(
            Arg::new(positional.name)
                .value_name(positional.value_name)
                .help(positional.description)
                .required(true)
                .value_parser(parser),
        );
    }

    for option in capability.option_specs() {
        command = command.arg(option_arg(option));
    }

    if matches!(
        capability,
        CapabilityId::SystemctlList | CapabilityId::DbusList
    ) {
        command = command.mut_arg("all-users", |argument| {
            argument.conflicts_with_all(["socket", "user"])
        });
    }

    if capability == CapabilityId::FilesystemUnixSockets {
        command = command
            .long_about(
                "Explicit bounded scan for filesystem Unix sockets. By default the scan stays on \
                 the filesystem containing PATH and reports skipped visible mount boundaries. \
                 Pass --include-visible-mounts only when mounted descendants are in scope.",
            )
            .after_help(
                "Examples:\n  \
                   vzik filesystem unix-sockets /run\n  \
                   vzik filesystem unix-sockets --include-visible-mounts -- /srv",
            );
    }
    add_global_limit_args(command, false)
}

fn option_arg(spec: OptionSpec) -> Arg {
    let argument = Arg::new(spec.name).long(spec.name).help(spec.description);
    match spec.kind {
        OptionKind::Number(number) => argument
            .value_name(spec.value_name)
            .default_value(number.default_display)
            .value_parser(
                clap_builder::builder::RangedU64ValueParser::<u64>::new()
                    .range(number.minimum..=number.maximum),
            ),
        OptionKind::Flag => argument.action(ArgAction::SetTrue),
        OptionKind::LinuxPath { default } => argument
            .value_name(spec.value_name)
            .default_value(default)
            .value_parser(OsStringValueParser::new()),
        OptionKind::OptionalLinuxPath => argument
            .value_name(spec.value_name)
            .value_parser(OsStringValueParser::new()),
        OptionKind::OptionalUtf8 => argument
            .value_name(spec.value_name)
            .value_parser(NonEmptyStringValueParser::new()),
    }
}

fn add_global_limit_args(mut command: Command, safe_profile: bool) -> Command {
    for limit in global_limit_specs(safe_profile) {
        command = command.arg(
            Arg::new(limit.name)
                .long(limit.name)
                .value_name(limit.value_name)
                .help(limit.description)
                .default_value(limit.default_display)
                .value_parser(
                    clap_builder::builder::RangedU64ValueParser::<u64>::new()
                        .range(limit.minimum..=limit.maximum),
                ),
        );
    }
    command
}

pub(crate) fn global_limit_specs(safe_profile: bool) -> [NumberSpec; 3] {
    let (deadline_default, deadline_display, deadline_maximum) = if safe_profile {
        (SAFE_DEADLINE_SECONDS, "2400", SAFE_DEADLINE_SECONDS)
    } else {
        (TARGETED_DEADLINE_SECONDS, "600", TARGETED_DEADLINE_HARD)
    };
    let (records_default, records_display) = if safe_profile {
        (SAFE_RECORDS, "250000")
    } else {
        (TARGETED_RECORDS, "1000000")
    };
    [
        NumberSpec {
            name: "deadline-seconds",
            value_name: "N",
            description: "Cooperative wall-clock deadline checked between bounded operations",
            default: deadline_default,
            default_display: deadline_display,
            minimum: 1,
            maximum: deadline_maximum,
        },
        NumberSpec {
            name: "max-records",
            value_name: "N",
            description: "Maximum protocol records",
            default: records_default,
            default_display: records_display,
            minimum: 32,
            maximum: records_default,
        },
        NumberSpec {
            name: "max-output-bytes",
            value_name: "N",
            description: "Maximum JSONL output bytes",
            default: SAFE_OUTPUT_BYTES,
            default_display: "268435456",
            minimum: 262_144,
            maximum: SAFE_OUTPUT_BYTES,
        },
    ]
}

fn limits_from_matches(matches: &ArgMatches, safe_profile: bool) -> GlobalLimits {
    let defaults = if safe_profile {
        GlobalLimits::safe()
    } else {
        GlobalLimits::targeted()
    };

    GlobalLimits {
        deadline_seconds: matches
            .get_one::<u64>("deadline-seconds")
            .copied()
            .unwrap_or(defaults.deadline_seconds),
        max_records: matches
            .get_one::<u64>("max-records")
            .copied()
            .unwrap_or(defaults.max_records),
        max_output_bytes: matches
            .get_one::<u64>("max-output-bytes")
            .copied()
            .unwrap_or(defaults.max_output_bytes),
        max_line_bytes: MAX_LINE_BYTES,
    }
}

fn group_description(group: &str) -> &'static str {
    match group {
        "host" => "Inspect host identity",
        "kernel" => "Inspect kernel state",
        "security" => "Inspect security posture",
        "process" => "Inspect processes",
        "network" => "Inspect network state",
        "mount" => "Inspect mounts",
        "cgroup" => "Inspect cgroups",
        "user" => "Inspect local users",
        "group" => "Inspect local groups",
        "auth" => "Inspect authentication policy",
        "sudo" => "Inspect sudo policy",
        "package" => "Inspect installed packages",
        "service" => "Inspect services",
        "schedule" => "Inspect scheduled tasks",
        "container" => "Inspect runtime-agnostic container state",
        "portoctl" => "Inspect containers through the Porto API",
        "systemctl" => "Inspect systemd managers through D-Bus",
        "dbus" => "Inspect D-Bus names and peer credentials",
        "ssh" => "Inspect SSH configuration",
        "filesystem" => "Inspect filesystem security surfaces",
        "file" => "Inspect explicit files",
        _ => "Inspect system state",
    }
}

fn render_root_help() -> Result<String, String> {
    let mut output = Vec::new();
    root_command()
        .write_long_help(&mut output)
        .map_err(|error| format!("cannot render help: {error}"))?;
    String::from_utf8(output).map_err(|_| "generated help is not UTF-8".into())
}

fn number_value(matches: &ArgMatches, name: &str) -> u64 {
    *matches
        .get_one::<u64>(name)
        .expect("Clap supplies every numeric default")
}

fn usize_value(matches: &ArgMatches, name: &str) -> Result<usize, String> {
    usize::try_from(number_value(matches, name))
        .map_err(|_| format!("--{name} does not fit this platform"))
}

fn string_value(matches: &ArgMatches, spec: PositionalSpec) -> Result<String, String> {
    let value = matches
        .get_one::<String>(spec.name)
        .expect("Clap enforces required UTF-8 positional");
    if value.len() > spec.max_bytes {
        return Err(format!(
            "{} must contain between 1 and {} bytes",
            spec.description, spec.max_bytes
        ));
    }
    Ok(value.clone())
}

fn path_value(matches: &ArgMatches, spec: PositionalSpec) -> Result<PathBuf, String> {
    let value = matches
        .get_one::<OsString>(spec.name)
        .expect("Clap enforces required path positional");
    bounded_path(value, spec.max_bytes, spec.description)
}

fn path_option_value(
    matches: &ArgMatches,
    name: &str,
    max_bytes: usize,
) -> Result<PathBuf, String> {
    let value = matches
        .get_one::<OsString>(name)
        .expect("Clap supplies the path default");
    bounded_path(value, max_bytes, &format!("--{name}"))
}

fn optional_path_value(
    matches: &ArgMatches,
    name: &str,
    max_bytes: usize,
) -> Result<Option<PathBuf>, String> {
    matches
        .get_one::<OsString>(name)
        .map(|value| bounded_path(value, max_bytes, &format!("--{name}")))
        .transpose()
}

fn optional_string_value(
    matches: &ArgMatches,
    name: &str,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    let Some(value) = matches.get_one::<String>(name) else {
        return Ok(None);
    };
    if value.len() > max_bytes {
        return Err(format!(
            "--{name} must contain between 1 and {max_bytes} bytes"
        ));
    }
    Ok(Some(value.clone()))
}

fn bounded_path(value: &OsStr, max_bytes: usize, description: &str) -> Result<PathBuf, String> {
    let length = os_len(value);
    if length == 0 || length > max_bytes {
        return Err(format!(
            "{description} must contain between 1 and {max_bytes} bytes"
        ));
    }
    Ok(PathBuf::from(value))
}

fn strip_capture_prefix(mut args: Vec<OsString>) -> Result<Vec<OsString>, String> {
    let mut index = 0;
    while index < args.len() {
        let expected = match args[index].to_str() {
            Some("--output") => Some(&["json", "jsonl"][..]),
            Some("--log-format") => Some(&["json"][..]),
            Some("--log-level") => Some(&["error"][..]),
            _ => None,
        };

        let Some(allowed) = expected else { break };

        if index + 1 >= args.len() {
            return Err(format!(
                "{} requires a value",
                args[index].to_string_lossy()
            ));
        }

        let value = args[index + 1]
            .to_str()
            .ok_or_else(|| format!("{} value must be UTF-8", args[index].to_string_lossy()))?;

        if !allowed.contains(&value) {
            return Err(format!(
                "unsupported {} value: {value}",
                args[index].to_string_lossy()
            ));
        }

        index += 2;
    }

    args.drain(..index);
    Ok(args)
}

fn os_len(value: &OsStr) -> usize {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().len()
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().len()
    }
}

pub(crate) fn linux_path(path: &std::path::Path) -> Value {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        crate::protocol::linux_bytes(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        json!({"display": path.to_string_lossy()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    #[test]
    fn process_inspection_rejects_non_pid_inputs() {
        for pid in ["0", "-1", "2147483648", "1/../2", "self", ""] {
            assert!(
                parse(args(&["vzik", "process", "inspect", pid])).is_err(),
                "accepted invalid PID {pid:?}",
            );
        }
    }

    #[test]
    fn capability_discovery_accepts_only_exact_catalog_ids() {
        let Action::Capabilities(None) =
            parse(args(&["vzik", "capabilities"])).expect("parse capability index")
        else {
            panic!("expected capability index");
        };
        let Action::Capabilities(Some(capability)) =
            parse(args(&["vzik", "capabilities", "porto.list"])).expect("parse capability detail")
        else {
            panic!("expected capability detail");
        };
        assert_eq!(capability, CapabilityId::PortoList);

        let Err(error) = parse(args(&["vzik", "capabilities", "porto.unknown"])) else {
            panic!("unknown capability must fail");
        };
        assert_eq!(error.details()["kind"], "unknown_capability");
        assert_eq!(error.details()["capability"], "porto.unknown");
    }

    #[test]
    fn baseline_v3_declares_narrow_network_and_systemd_scope() {
        let Action::Execute(execution) = parse(args(&["vzik", "collect"])).expect("parse baseline")
        else {
            panic!("expected baseline execution");
        };

        assert_eq!(execution.command_id, "baseline-v3");
        assert!(
            !execution
                .invocations
                .iter()
                .any(|invocation| invocation.capability == CapabilityId::NetworkListeners)
        );
        let sockets = execution
            .invocations
            .iter()
            .find(|invocation| invocation.capability == CapabilityId::NetworkSockets)
            .expect("baseline socket inventory");
        assert_eq!(
            sockets.request.normalized()["socket_selection"],
            "inet_all_unix_listeners"
        );
        let systemd = execution
            .invocations
            .iter()
            .find(|invocation| invocation.capability == CapabilityId::SystemctlList)
            .expect("baseline systemd inventory");
        assert_eq!(
            systemd.request.normalized()["excluded_unit_types"],
            json!(["device"])
        );

        let Action::Execute(sockets) =
            parse(args(&["vzik", "network", "sockets"])).expect("parse socket inventory")
        else {
            panic!("expected socket execution");
        };
        assert_eq!(
            sockets.invocations[0].request.normalized()["socket_selection"],
            "all"
        );
        let Action::Execute(systemd) =
            parse(args(&["vzik", "systemctl", "list"])).expect("parse systemd inventory")
        else {
            panic!("expected systemd execution");
        };
        assert_eq!(
            systemd.invocations[0].request.normalized()["unit_selection"],
            "all"
        );
    }

    #[test]
    fn limits_reject_values_above_hard_bounds() {
        assert!(parse(args(&["vzik", "process", "list", "--max-items", "32768"])).is_ok());
        assert!(parse(args(&["vzik", "process", "list", "--max-items", "32769"])).is_err());
    }

    #[test]
    fn privilege_surface_scan_is_explicit_only() {
        assert!(
            CapabilityId::FilesystemPrivilegeSurfaces
                .baseline_position()
                .is_none()
        );
        let action = parse(args(&[
            "vzik",
            "filesystem",
            "privilege-surfaces",
            "/tmp",
            "--max-items",
            "7",
            "--max-entries",
            "11",
            "--max-depth",
            "3",
        ]))
        .expect("parse");
        let Action::Execute(execution) = action else {
            panic!("expected collection action");
        };
        let invocation = execution
            .invocations
            .into_iter()
            .next()
            .expect("invocation");
        assert_eq!(
            invocation.capability,
            CapabilityId::FilesystemPrivilegeSurfaces
        );
        assert!(matches!(
            invocation.request,
            Request::FilesystemScan {
                max_items: 7,
                max_entries: 11,
                max_depth: 3,
                ..
            }
        ));
    }

    #[test]
    fn stream_limit_requires_stream_collection() {
        assert!(
            parse(args(&[
                "vzik",
                "portoctl",
                "inspect",
                "self",
                "--max-stream-bytes",
                "4096",
            ]))
            .is_err()
        );
        assert!(
            parse(args(&[
                "vzik",
                "portoctl",
                "inspect",
                "self",
                "--include-streams",
                "--max-stream-bytes",
                "4096",
            ]))
            .is_ok()
        );
    }

    #[test]
    fn bus_commands_reject_conflicting_scope_selection() {
        assert!(
            parse(args(&[
                "vzik",
                "dbus",
                "list",
                "--all-users",
                "--socket",
                "/tmp/bus",
            ]))
            .is_err()
        );
        assert!(
            parse(args(&[
                "vzik",
                "systemctl",
                "list",
                "--all-users",
                "--user",
                "alice"
            ]))
            .is_err()
        );
    }
}
