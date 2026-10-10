use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::sync::{Arc, LazyLock};

use axe_store_client::{
    CachePolicy, Client, ClientConfig, DownloadEvent, FailureClass, NetworkPolicy,
    ProgressReporter, StoreError,
};

use crate::registry::{BuildMode, RegistryBuilder, StoreErrorInfo, StoreInfo};

static CLIENT: LazyLock<Result<Client, String>> = LazyLock::new(|| {
    let config = ClientConfig::from_embedded(
        crate::embedded::INPUTS.store_config_json,
        crate::embedded::INPUTS.store_trusted_keys,
        crate::embedded::INPUTS.bootstrap_index_zstd,
        crate::tls_roots::certificates_der().into(),
        Arc::from(crate::tls_roots::pem_bundle()),
    )
    .map_err(|error| error.to_string())?;
    let network_policy = match crate::registry::StoreMode::from_environment()? {
        crate::registry::StoreMode::Auto => NetworkPolicy::Online,
        crate::registry::StoreMode::CacheOnly | crate::registry::StoreMode::Off => {
            NetworkPolicy::Offline
        }
    };
    Client::new(config, network_policy).map_err(|error| error.to_string())
});

fn client() -> Result<&'static Client, String> {
    CLIENT.as_ref().map_err(Clone::clone)
}

pub(crate) fn revalidate_index_if_stale() -> Result<(), String> {
    client()?
        .index(CachePolicy::RevalidateIfStale)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(crate) fn store_is_blocked() -> bool {
    client()
        .ok()
        .and_then(|client| client.index(CachePolicy::CacheOnly).ok())
        .is_some_and(|index| index.blocking_error.is_some())
}

pub fn register_controls(commands: &mut RegistryBuilder) {
    commands.insert_bundled_if_vacant(
        "clean-tools",
        Some("Remove managed AXE Store cache entries"),
        clean as brush_shell::bundled::BundledFn,
    );
    commands.insert_bundled_if_vacant(
        "refresh-tools",
        Some("Refresh and verify AXE Store metadata"),
        refresh as brush_shell::bundled::BundledFn,
    );
}

pub fn register(commands: &mut RegistryBuilder, mode: BuildMode) -> Result<StoreInfo, String> {
    let client = client()?;
    let policy = match mode {
        BuildMode::CacheOnly => CachePolicy::CacheOnly,
        BuildMode::RevalidateIfStale => CachePolicy::RevalidateIfStale,
    };
    let result = client.index(policy).map_err(|error| error.to_string())?;
    let target = axe_artifact_target().map_err(|error| error.to_string())?;
    let channel = client.channel();
    for (name, tool) in &result.index.tools {
        if !tool
            .channels
            .get(channel)
            .is_some_and(|targets| targets.contains(target.as_str()))
        {
            continue;
        }
        let category = tool
            .id
            .split_once('/')
            .map_or("store", |(category, _)| category);
        commands.insert_store_if_vacant(
            name,
            name,
            category,
            tool.synopsis.as_deref(),
            entry as brush_shell::bundled::BundledFn,
        );
        for alias in &tool.aliases {
            commands.insert_store_if_vacant(
                alias,
                name,
                category,
                tool.synopsis.as_deref(),
                entry as brush_shell::bundled::BundledFn,
            );
        }
    }

    let store = match result.blocking_error.as_ref() {
        Some(error) => StoreInfo::Blocked {
            channel: channel.to_owned(),
            index_generation: result.index.generation,
            error: StoreErrorInfo {
                class: failure_class_name(error.class),
                stage: error.stage.to_string(),
                message: error.source.clone(),
            },
        },
        None => StoreInfo::Available {
            channel: channel.to_owned(),
            index_generation: result.index.generation,
        },
    };
    Ok(store)
}

fn failure_class_name(class: FailureClass) -> &'static str {
    match class {
        FailureClass::Transient => "transient",
        FailureClass::Integrity => "integrity",
        FailureClass::Configuration => "configuration",
        FailureClass::Unavailable => "unavailable",
    }
}

fn allows_path_fallback(class: FailureClass) -> bool {
    matches!(class, FailureClass::Transient | FailureClass::Unavailable)
}

fn axe_artifact_target() -> Result<axe_artifact::Target, axe_artifact::Error> {
    axe_artifact::Target::detect()
}

pub fn entry(args: Vec<OsString>) -> i32 {
    let Some(name) = args
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .and_then(OsStr::to_str)
    else {
        eprintln!("axe: invalid AXE Store command name");
        return 127;
    };

    let client = match client() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("axe: {name}: AXE Store configuration error: {error}");
            return 126;
        }
    };
    let resolved = match client.resolve(name, client.channel()) {
        Ok(resolved) => resolved,
        Err(error) => return handle_delivery_error(name, &args[1..], &error),
    };

    if args
        .get(1)
        .is_some_and(|argument| argument == "--axe-tool-info")
    {
        println!(
            "{} {} {} {:?} {} static={}",
            resolved.name,
            resolved.version,
            resolved.target,
            resolved.artifact.kind,
            resolved.artifact.object_sha256,
            resolved.artifact.fully_static
        );
        return 0;
    }

    let mut progress = StderrProgress::new();
    let prepared = match client.prepare(&resolved, &mut progress) {
        Ok(prepared) => prepared,
        Err(error) => return handle_delivery_error(name, &args[1..], &error),
    };
    match prepared.run(name, &args[1..]) {
        Ok(status) => map_status(status),
        Err(error) => handle_delivery_error(name, &args[1..], &error),
    }
}

pub fn clean(args: Vec<OsString>) -> i32 {
    if args.len() == 2 && (args[1] == "--help" || args[1] == "-h") {
        println!(
            "Usage: clean-tools\n\nRemove AXE Store metadata, objects, and unpacked artifacts."
        );
        return 0;
    }
    if args.len() != 1 {
        eprintln!("clean-tools: no arguments expected");
        return 2;
    }
    let client = match client() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("clean-tools: {error}");
            return 1;
        }
    };
    match client.clean() {
        Ok(removed) => {
            for root in &removed {
                println!("{}", root.display());
            }
            println!(
                "clean-tools: cleaned {} cache root{}",
                removed.len(),
                if removed.len() == 1 { "" } else { "s" }
            );
            0
        }
        Err(error) => {
            eprintln!("clean-tools: {error}");
            1
        }
    }
}

pub fn refresh(args: Vec<OsString>) -> i32 {
    if args.len() == 2 && (args[1] == "--help" || args[1] == "-h") {
        println!("Usage: refresh-tools\n\nFetch and verify the current AXE Store Index.");
        return 0;
    }
    if args.len() != 1 {
        eprintln!("refresh-tools: no arguments expected");
        return 2;
    }
    match client().and_then(|client| client.refresh_index().map_err(|error| error.to_string())) {
        Ok(index) => {
            println!(
                "refresh-tools: verified Index generation {}",
                index.generation
            );
            0
        }
        Err(error) => {
            eprintln!("refresh-tools: {error}");
            126
        }
    }
}

fn handle_delivery_error(name: &str, args: &[OsString], error: &StoreError) -> i32 {
    if allows_path_fallback(error.class) {
        run_path_fallback(name, args, error)
    } else {
        hard_error(name, error)
    }
}

fn hard_error(name: &str, error: &StoreError) -> i32 {
    let class = failure_class_name(error.class);
    eprintln!(
        "axe: {name}: store {class} error ({}: {})",
        error.stage, error.source
    );
    126
}

fn run_path_fallback(name: &str, args: &[OsString], store_error: &StoreError) -> i32 {
    let candidates = match which::which_all(name) {
        Ok(candidates) => candidates,
        Err(_) => return hard_error(name, store_error),
    };
    let bridge = std::env::var_os("AXE_APPLET_DIR")
        .filter(|directory| !directory.is_empty())
        .map(std::path::PathBuf::from);
    let executable = crate::executable::current();

    run_path_candidates(
        name,
        args,
        store_error,
        candidates,
        bridge.as_deref(),
        &executable,
    )
}

fn run_path_candidates(
    name: &str,
    args: &[OsString],
    store_error: &StoreError,
    candidates: impl IntoIterator<Item = impl AsRef<Path>>,
    bridge: Option<&Path>,
    executable: &crate::executable::Executable,
) -> i32 {
    for candidate in candidates {
        let candidate = candidate.as_ref();
        if bridge.is_some_and(|bridge| candidate.starts_with(bridge))
            || executable.matches(candidate)
        {
            continue;
        }
        let status = run_path_candidate(candidate, name, args);
        match status {
            Ok(status) => {
                eprintln!(
                    "axe: {name}: store temporarily unavailable ({}: {}); using {}",
                    store_error.stage,
                    store_error.source,
                    candidate.display()
                );
                return map_status(status);
            }
            Err(_) => continue,
        }
    }
    hard_error(name, store_error)
}

fn run_path_candidate(path: &Path, name: &str, args: &[OsString]) -> io::Result<ExitStatus> {
    let mut command = Command::new(path);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.arg0(name);
    }
    command.args(args).status()
}

struct StderrProgress<W> {
    terminal: bool,
    writer: W,
    name: String,
    version: String,
}

impl StderrProgress<io::Stderr> {
    fn new() -> Self {
        Self {
            terminal: io::stderr().is_terminal(),
            writer: io::stderr(),
            name: String::new(),
            version: String::new(),
        }
    }
}

impl<W: Write> ProgressReporter for StderrProgress<W> {
    fn report(&mut self, event: DownloadEvent<'_>) {
        let stderr = &mut self.writer;
        match event {
            DownloadEvent::Started {
                name,
                version,
                target,
                total,
            } => {
                self.name.clear();
                self.name.push_str(name);
                self.version.clear();
                self.version.push_str(version);
                if self.terminal {
                    let _ = write!(
                        stderr,
                        "axe: downloading {name} {version} for {target}: 0/{:.1} MiB (0%)",
                        total as f64 / 1_048_576.0
                    );
                } else {
                    let _ = writeln!(
                        stderr,
                        "axe: downloading {name} {version} for {target} ({total} bytes)"
                    );
                }
                let _ = stderr.flush();
            }
            DownloadEvent::Advanced { downloaded, total } if self.terminal => {
                let percent = downloaded
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(100);
                let _ = write!(
                    stderr,
                    "\raxe: downloading {} {}: {:.1}/{:.1} MiB ({percent}%)\x1b[K",
                    self.name,
                    self.version,
                    downloaded as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0
                );
                let _ = stderr.flush();
            }
            DownloadEvent::Advanced { .. } => {}
            DownloadEvent::Finished { downloaded } => {
                if self.terminal {
                    let _ = writeln!(stderr);
                } else {
                    let _ = writeln!(
                        stderr,
                        "axe: downloaded {} {} ({downloaded} bytes)",
                        self.name, self.version
                    );
                }
            }
            DownloadEvent::Failed => {
                if self.terminal {
                    let _ = writeln!(stderr);
                }
            }
        }
    }
}

fn map_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    }
    #[cfg(not(unix))]
    128
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn removed_store_target_allows_path_fallback() {
        assert!(allows_path_fallback(FailureClass::Unavailable));
        assert!(allows_path_fallback(FailureClass::Transient));
        assert!(!allows_path_fallback(FailureClass::Integrity));
        assert!(!allows_path_fallback(FailureClass::Configuration));
    }

    #[test]
    fn terminal_progress_updates_erase_stale_line_suffix() {
        let mut progress = StderrProgress {
            terminal: true,
            writer: Vec::new(),
            name: String::new(),
            version: String::new(),
        };
        progress.report(DownloadEvent::Started {
            name: "python",
            version: "3.14.7-web.1",
            target: axe_artifact::Target::X86_64Linux,
            total: 12_058_624,
        });
        progress.report(DownloadEvent::Advanced {
            downloaded: 12_058_624,
            total: 12_058_624,
        });
        progress.report(DownloadEvent::Finished {
            downloaded: 12_058_624,
        });

        let output = String::from_utf8(progress.writer).expect("progress is UTF-8");
        assert_eq!(
            output,
            concat!(
                "axe: downloading python 3.14.7-web.1 for x86_64-linux: ",
                "0/11.5 MiB (0%)",
                "\raxe: downloading python 3.14.7-web.1: 11.5/11.5 MiB (100%)\x1b[K",
                "\n",
            )
        );
    }

    #[test]
    fn removed_target_fallback_skips_bridge_and_runs_next_candidate_once() {
        let root = std::env::temp_dir().join(format!(
            "axe-ondemand-fallback-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        ));
        let bridge = root.join("bridge");
        let external = root.join("external");
        std::fs::create_dir_all(&bridge).expect("create bridge directory");
        std::fs::create_dir_all(&external).expect("create external directory");

        let current = std::env::current_exe().expect("locate test executable");
        let bridge_candidate = bridge.join("store-tool");
        symlink(&current, &bridge_candidate).expect("create recursive bridge candidate");

        let count = root.join("count");
        let external_candidate = external.join("store-tool");
        // Copy in an owned subprocess: parallel tests can fork while a file
        // is writable and retain its descriptor until exec despite CLOEXEC.
        // Waiting for the copier to exit prevents ETXTBSY on first execution.
        let prepared = Command::new(&current)
            .args([
                "--exact",
                "ondemand::tests::external_candidate_fixture",
                "--ignored",
            ])
            .env("AXE_TEST_COPY_EXECUTABLE_TO", &external_candidate)
            .output()
            .expect("prepare external candidate");
        assert!(prepared.status.success(), "{prepared:?}");

        let error = StoreError {
            stage: axe_store_client::StoreStage::Manifest,
            class: FailureClass::Unavailable,
            source: String::from("target removed"),
        };
        let executable = crate::executable::Executable::from_filesystem_path(&current)
            .expect("record test executable");
        let code = run_path_candidates(
            "store-tool",
            &[
                OsString::from("--exact"),
                OsString::from("ondemand::tests::external_candidate_fixture"),
                OsString::from("--ignored"),
                OsString::from("--nocapture"),
            ],
            &error,
            [&bridge_candidate, &external_candidate],
            Some(&bridge),
            &executable,
        );

        assert_eq!(code, 0);
        assert_eq!(std::fs::read(&count).expect("read execution count"), b"x");
        std::fs::remove_dir_all(root).expect("remove fallback test directory");
    }

    #[test]
    #[ignore = "owned subprocess fixture for PATH fallback"]
    fn external_candidate_fixture() {
        let current = std::env::current_exe().expect("locate fallback candidate");
        if let Some(destination) = std::env::var_os("AXE_TEST_COPY_EXECUTABLE_TO") {
            std::fs::copy(&current, destination).expect("copy external candidate");
            return;
        }
        if current.file_name() != Some(OsStr::new("store-tool")) {
            return;
        }
        let count = current
            .parent()
            .and_then(Path::parent)
            .expect("locate fallback test root")
            .join("count");

        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(count)
            .expect("open execution count")
            .write_all(b"x")
            .expect("record fallback execution");
    }
}
