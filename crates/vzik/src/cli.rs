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
}

impl PositionalKind {
    const fn id(self) -> &'static str {
        match self {
            Self::Utf8 => "utf8_string",
            Self::LinuxPath => "linux_path",
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
        json!({
            "name":self.name,
            "value_name":self.value_name,
            "description":self.description,
            "type":self.kind.id(),
            "required":true,
            "max_bytes":self.max_bytes,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum RequestSpec {
    Empty,
    Items(NumberSpec),
    ContainerInspect,
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

#[derive(Clone, Copy)]
pub(crate) struct CapabilitySpec {
    pub command: &'static [&'static str],
    pub description: &'static str,
    pub request: RequestSpec,
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

const fn capability(
    command: &'static [&'static str],
    description: &'static str,
    request: RequestSpec,
) -> CapabilitySpec {
    CapabilitySpec {
        command,
        description,
        request,
    }
}

const fn items(default: u64, default_display: &'static str, maximum: u64) -> RequestSpec {
    RequestSpec::Items(item_limit(default, default_display, maximum))
}

const fn filesystem_scan(default: u64, default_display: &'static str, maximum: u64) -> RequestSpec {
    RequestSpec::FilesystemScan(item_limit(default, default_display, maximum))
}

impl CapabilityId {
    pub const ALL: [Self; 37] = [
        Self::HostInfo,
        Self::KernelInfo,
        Self::KernelModules,
        Self::KernelSysctls,
        Self::SecurityPosture,
        Self::ProcessList,
        Self::NetworkInterfaces,
        Self::NetworkAddresses,
        Self::NetworkResolvers,
        Self::NetworkRoutes,
        Self::NetworkNeighbors,
        Self::NetworkSockets,
        Self::NetworkListeners,
        Self::NetworkFirewall,
        Self::MountList,
        Self::CgroupInspect,
        Self::UserList,
        Self::GroupList,
        Self::AuthPosture,
        Self::SudoRules,
        Self::PackageList,
        Self::ServiceList,
        Self::ScheduleList,
        Self::SystemctlList,
        Self::SystemctlInspect,
        Self::DbusList,
        Self::DbusInspect,
        Self::ContainerList,
        Self::ContainerInspect,
        Self::PortoList,
        Self::PortoInspect,
        Self::SshServerConfig,
        Self::SshAuthorizedKeys,
        Self::FilesystemPrivilegeSurfaces,
        Self::FilesystemUnixSockets,
        Self::FileStat,
        Self::FileRead,
    ];

    pub const BASELINE: [Self; 30] = [
        Self::HostInfo,
        Self::KernelInfo,
        Self::KernelModules,
        Self::KernelSysctls,
        Self::SecurityPosture,
        Self::ProcessList,
        Self::NetworkInterfaces,
        Self::NetworkAddresses,
        Self::NetworkResolvers,
        Self::NetworkRoutes,
        Self::NetworkNeighbors,
        Self::NetworkSockets,
        Self::NetworkFirewall,
        Self::MountList,
        Self::CgroupInspect,
        Self::UserList,
        Self::GroupList,
        Self::AuthPosture,
        Self::SudoRules,
        Self::PackageList,
        Self::ServiceList,
        Self::ScheduleList,
        Self::SystemctlList,
        Self::DbusList,
        Self::ContainerList,
        Self::ContainerInspect,
        Self::PortoList,
        Self::PortoInspect,
        Self::SshServerConfig,
        Self::SshAuthorizedKeys,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::HostInfo => "host.info",
            Self::KernelInfo => "kernel.info",
            Self::KernelModules => "kernel.modules",
            Self::KernelSysctls => "kernel.sysctls",
            Self::SecurityPosture => "security.posture",
            Self::ProcessList => "process.list",
            Self::NetworkInterfaces => "network.interfaces",
            Self::NetworkAddresses => "network.addresses",
            Self::NetworkResolvers => "network.resolvers",
            Self::NetworkRoutes => "network.routes",
            Self::NetworkNeighbors => "network.neighbors",
            Self::NetworkSockets => "network.sockets",
            Self::NetworkListeners => "network.listeners",
            Self::NetworkFirewall => "network.firewall",
            Self::MountList => "mount.list",
            Self::CgroupInspect => "cgroup.inspect",
            Self::UserList => "user.list",
            Self::GroupList => "group.list",
            Self::AuthPosture => "auth.posture",
            Self::SudoRules => "sudo.rules",
            Self::PackageList => "package.list",
            Self::ServiceList => "service.list",
            Self::ScheduleList => "schedule.list",
            Self::SystemctlList => "systemctl.list",
            Self::SystemctlInspect => "systemctl.inspect",
            Self::DbusList => "dbus.list",
            Self::DbusInspect => "dbus.inspect",
            Self::ContainerList => "container.list",
            Self::ContainerInspect => "container.inspect",
            Self::PortoList => "porto.list",
            Self::PortoInspect => "porto.inspect",
            Self::SshServerConfig => "ssh.server_config",
            Self::SshAuthorizedKeys => "ssh.authorized_keys",
            Self::FilesystemPrivilegeSurfaces => "filesystem.privilege_surfaces",
            Self::FilesystemUnixSockets => "filesystem.unix_sockets",
            Self::FileStat => "file.stat",
            Self::FileRead => "file.read",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|capability| capability.id() == id)
    }

    pub(crate) const fn spec(self) -> CapabilitySpec {
        match self {
            Self::HostInfo => capability(
                &["host", "info"],
                "Operating-system and host identity facts",
                RequestSpec::Empty,
            ),
            Self::KernelInfo => capability(
                &["kernel", "info"],
                "Kernel identity and boot command-line facts",
                RequestSpec::Empty,
            ),
            Self::KernelModules => capability(
                &["kernel", "modules"],
                "Loaded kernel module inventory",
                items(8192, "8192", 32_768),
            ),
            Self::KernelSysctls => capability(
                &["kernel", "sysctls"],
                "Pentest-relevant kernel and network sysctls",
                RequestSpec::Empty,
            ),
            Self::SecurityPosture => capability(
                &["security", "posture"],
                "LSM, boot, platform, and CPU security posture",
                items(1024, "1024", 4096),
            ),
            Self::ProcessList => capability(
                &["process", "list"],
                "Process summaries with identity and executable paths",
                items(4096, "4096", 32_768),
            ),
            Self::NetworkInterfaces => capability(
                &["network", "interfaces"],
                "Network interface metadata",
                items(1024, "1024", 4096),
            ),
            Self::NetworkAddresses => capability(
                &["network", "addresses"],
                "IPv4 and IPv6 interface addresses",
                items(1024, "1024", 4096),
            ),
            Self::NetworkResolvers => capability(
                &["network", "resolvers"],
                "Static resolver directives",
                RequestSpec::Empty,
            ),
            Self::NetworkRoutes => capability(
                &["network", "routes"],
                "IPv4 and IPv6 route inventory",
                items(16_384, "16384", 65_536),
            ),
            Self::NetworkNeighbors => capability(
                &["network", "neighbors"],
                "IPv4 neighbor cache inventory",
                items(8192, "8192", 32_768),
            ),
            Self::NetworkSockets => capability(
                &["network", "sockets"],
                "Internet and Unix socket inventory with bounded ownership",
                items(32_768, "32768", 100_000),
            ),
            Self::NetworkListeners => capability(
                &["network", "listeners"],
                "Listening Internet and Unix sockets with bounded ownership",
                items(16_384, "16384", 65_536),
            ),
            Self::NetworkFirewall => capability(
                &["network", "firewall"],
                "Runtime table names and static firewall rule evidence",
                items(10_000, "10000", 50_000),
            ),
            Self::MountList => capability(
                &["mount", "list"],
                "Current mount namespace inventory",
                items(4096, "4096", 16_384),
            ),
            Self::CgroupInspect => capability(
                &["cgroup", "inspect"],
                "Current process cgroup memberships and limits",
                RequestSpec::Empty,
            ),
            Self::UserList => capability(
                &["user", "list"],
                "Local passwd user inventory",
                items(10_000, "10000", 50_000),
            ),
            Self::GroupList => capability(
                &["group", "list"],
                "Local group inventory",
                items(10_000, "10000", 50_000),
            ),
            Self::AuthPosture => capability(
                &["auth", "posture"],
                "Local password state and authentication policy directives",
                items(10_000, "10000", 50_000),
            ),
            Self::SudoRules => capability(
                &["sudo", "rules"],
                "Static sudo policy directives",
                items(10_000, "10000", 50_000),
            ),
            Self::PackageList => capability(
                &["package", "list"],
                "Installed packages from native package databases",
                items(50_000, "50000", 200_000),
            ),
            Self::ServiceList => capability(
                &["service", "list"],
                "Static systemd unit and SysV service inventory",
                items(20_000, "20000", 100_000),
            ),
            Self::ScheduleList => capability(
                &["schedule", "list"],
                "Static cron and systemd timer inventory",
                items(10_000, "10000", 100_000),
            ),
            Self::SystemctlList => capability(
                &["systemctl", "list"],
                "Runtime systemd unit state from system and user managers",
                RequestSpec::SystemctlList(item_limit(20_000, "20000", 100_000)),
            ),
            Self::SystemctlInspect => capability(
                &["systemctl", "inspect"],
                "Detailed runtime state for one systemd unit",
                RequestSpec::SystemctlInspect,
            ),
            Self::DbusList => capability(
                &["dbus", "list"],
                "D-Bus names, activation state, owners, and peer credentials",
                RequestSpec::DbusList(item_limit(20_000, "20000", 100_000)),
            ),
            Self::DbusInspect => capability(
                &["dbus", "inspect"],
                "Detailed ownership and credentials for one D-Bus name",
                RequestSpec::DbusInspect,
            ),
            Self::ContainerList => capability(
                &["container", "list"],
                "Runtime-agnostic running-container discovery from process cgroups",
                items(10_000, "10000", 50_000),
            ),
            Self::ContainerInspect => capability(
                &["container", "inspect"],
                "Named runtime-agnostic container context from procfs",
                RequestSpec::ContainerInspect,
            ),
            Self::PortoList => capability(
                &["portoctl", "list"],
                "Porto container inventory with complete property evidence",
                RequestSpec::PortoList(item_limit(10_000, "10000", 50_000)),
            ),
            Self::PortoInspect => capability(
                &["portoctl", "inspect"],
                "Named Porto snapshot with complete property evidence",
                RequestSpec::PortoInspect,
            ),
            Self::SshServerConfig => capability(
                &["ssh", "server-config"],
                "Static OpenSSH server directives",
                items(10_000, "10000", 50_000),
            ),
            Self::SshAuthorizedKeys => capability(
                &["ssh", "authorized-keys"],
                "Authorized-key fingerprints and options",
                items(10_000, "10000", 50_000),
            ),
            Self::FilesystemPrivilegeSurfaces => capability(
                &["filesystem", "privilege-surfaces"],
                "Explicit bounded scan for privilege-relevant filesystem metadata",
                filesystem_scan(100_000, "100000", 1_000_000),
            ),
            Self::FilesystemUnixSockets => capability(
                &["filesystem", "unix-sockets"],
                "Explicit bounded scan for filesystem Unix sockets",
                filesystem_scan(100_000, "100000", 1_000_000),
            ),
            Self::FileStat => capability(
                &["file", "stat"],
                "Metadata for one explicitly selected path",
                RequestSpec::FileStat,
            ),
            Self::FileRead => capability(
                &["file", "read"],
                "Chunked content from one explicitly selected regular file",
                RequestSpec::FileRead,
            ),
        }
    }

    pub const fn command(self) -> &'static [&'static str] {
        self.spec().command
    }
    pub const fn description(self) -> &'static str {
        self.spec().description
    }

    pub(crate) const fn request_spec(self) -> RequestSpec {
        self.spec().request
    }

    pub const fn data_kind(self) -> &'static str {
        match self {
            Self::HostInfo => "host",
            Self::KernelInfo => "kernel",
            Self::KernelModules => "kernel_module",
            Self::KernelSysctls => "kernel_sysctl",
            Self::SecurityPosture => "security_control",
            Self::ProcessList => "process",
            Self::NetworkInterfaces => "network_interface",
            Self::NetworkAddresses => "network_address",
            Self::NetworkResolvers => "resolver_directive",
            Self::NetworkRoutes => "network_route",
            Self::NetworkNeighbors => "network_neighbor",
            Self::NetworkSockets | Self::NetworkListeners => "network_socket",
            Self::NetworkFirewall => "firewall_evidence",
            Self::MountList => "mount",
            Self::CgroupInspect => "cgroup_membership",
            Self::UserList => "user",
            Self::GroupList => "group",
            Self::AuthPosture => "auth_control",
            Self::SudoRules => "sudo_directive",
            Self::PackageList => "package",
            Self::ServiceList => "service",
            Self::ScheduleList => "schedule",
            Self::SystemctlList | Self::SystemctlInspect => "systemd_unit_runtime",
            Self::DbusList | Self::DbusInspect => "dbus_record",
            Self::ContainerList => "container",
            Self::ContainerInspect => "container_context",
            Self::PortoList => "porto_container",
            Self::PortoInspect => "porto_container_context",
            Self::SshServerConfig => "ssh_server_directive",
            Self::SshAuthorizedKeys => "ssh_authorized_key",
            Self::FilesystemPrivilegeSurfaces => "filesystem_privilege_surface",
            Self::FilesystemUnixSockets => "filesystem_unix_socket",
            Self::FileStat => "file_metadata",
            Self::FileRead => "file_chunk",
        }
    }

    pub const fn safety_class(self) -> &'static str {
        match self {
            Self::FilesystemPrivilegeSurfaces
            | Self::FilesystemUnixSockets
            | Self::FileStat
            | Self::FileRead => "passive_targeted",
            _ => "passive_native",
        }
    }

    pub fn data_kinds(self) -> Vec<&'static str> {
        match self {
            Self::PortoList => vec![
                "porto_container",
                "porto_property_catalog",
                "porto_property",
            ],
            Self::PortoInspect => vec![
                "porto_container_context",
                "porto_property_catalog",
                "porto_property",
            ],
            _ => vec![self.data_kind()],
        }
    }

    pub fn baseline_position(self) -> Option<usize> {
        Self::BASELINE
            .iter()
            .position(|capability| *capability == self)
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
        let unix_socket_connections = matches!(
            self,
            Self::SystemctlList
                | Self::SystemctlInspect
                | Self::DbusList
                | Self::DbusInspect
                | Self::PortoList
                | Self::PortoInspect
        );
        let scope = match self {
            Self::FilesystemPrivilegeSurfaces | Self::FilesystemUnixSockets => "explicit_tree",
            Self::FileStat | Self::FileRead => "explicit_path",
            _ => "visible_namespace",
        };
        json!({
            "read_only":true,
            "scope":scope,
            "executes_host_programs":false,
            "opens_inet_connections":false,
            "may_open_unix_socket":unix_socket_connections,
        })
    }

    pub(crate) const fn positional(self) -> Option<PositionalSpec> {
        match self.request_spec() {
            RequestSpec::ContainerInspect => Some(CONTAINER_NAME),
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
                        default: porto_api::DEFAULT_SOCKET_PATH,
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
            Self::ItemLimit { max_items } => json!({"max_items": max_items}),
            Self::SocketList {
                max_items,
                selection,
            } => json!({
                "max_items":max_items,
                "socket_selection":selection.id(),
            }),
            Self::ContainerInspect { name } => json!({"name":name}),
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
    pub output: PathBuf,
    pub receipt: PathBuf,
    pub stderr: Option<PathBuf>,
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
    if arguments.is_empty()
        || arguments
            .iter()
            .any(|value| matches!(value.to_str(), Some("-h" | "--help")))
    {
        return Ok(Action::Help(
            "Usage: vzik capture <COLLECTION COMMAND> --output PATH --receipt PATH [--stderr PATH]\n\
             Output options may appear before or after the collection command arguments.\n\
             The command validates the complete stream, seals and verifies the receipt, then writes a JSON coverage summary.\n\
             Files are written directly; interruption can leave capture or stderr without a receipt.\n\
             Examples:\n\
               vzik capture collect --output baseline.jsonl --receipt baseline.receipt.json\n\
               vzik capture filesystem unix-sockets /run --output sockets.jsonl --receipt sockets.receipt.json\n"
                .to_string(),
        ));
    }

    let mut output = None;
    let mut receipt = None;
    let mut stderr = None;
    let mut nested = Vec::with_capacity(arguments.len());
    let mut index = 0;
    while index < arguments.len() {
        let destination = match arguments[index].to_str() {
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

    let output = output.ok_or_else(|| "capture requires --output PATH".to_string())?;
    let receipt = receipt.ok_or_else(|| "capture requires --receipt PATH".to_string())?;
    let action = parse_core(program, nested)?;
    let Action::Execute(execution) = action else {
        return Err("capture requires a collection command".into());
    };

    Ok(Action::Capture(CaptureExecution {
        execution,
        output,
        receipt,
        stderr,
    }))
}

fn parse_collect(matches: &ArgMatches) -> Result<Action, String> {
    Ok(Action::Execute(Execution {
        invocation_kind: "profile",
        command_id: "baseline-v3",
        invocations: CapabilityId::BASELINE
            .into_iter()
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
    let (leaf, matches) = matches
        .subcommand()
        .ok_or_else(|| format!("a {group} command is required"))?;
    let capability = CapabilityId::ALL
        .into_iter()
        .find(|capability| capability.command() == [group, leaf])
        .ok_or_else(|| format!("unknown capability command: {group} {leaf}"))?;

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
            socket: PathBuf::from(porto_api::DEFAULT_SOCKET_PATH),
            show_sensitive: false,
            include_streams: false,
            max_stream_bytes: 0,
        },
        RequestSpec::PortoInspect => Request::PortoInspect {
            name: "self".into(),
            socket: PathBuf::from(porto_api::DEFAULT_SOCKET_PATH),
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
        .about("Bounded agent-first Linux host collector")
        .after_help(
            "Collection commands write protocol-v3 JSONL and never execute host programs or open \
             INET connections. capture writes only its explicit output, stderr, and receipt paths. \
             portoctl, systemctl, and dbus may connect only to configured Unix sockets.\n\n\
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
            "Validate a protocol-v3 JSONL stream",
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

    for capability in CapabilityId::ALL {
        let group = capability.command()[0];
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
    let spec = capability.spec();
    let mut command = Command::new(spec.command[1]).about(spec.description);
    if let Some(positional) = capability.positional() {
        let parser = match positional.kind {
            PositionalKind::Utf8 => ValueParser::new(NonEmptyStringValueParser::new()),
            PositionalKind::LinuxPath => ValueParser::new(OsStringValueParser::new()),
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
        crate::collect::linux_bytes(path.as_os_str().as_bytes())
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
    fn empty_invocation_displays_help() {
        let Action::Help(help) = parse(args(&["vzik"])).expect("parse") else {
            panic!("expected help");
        };
        assert!(help.contains("Usage: vzik"));
        assert!(help.contains("Commands:"));
    }

    #[test]
    fn discovery_and_capture_help_are_directly_actionable() {
        let Action::Help(root_help) = parse(args(&["vzik"])).expect("parse root help") else {
            panic!("expected root help");
        };
        assert!(root_help.contains("vzik capabilities"));
        assert!(root_help.contains("vzik capabilities porto.list"));

        let Action::Help(capture_help) =
            parse(args(&["vzik", "capture", "--help"])).expect("parse capture help")
        else {
            panic!("expected capture help");
        };
        assert!(capture_help.contains("Output options may appear before or after"));
        assert!(capture_help.contains("vzik capture filesystem unix-sockets /run"));

        let Err(error) = parse(args(&["vzik", "find", "Unix", "sockets"])) else {
            panic!("unknown command must fail");
        };
        assert_eq!(error.details()["kind"], "invalid_subcommand");
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
    fn every_capability_has_hierarchical_help() {
        for capability in CapabilityId::ALL {
            let mut argv = vec![OsString::from("vzik")];
            argv.extend(capability.command().iter().map(OsString::from));
            argv.push(OsString::from("--help"));
            let Action::Help(help) = parse(argv).expect("parse capability help") else {
                panic!("expected help for {}", capability.id());
            };
            assert!(
                help.contains(capability.spec().description),
                "missing description for {}",
                capability.id()
            );
        }

        let Action::Help(group_help) =
            parse(args(&["vzik", "portoctl", "--help"])).expect("parse group help")
        else {
            panic!("expected portoctl help");
        };
        assert!(group_help.contains("list"));
        assert!(group_help.contains("inspect"));

        let Action::Help(leaf_help) =
            parse(args(&["vzik", "portoctl", "inspect", "--help"])).expect("parse leaf help")
        else {
            panic!("expected portoctl inspect help");
        };
        assert!(leaf_help.contains("--socket <PATH>"));
        assert!(leaf_help.contains("/run/portod.socket"));
    }

    #[test]
    fn limits_reject_values_above_hard_bounds() {
        assert!(parse(args(&["vzik", "process", "list", "--max-items", "32768"])).is_ok());
        assert!(parse(args(&["vzik", "process", "list", "--max-items", "32769"])).is_err());
    }

    #[test]
    fn privilege_surface_scan_is_explicit_only() {
        assert!(!CapabilityId::BASELINE.contains(&CapabilityId::FilesystemPrivilegeSurfaces));
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

    #[test]
    fn generic_container_and_collect_commands_reject_porto_options() {
        assert!(
            parse(args(&[
                "vzik",
                "container",
                "inspect",
                "self",
                "--socket",
                "/tmp/portod.socket",
            ]))
            .is_err()
        );
        assert!(
            parse(args(
                &["vzik", "collect", "--socket", "/tmp/portod.socket",]
            ))
            .is_err()
        );
    }
}
