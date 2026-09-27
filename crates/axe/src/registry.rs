use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::sync::OnceLock;

use brush_shell::bundled::{BundledCommand, BundledFn, InProcessFn, ShellExecution};
use serde::{Deserialize, Serialize};

pub(crate) const INDEX_CACHE_ONLY_ENV: &str = "__AXE_STORE_INDEX_CACHE_ONLY";

#[derive(Clone, Copy, Debug)]
pub enum BuildMode {
    CacheOnly,
    RevalidateIfStale,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum StoreMode {
    #[default]
    Auto,
    CacheOnly,
    Off,
}

impl StoreMode {
    pub(crate) fn from_environment() -> Result<Self, String> {
        let Some(value) = std::env::var_os("AXE_STORE_MODE") else {
            return Ok(Self::Auto);
        };
        let value = value
            .to_str()
            .ok_or_else(|| "AXE_STORE_MODE is not valid UTF-8".to_string())?;
        value.parse()
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::CacheOnly => "cache-only",
            Self::Off => "off",
        }
    }

    pub(crate) fn restrict(self, requested: Self) -> Self {
        match (self, requested) {
            (Self::Off, _) | (_, Self::Off) => Self::Off,
            (Self::CacheOnly, _) | (_, Self::CacheOnly) => Self::CacheOnly,
            (Self::Auto, Self::Auto) => Self::Auto,
        }
    }
}

impl std::fmt::Display for StoreMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for StoreMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "cache-only" => Ok(Self::CacheOnly),
            "off" => Ok(Self::Off),
            _ => Err(format!(
                "invalid AXE Store mode {value:?}; expected auto, cache-only, or off"
            )),
        }
    }
}

pub struct Registry {
    pub commands: HashMap<String, BundledCommand>,
    pub visible_names: Vec<String>,
    pub blocking_index_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandSource {
    Bundled,
    Store,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CommandInfo {
    pub name: String,
    pub canonical_name: String,
    pub source: CommandSource,
    pub category: String,
    pub alias_of: Option<String>,
    pub synopsis: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct StoreErrorInfo {
    pub class: &'static str,
    pub stage: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum StoreInfo {
    Disabled,
    Available {
        channel: String,
        index_generation: u64,
    },
    Blocked {
        channel: String,
        index_generation: u64,
        error: StoreErrorInfo,
    },
}

impl StoreInfo {
    fn blocking_error(&self) -> Option<String> {
        match self {
            Self::Blocked { error, .. } => Some(format!(
                "store {} error ({}: {})",
                error.class, error.stage, error.message
            )),
            _ => None,
        }
    }
}

pub(crate) fn bridge_command_names(store_mode: StoreMode) -> Vec<String> {
    command_info()
        .iter()
        .filter(|command| {
            store_mode != StoreMode::Off || matches!(command.source, CommandSource::Bundled)
        })
        .map(|command| command.name.clone())
        .collect()
}

#[derive(Clone)]
struct RegisteredCommand {
    entry: BundledFn,
    shell_execution: ShellExecution,
    info: CommandInfo,
}

#[derive(Default)]
pub(crate) struct RegistryBuilder {
    commands: HashMap<String, RegisteredCommand>,
}

impl RegistryBuilder {
    pub(crate) fn insert_bundled(
        &mut self,
        name: impl Into<String>,
        category: &str,
        synopsis: Option<&str>,
        entry: BundledFn,
    ) {
        let name = name.into();
        let synopsis = synopsis.unwrap_or_else(|| bundled_synopsis(&name));
        let info = CommandInfo {
            canonical_name: name.clone(),
            name: name.clone(),
            source: CommandSource::Bundled,
            category: category.to_owned(),
            alias_of: None,
            synopsis: synopsis.to_owned(),
        };
        self.commands.insert(
            name,
            RegisteredCommand {
                entry,
                shell_execution: ShellExecution::Spawn,
                info,
            },
        );
    }

    pub(crate) fn insert_builtin_bundled(
        &mut self,
        name: impl Into<String>,
        category: &str,
        synopsis: &str,
        entry: BundledFn,
        callback: InProcessFn,
    ) {
        let name = name.into();
        let info = CommandInfo {
            canonical_name: name.clone(),
            name: name.clone(),
            source: CommandSource::Bundled,
            category: category.to_owned(),
            alias_of: None,
            synopsis: synopsis.to_owned(),
        };
        self.commands.insert(
            name,
            RegisteredCommand {
                entry,
                shell_execution: ShellExecution::Builtin(callback),
                info,
            },
        );
    }

    pub(crate) fn insert_bundled_if_vacant(
        &mut self,
        name: &str,
        category: &str,
        synopsis: Option<&str>,
        entry: BundledFn,
    ) {
        if !self.commands.contains_key(name) {
            self.insert_bundled(name, category, synopsis, entry);
        }
    }

    fn extend_bundled(
        &mut self,
        commands: impl IntoIterator<Item = (String, BundledFn)>,
        category: &str,
    ) {
        for (name, entry) in commands {
            self.insert_bundled(name, category, None, entry);
        }
    }

    pub(crate) fn insert_store_if_vacant(
        &mut self,
        name: &str,
        canonical_name: &str,
        category: &str,
        synopsis: Option<&str>,
        entry: BundledFn,
    ) {
        if self.commands.contains_key(name) {
            return;
        }

        let synopsis = synopsis
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Run the {name} AXE Store tool"));
        let info = CommandInfo {
            name: name.to_owned(),
            canonical_name: canonical_name.to_owned(),
            source: CommandSource::Store,
            category: category.to_owned(),
            alias_of: (name != canonical_name).then(|| canonical_name.to_owned()),
            synopsis,
        };
        self.commands.insert(
            name.to_owned(),
            RegisteredCommand {
                entry,
                shell_execution: ShellExecution::Spawn,
                info,
            },
        );
    }

    fn contains_key(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    fn local_command_requires_store(&self, name: &str) -> bool {
        self.commands.get(name).is_some_and(|command| {
            matches!(command.info.canonical_name.as_str(), "commands" | "sshd")
        })
    }

    fn local_command_uses_store_cache_only(&self, name: &str) -> bool {
        self.commands
            .get(name)
            .is_some_and(|command| command.info.canonical_name == "sshd")
    }

    fn entry(&self, name: &str) -> Option<BundledFn> {
        self.commands.get(name).map(|command| command.entry)
    }

    fn insert_alias(&mut self, name: String, target: &str, synopsis: Option<&str>) {
        let target = self
            .commands
            .get(target)
            .expect("alias target was checked before insertion");
        let info = CommandInfo {
            name: name.clone(),
            canonical_name: target.info.canonical_name.clone(),
            source: target.info.source,
            category: target.info.category.clone(),
            alias_of: Some(target.info.name.clone()),
            synopsis: synopsis.unwrap_or(&target.info.synopsis).to_owned(),
        };
        self.commands.insert(
            name,
            RegisteredCommand {
                entry: alias_entry as BundledFn,
                shell_execution: target.shell_execution,
                info,
            },
        );
    }

    fn finish(self) -> (HashMap<String, BundledCommand>, Vec<CommandInfo>) {
        let mut entries = HashMap::with_capacity(self.commands.len());
        let mut info = Vec::with_capacity(self.commands.len());
        for (name, command) in self.commands {
            entries.insert(
                name,
                BundledCommand {
                    entry: command.entry,
                    shell_execution: command.shell_execution,
                },
            );
            info.push(command.info);
        }
        info.sort_unstable_by(|left, right| left.name.cmp(&right.name));

        (entries, info)
    }
}

fn bundled_synopsis(name: &str) -> &'static str {
    match name {
        "[" | "test" => "Evaluate expressions and file predicates",
        "arch" => "Print the machine architecture",
        "arp" => "Inspect and modify the ARP cache",
        "awk" => "Process text with pattern-action programs",
        "b2sum" => "Compute and verify BLAKE2 checksums",
        "base32" => "Encode or decode Base32 data",
        "base64" => "Encode or decode Base64 data",
        "basename" => "Strip directory and suffix components from paths",
        "basenc" => "Encode or decode data with printable base encodings",
        "blkid" => "Inspect block device attributes and filesystem signatures",
        "blockdev" => "Inspect and configure block device parameters",
        "bunzip2" | "bzcat" | "bzip2" => "Compress or decompress bzip2 data",
        "cat" => "Concatenate files and write them to standard output",
        "chgrp" => "Change file group ownership",
        "chmod" => "Change file permission modes",
        "chown" => "Change file owner and group",
        "chroot" => "Run a command with a different root directory",
        "cksum" => "Compute and verify file checksums",
        "cmp" => "Compare two files byte by byte",
        "comm" => "Compare sorted files line by line",
        "cp" => "Copy files and directories",
        "csplit" => "Split files at context-defined boundaries",
        "cut" => "Select fields or byte ranges from input lines",
        "date" => "Display or set the system date and time",
        "dd" => "Copy and convert raw data streams",
        "df" => "Report filesystem space usage",
        "diff" => "Compare files line by line",
        "diff3" => "Compare three files line by line",
        "dir" | "ls" | "vdir" => "List directory contents",
        "dircolors" => "Generate color settings for directory listings",
        "dirname" => "Strip the final component from paths",
        "dmesg" => "Inspect the kernel message buffer",
        "du" => "Estimate file and directory space usage",
        "echo" => "Write arguments to standard output",
        "env" => "Inspect or modify the environment for a command",
        "expand" => "Convert tabs to spaces",
        "expr" => "Evaluate arithmetic and string expressions",
        "factor" => "Print prime factors of integers",
        "false" => "Return an unsuccessful exit status",
        "file" => "Identify file formats from their contents",
        "find" => "Search directory trees and apply predicates",
        "fmt" => "Reformat text paragraphs",
        "fold" => "Wrap input lines to a specified width",
        "free" => "Report system memory usage",
        "goblin" => "Inspect ELF, PE, Mach-O, and archive binaries",
        "grep" => "Search text for matching patterns",
        "groups" => "Print group memberships",
        "gzip" => "Compress or decompress gzip data",
        "head" => "Print the beginning of files",
        "hexdump" => "Display file contents in hexadecimal and other formats",
        "host" => "Resolve DNS names and addresses",
        "hostid" => "Print the numeric host identifier",
        "http" => "Make bounded HTTP requests with structured output",
        "hostname" => "Display or set the system hostname",
        "hugetop" => "Report huge page usage by process",
        "id" => "Print user and group identifiers",
        "ifconfig" => "Inspect and configure network interfaces",
        "inotifywait" => "Wait for filesystem events",
        "inotifywatch" => "Collect filesystem event statistics",
        "install" => "Copy files while setting attributes",
        "iostat" => "Report CPU and device I/O statistics",
        "ip" => "Inspect and configure Linux networking",
        "ipaddr" => "Inspect and configure network addresses",
        "ipcalc" => "Calculate IPv4 and IPv6 network parameters",
        "ipcs" => "Report System V IPC resources",
        "iplink" => "Inspect and configure network links",
        "ipneigh" => "Inspect and configure neighbor tables",
        "iproute" => "Inspect and configure network routes",
        "iprule" => "Inspect and configure routing policy rules",
        "join" => "Join lines from files on a common field",
        "jq" => "Process and transform JSON data",
        "kill" => "Send signals to processes",
        "last" => "Show recent login sessions",
        "link" => "Create a hard link to a file",
        "ln" => "Create hard or symbolic links",
        "logname" => "Print the current login name",
        "lsof" => "List open files and their owning processes",
        "lsmod" => "List loaded Linux kernel modules",
        "lspci" => "List PCI devices",
        "lsscsi" => "List SCSI devices",
        "lsusb" => "List USB devices",
        "md5sum" => "Compute and verify MD5 checksums",
        "mkdir" => "Create directories",
        "mkfifo" => "Create named pipes",
        "mknod" => "Create block, character, or FIFO special files",
        "mktemp" => "Create temporary files or directories",
        "modinfo" => "Display Linux kernel module information",
        "more" => "Page through text one screen at a time",
        "mount" => "Inspect mounts or mount filesystems read-only",
        "mountpoint" => "Check whether a path is a mount point",
        "mv" => "Move or rename files and directories",
        "nice" => "Run a command with adjusted scheduling priority",
        "nl" => "Number lines from input files",
        "nohup" => "Run a command immune to hangups",
        "nproc" => "Print the number of available processing units",
        "nslookup" => "Query DNS name servers",
        "numfmt" => "Convert numbers to and from human-readable units",
        "od" => "Dump file contents in octal and other formats",
        "paste" => "Merge corresponding lines from files",
        "pathchk" => "Check pathnames for portability and validity",
        "pgrep" => "Find process IDs by name and attributes",
        "pidof" => "Find process IDs by executable name",
        "pidwait" => "Wait for selected processes to exit",
        "ping" | "ping6" => "Test network reachability with ICMP echo requests",
        "pinky" => "Print concise user login information",
        "pkill" => "Send signals to processes selected by name",
        "pmap" => "Report process memory maps",
        "pr" => "Paginate or columnate files for printing",
        "printenv" => "Print environment variables",
        "printf" => "Format and write data",
        "ps" => "Report process status",
        "ptx" => "Produce a permuted index of file contents",
        "pwd" => "Print the current working directory",
        "pwdx" => "Report process working directories",
        "readlink" => "Print resolved symbolic link targets",
        "realpath" => "Resolve paths to canonical absolute form",
        "rm" => "Remove files and directories",
        "rmdir" => "Remove empty directories",
        "sed" => "Transform text streams with editing expressions",
        "seq" => "Print numeric sequences",
        "sha1sum" => "Compute and verify SHA-1 checksums",
        "sha224sum" => "Compute and verify SHA-224 checksums",
        "sha256sum" => "Compute and verify SHA-256 checksums",
        "sha384sum" => "Compute and verify SHA-384 checksums",
        "sha512sum" => "Compute and verify SHA-512 checksums",
        "shred" => "Overwrite files to obscure their contents",
        "shuf" => "Randomly permute input lines",
        "skill" => "Send signals to processes selected interactively",
        "slabtop" => "Display Linux kernel slab cache usage",
        "sleep" => "Delay execution for a duration",
        "snice" => "Adjust scheduling priority for selected processes",
        "sort" => "Sort lines of text",
        "split" => "Split files into pieces",
        "sshd" => "Serve certificate-authenticated SSH and SFTP sessions",
        "stat" => "Display file or filesystem status",
        "strings" => "Extract printable strings from files",
        "stty" => "Inspect and change terminal settings",
        "sum" => "Compute file checksums and block counts",
        "sync" => "Flush buffered filesystem data",
        "sysctl" => "Inspect and modify kernel parameters",
        "tac" => "Concatenate files in reverse line order",
        "tail" => "Print the end of files",
        "tar" => "Create, inspect, and extract tar archives",
        "tee" => "Copy standard input to files and standard output",
        "timeout" => "Run a command with a time limit",
        "tload" => "Display a terminal graph of system load",
        "top" => "Display live process and system statistics",
        "touch" => "Update timestamps or create empty files",
        "tr" => "Translate or delete characters",
        "traceroute" | "traceroute6" => "Trace the network path to a host",
        "tree" => "Display directory trees",
        "true" => "Return a successful exit status",
        "truncate" => "Shrink or extend files to a specified size",
        "tsort" => "Topologically sort dependency pairs",
        "tty" => "Print the terminal connected to standard input",
        "uname" => "Print system information",
        "unexpand" => "Convert spaces to tabs",
        "uniq" => "Report or remove adjacent duplicate lines",
        "unlink" => "Remove one filesystem name",
        "unxz" | "xz" | "xzcat" => "Compress or decompress xz data",
        "uptime" => "Report system uptime and load averages",
        "users" => "Print logged-in user names",
        "vmstat" => "Report virtual memory statistics",
        "w" => "Show logged-in users and their processes",
        "watch" => "Run a command repeatedly and display its output",
        "wc" => "Count lines, words, and bytes",
        "which" => "Locate executables resolved through PATH",
        "who" => "Show logged-in users",
        "whoami" => "Print the effective user name",
        "xargs" => "Build and run commands from standard input",
        "yes" => "Repeat a string until interrupted",
        _ => panic!("bundled command '{name}' is missing a synopsis"),
    }
}

#[derive(Debug, Deserialize)]
struct Alias {
    argv: Vec<String>,
    #[serde(default)]
    help: Vec<String>,
    synopsis: Option<String>,
}

struct RuntimeAlias {
    entry: BundledFn,
    argv: Vec<OsString>,
    help: Vec<String>,
}

static ALIASES: OnceLock<HashMap<String, RuntimeAlias>> = OnceLock::new();
static COMMAND_INFO: OnceLock<Vec<CommandInfo>> = OnceLock::new();

pub(crate) fn command_info() -> &'static [CommandInfo] {
    COMMAND_INFO
        .get()
        .expect("command information is initialized with the registry")
}

pub fn build(
    mode: BuildMode,
    preferred_local: Option<&str>,
    store_mode: StoreMode,
) -> Result<Registry, String> {
    {
        crate::relay_config::load()?;
        crate::sshd_config::load()?;
    }

    let mut commands = RegistryBuilder::default();
    commands.insert_bundled(
        "commands",
        "control",
        Some("List commands available through AXE"),
        crate::applets::commands as BundledFn,
    );
    commands.insert_builtin_bundled(
        "doctor",
        "control",
        "Diagnose the current AXE runtime, shell, isolation, and restrictions",
        crate::applets::doctor as BundledFn,
        crate::applets::doctor_builtin,
    );
    commands.extend_bundled(brush_coreutils_builtins::bundled_commands(), "coreutils");
    commands.extend_bundled(
        crate::applets::admin_coreutils::commands(),
        "administration",
    );
    commands.insert_bundled("awk", "text", None, crate::applets::awk as BundledFn);
    commands.insert_bundled("grep", "text", None, crate::applets::grep as BundledFn);
    commands.insert_bundled("sed", "text", None, crate::applets::sed as BundledFn);
    {
        commands.insert_bundled(
            "find",
            "filesystem",
            None,
            crate::applets::find as BundledFn,
        );
        commands.insert_bundled("xargs", "process", None, crate::applets::xargs as BundledFn);
    }
    {
        commands.insert_bundled("diff", "text", None, crate::applets::diff as BundledFn);
        commands.insert_bundled("cmp", "text", None, crate::applets::cmp as BundledFn);
        commands.insert_bundled("diff3", "text", None, crate::applets::diff3 as BundledFn);
    }
    commands.insert_bundled("file", "binary", None, crate::applets::file as BundledFn);
    commands.insert_bundled(
        "goblin",
        "binary",
        None,
        crate::applets::goblin as BundledFn,
    );
    commands.insert_bundled("jq", "data", None, crate::applets::jq as BundledFn);
    commands.insert_bundled(
        "strings",
        "binary",
        None,
        crate::applets::strings as BundledFn,
    );
    commands.insert_bundled("tar", "archive", None, crate::applets::tar as BundledFn);
    commands.insert_bundled(
        "gzip",
        "compression",
        None,
        crate::applets::gzip as BundledFn,
    );
    {
        for name in ["bzip2", "bunzip2", "bzcat"] {
            commands.insert_bundled(
                name,
                "compression",
                None,
                crate::applets::bzip2 as BundledFn,
            );
        }
        for name in ["xz", "unxz", "xzcat"] {
            commands.insert_bundled(name, "compression", None, crate::applets::xz as BundledFn);
        }
    }
    commands.insert_bundled("http", "network", None, crate::applets::http as BundledFn);
    #[cfg(target_os = "linux")]
    {
        commands.extend_bundled(crate::applets::linux_network::commands(), "network");
        commands.insert_bundled(
            "nslookup",
            "network",
            None,
            crate::applets::nslookup as BundledFn,
        );
        commands.insert_bundled("host", "network", None, crate::applets::host as BundledFn);
        commands.insert_bundled("ping", "network", None, crate::applets::ping as BundledFn);
        commands.insert_bundled("ping6", "network", None, crate::applets::ping6 as BundledFn);
        commands.insert_bundled(
            "traceroute",
            "network",
            None,
            crate::applets::traceroute as BundledFn,
        );
        commands.insert_bundled(
            "traceroute6",
            "network",
            None,
            crate::applets::traceroute6 as BundledFn,
        );
    }
    #[cfg(target_os = "linux")]
    commands.extend_bundled(crate::applets::linux_storage::commands(), "storage");
    #[cfg(target_os = "linux")]
    commands.extend_bundled(crate::applets::linux_inspect::commands(), "inspection");
    #[cfg(target_os = "linux")]
    {
        commands.insert_bundled(
            "inotifywait",
            "filesystem",
            None,
            crate::applets::inotifywait as BundledFn,
        );
        commands.insert_bundled(
            "inotifywatch",
            "filesystem",
            None,
            crate::applets::inotifywatch as BundledFn,
        );
    }
    #[cfg(target_os = "linux")]
    commands.extend_bundled(
        [
            ("free".into(), crate::applets::free as BundledFn),
            ("hugetop".into(), crate::applets::hugetop as BundledFn),
            ("pgrep".into(), crate::applets::pgrep as BundledFn),
            ("pidof".into(), crate::applets::pidof as BundledFn),
            ("pidwait".into(), crate::applets::pidwait as BundledFn),
            ("pkill".into(), crate::applets::pkill as BundledFn),
            ("pmap".into(), crate::applets::pmap as BundledFn),
            ("ps".into(), crate::applets::ps as BundledFn),
            ("pwdx".into(), crate::applets::pwdx as BundledFn),
            ("skill".into(), crate::applets::skill as BundledFn),
            ("slabtop".into(), crate::applets::slabtop as BundledFn),
            ("snice".into(), crate::applets::snice as BundledFn),
            ("sysctl".into(), crate::applets::sysctl as BundledFn),
            ("tload".into(), crate::applets::tload as BundledFn),
            ("top".into(), crate::applets::top as BundledFn),
            ("vmstat".into(), crate::applets::vmstat as BundledFn),
            ("w".into(), crate::applets::w as BundledFn),
            ("watch".into(), crate::applets::watch as BundledFn),
        ],
        "process",
    );
    #[cfg(target_os = "linux")]
    commands.extend_bundled(crate::applets::util_linux::commands(), "system");
    commands.insert_bundled(
        "tree",
        "filesystem",
        None,
        crate::applets::tree as BundledFn,
    );
    commands.insert_bundled(
        "which",
        "environment",
        None,
        crate::applets::which as BundledFn,
    );
    commands.insert_bundled("sshd", "service", None, crate::applets::sshd as BundledFn);
    commands.insert_bundled(
        "vzik",
        "host-inspection",
        Some("Collect bounded host and container evidence as JSONL"),
        crate::applets::vzik as BundledFn,
    );

    crate::ondemand::register_controls(&mut commands);

    let config: HashMap<String, Alias> =
        serde_json::from_str(include_str!("../../../config/aliases.json"))
            .map_err(|error| format!("invalid config/aliases.json: {error}"))?;
    let mut aliases = HashMap::with_capacity(config.len());
    for (name, alias) in config {
        if commands.contains_key(&name) {
            return Err(format!("alias '{name}' duplicates an applet"));
        }
        let Some(target) = alias.argv.first().cloned() else {
            return Err(format!("alias '{name}' has empty argv"));
        };
        let Some(entry) = commands.entry(&target) else {
            continue;
        };
        aliases.insert(
            name.clone(),
            RuntimeAlias {
                entry,
                argv: alias.argv.into_iter().map(OsString::from).collect(),
                help: alias.help,
            },
        );
        commands.insert_alias(name, &target, alias.synopsis.as_deref());
    }
    ALIASES
        .set(aliases)
        .map_err(|_| "applet registry was built more than once".to_string())?;

    let store_info = if store_mode == StoreMode::Off {
        StoreInfo::Disabled
    } else {
        let local = preferred_local.filter(|name| commands.contains_key(name));
        if local.is_some_and(|name| !commands.local_command_requires_store(name)) {
            StoreInfo::Disabled
        } else {
            let mode =
                if local.is_some_and(|name| commands.local_command_uses_store_cache_only(name)) {
                    BuildMode::CacheOnly
                } else {
                    mode
                };
            crate::ondemand::register(&mut commands, mode)?
        }
    };
    let blocking_index_error = store_info.blocking_error();
    let (commands, command_info) = commands.finish();
    let visible_names = command_info
        .iter()
        .map(|command| command.name.clone())
        .collect();
    COMMAND_INFO
        .set(command_info)
        .map_err(|_| "command information was built more than once".to_string())?;

    Ok(Registry {
        commands,
        visible_names,
        blocking_index_error,
    })
}

pub fn invoke(
    commands: &HashMap<String, BundledCommand>,
    name: &str,
    args: impl IntoIterator<Item = OsString>,
) -> i32 {
    let Some(command) = commands.get(name) else {
        eprintln!("axe: unknown applet: {name}");
        return 127;
    };
    let mut argv = Vec::new();
    argv.push(OsString::from(name));
    argv.extend(args);
    (command.entry)(argv)
}

fn alias_entry(args: Vec<OsString>) -> i32 {
    let Some(name) = args.first().and_then(|value| value.to_str()) else {
        eprintln!("axe: alias name is not valid UTF-8");
        return 127;
    };
    let Some(alias) = ALIASES.get().and_then(|aliases| aliases.get(name)) else {
        eprintln!("axe: unknown alias: {name}");
        return 127;
    };
    if !alias.help.is_empty()
        && args.len() == 2
        && (args[1] == OsStr::new("--help") || args[1] == OsStr::new("-h"))
    {
        for line in &alias.help {
            println!("{line}");
        }
        return 0;
    }
    let mut argv = Vec::with_capacity(alias.argv.len() + args.len().saturating_sub(1));
    argv.extend(alias.argv.iter().cloned());
    argv.extend(args.into_iter().skip(1));
    (alias.entry)(argv)
}
