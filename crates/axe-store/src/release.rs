use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::fs_atomic::{self, InstallMode};
use axe_artifact::{Digest, Target};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::publish::{Backend, S3Sink, read_config};
use crate::validation::validate_executable;

const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
const STABLE_CACHE: &str = "public, max-age=0, must-revalidate";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleasePhase {
    All,
    ImmutableOnly,
    StableOnly,
}

pub struct ReleaseOptions<'a> {
    pub workspace: &'a Path,
    pub input: &'a Path,
    pub metadata: &'a Path,
    pub targets: &'a [Target],
    pub backend: Backend,
    pub directory: Option<&'a Path>,
    pub config: &'a Path,
    pub phase: ReleasePhase,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRecord {
    version: String,
    url: String,
    stable_url: String,
    hash: String,
}

struct Artifact {
    target: Target,
    path: PathBuf,
    digest: Digest,
    versioned_key: String,
    stable_key: String,
    record: ReleaseRecord,
}

trait ReleaseSink {
    fn put_immutable(&mut self, key: &str, path: &Path, digest: Digest) -> Result<(), String>;
    fn verify_immutable(&mut self, key: &str, path: &Path, digest: Digest) -> Result<(), String>;
    fn put_stable(&mut self, key: &str, path: &Path) -> Result<(), String>;
}

pub fn release(options: &ReleaseOptions<'_>) -> Result<usize, String> {
    if options.targets.is_empty() {
        return Err("at least one Axe release target is required".into());
    }

    let config = read_config(options.config)?;
    let release_prefix = config.storage.release_prefix.trim_matches('/');
    let public_base = format!(
        "{}/{}/{release_prefix}",
        config.storage.endpoint.trim_end_matches('/'),
        config.storage.bucket.trim_matches('/')
    );
    let targets = options.targets.iter().copied().collect::<BTreeSet<_>>();
    let mut artifacts = Vec::with_capacity(targets.len());

    for target in targets {
        let path = options.input.join(artifact_filename(target));
        let fully_static = validate_executable(&path, target, false)?;
        if target.is_linux() && !fully_static {
            return Err(format!(
                "Axe release {} is not a fully static Linux executable",
                path.display()
            ));
        }

        let digest = Digest::from_file(&path)
            .map_err(|error| format!("digest {}: {error}", path.display()))?;
        let (versioned_key, stable_key) = release_keys(target);
        let record = ReleaseRecord {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            url: format!("{public_base}/{versioned_key}"),
            stable_url: format!("{public_base}/{stable_key}"),
            hash: format!("sha256-{}", BASE64.encode(digest.0)),
        };
        artifacts.push(Artifact {
            target,
            path,
            digest,
            versioned_key,
            stable_key,
            record,
        });
    }

    if options.phase == ReleasePhase::StableOnly {
        let records = read_release_records(options.metadata)?;
        for artifact in &artifacts {
            if records.get(artifact.target.as_str()) != Some(&artifact.record) {
                return Err(format!(
                    "release metadata for {} does not match local version, URLs and hash",
                    artifact.target
                ));
            }
        }
    }

    let mut sink: Box<dyn ReleaseSink> = match options.backend {
        Backend::Directory => Box::new(DirectoryReleaseSink::new(
            options
                .directory
                .ok_or_else(|| "--directory is required for directory backend".to_string())?,
            release_prefix,
        )?),
        Backend::S3 => Box::new(S3Sink::for_prefix(
            options.workspace,
            &config,
            release_prefix,
        )?),
    };

    if options.phase != ReleasePhase::StableOnly {
        for artifact in &artifacts {
            eprintln!(
                "axe-store: release: publishing immutable {}",
                artifact.versioned_key
            );
            sink.put_immutable(&artifact.versioned_key, &artifact.path, artifact.digest)?;
        }
        if options.phase == ReleasePhase::ImmutableOnly {
            let mut records = read_release_records(options.metadata)?;
            for artifact in &artifacts {
                records.insert(artifact.target.as_str().to_owned(), artifact.record.clone());
            }
            write_release_records(options.metadata, &records)?;
        }
    } else {
        for artifact in &artifacts {
            sink.verify_immutable(&artifact.versioned_key, &artifact.path, artifact.digest)?;
        }
    }
    if options.phase != ReleasePhase::ImmutableOnly {
        for artifact in &artifacts {
            eprintln!(
                "axe-store: release: updating stable {}",
                artifact.stable_key
            );
            sink.put_stable(&artifact.stable_key, &artifact.path)?;
        }
    }
    if options.phase == ReleasePhase::All {
        let mut records = read_release_records(options.metadata)?;
        for artifact in &artifacts {
            records.insert(artifact.target.as_str().to_owned(), artifact.record.clone());
        }
        write_release_records(options.metadata, &records)?;
    }

    Ok(artifacts.len())
}

fn artifact_filename(target: Target) -> &'static str {
    match target {
        Target::X86_64Linux => "axe-x86_64-unknown-linux-musl",
        Target::Aarch64Linux => "axe-aarch64-unknown-linux-musl",
        Target::Aarch64Darwin => "axe-aarch64-apple-darwin",
    }
}

fn release_keys(target: Target) -> (String, String) {
    let installed_name = "axe";
    let versioned_key = format!(
        "releases/v{}/{}/{installed_name}",
        env!("CARGO_PKG_VERSION"),
        target.as_str()
    );
    let stable_key = format!("stable/{}/{installed_name}", target.as_str());
    (versioned_key, stable_key)
}

fn read_release_records(path: &Path) -> Result<BTreeMap<String, ReleaseRecord>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse {}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

fn write_release_records(
    path: &Path,
    records: &BTreeMap<String, ReleaseRecord>,
) -> Result<(), String> {
    fs_atomic::write(path, InstallMode::Replace, None, |file| {
        serde_json::to_writer_pretty(&mut *file, records)?;
        file.write_all(b"\n")
    })
    .map_err(|error| format!("write {}: {error}", path.display()))
}

struct DirectoryReleaseSink {
    root: PathBuf,
}

impl DirectoryReleaseSink {
    fn new(directory: &Path, prefix: &str) -> Result<Self, String> {
        let root = directory.join(prefix);
        fs::create_dir_all(&root).map_err(|error| format!("create {}: {error}", root.display()))?;
        Ok(Self { root })
    }

    fn destination(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    fn copy(&self, key: &str, source: &Path, install: InstallMode) -> io::Result<()> {
        fs_atomic::copy(source, &self.destination(key), install)
    }
}

impl ReleaseSink for DirectoryReleaseSink {
    fn put_immutable(&mut self, key: &str, path: &Path, digest: Digest) -> Result<(), String> {
        let destination = self.destination(key);
        match self.copy(key, path, InstallMode::NoReplace) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = Digest::from_file(&destination)
                    .map_err(|error| format!("digest {}: {error}", destination.display()))?;
                if existing == digest {
                    Ok(())
                } else {
                    Err(format!(
                        "immutable Axe release {} already exists with different bytes",
                        destination.display()
                    ))
                }
            }
            Err(error) => Err(format!("write {}: {error}", destination.display())),
        }
    }

    fn verify_immutable(&mut self, key: &str, _path: &Path, digest: Digest) -> Result<(), String> {
        let destination = self.destination(key);
        match Digest::from_file(&destination) {
            Ok(existing) if existing == digest => Ok(()),
            Ok(_) => Err(format!(
                "immutable Axe release {} already exists with different bytes",
                destination.display()
            )),
            Err(error) => Err(format!("verify {}: {error}", destination.display())),
        }
    }

    fn put_stable(&mut self, key: &str, path: &Path) -> Result<(), String> {
        let destination = self.destination(key);
        self.copy(key, path, InstallMode::Replace)
            .map_err(|error| format!("write {}: {error}", destination.display()))
    }
}

impl ReleaseSink for S3Sink {
    fn put_immutable(&mut self, key: &str, path: &Path, digest: Digest) -> Result<(), String> {
        self.put_file_immutable(key, path, digest, IMMUTABLE_CACHE)
    }

    fn verify_immutable(&mut self, key: &str, path: &Path, digest: Digest) -> Result<(), String> {
        self.verify_file_immutable(key, path, digest)
    }

    fn put_stable(&mut self, key: &str, path: &Path) -> Result<(), String> {
        self.put_file(key, path, STABLE_CACHE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rand::RngExt as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "axe-store-release-{}-{:016x}",
                std::process::id(),
                rand::rng().random::<u64>()
            ));
            fs::create_dir(&root).expect("create release fixture");
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_elf(path: &Path, machine: u16, entry: u8) {
        let mut bytes = [0_u8; 64];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16] = 2; // ET_EXEC
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[20] = 1; // EV_CURRENT
        bytes[24] = entry;
        bytes[52] = 64; // ELF64 header size
        fs::write(path, bytes).expect("write executable fixture");
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("make fixture executable");
    }

    #[test]
    fn release_phases_keep_target_metadata_and_reject_changed_immutable_bytes() {
        let scratch = Scratch::new();
        let input = scratch.0.join("dist");
        fs::create_dir(&input).expect("create dist");
        let x86 = input.join(artifact_filename(Target::X86_64Linux));
        let arm = input.join(artifact_filename(Target::Aarch64Linux));
        write_elf(&x86, goblin::elf::header::EM_X86_64, 1);
        write_elf(&arm, goblin::elf::header::EM_AARCH64, 2);
        let directory = scratch.0.join("published");
        let metadata = scratch.0.join("axe-releases.json");
        let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/store.json");
        let targets = [Target::X86_64Linux, Target::Aarch64Linux];
        let mut options = ReleaseOptions {
            workspace: &scratch.0,
            input: &input,
            metadata: &metadata,
            targets: &targets,
            backend: Backend::Directory,
            directory: Some(&directory),
            config: &config,
            phase: ReleasePhase::ImmutableOnly,
        };

        assert_eq!(release(&options).expect("publish immutable objects"), 2);
        let records = read_release_records(&metadata).expect("read published release metadata");
        assert_eq!(records.len(), 2);
        let prefix = "https://storage.yandexcloud.net/axe-store/axe";
        for target in targets {
            let record = &records[target.as_str()];
            let source = input.join(artifact_filename(target));
            let digest = Digest::from_file(&source).expect("digest fixture");
            assert_eq!(record.version, env!("CARGO_PKG_VERSION"));
            assert_eq!(
                record.url,
                format!(
                    "{prefix}/releases/v{}/{}/axe",
                    record.version,
                    target.as_str()
                )
            );
            assert_eq!(
                record.stable_url,
                format!("{prefix}/stable/{}/axe", target.as_str())
            );
            assert_eq!(record.hash, format!("sha256-{}", BASE64.encode(digest.0)));
            assert_eq!(
                fs::read(directory.join("axe").join(format!(
                    "releases/v{}/{}/axe",
                    record.version,
                    target.as_str()
                )))
                .expect("read immutable release"),
                fs::read(&source).expect("read source")
            );
            assert!(!directory.join("axe/stable").join(target.as_str()).exists());
        }
        assert_ne!(
            records[Target::X86_64Linux.as_str()].hash,
            records[Target::Aarch64Linux.as_str()].hash
        );

        release(&options).expect("retry identical immutable release");
        options.phase = ReleasePhase::StableOnly;
        let immutable_arm = directory.join(format!(
            "axe/releases/v{}/aarch64-linux/axe",
            env!("CARGO_PKG_VERSION")
        ));
        let original_arm = fs::read(&arm).expect("read original aarch64");
        fs::write(&immutable_arm, b"changed remote bytes").expect("corrupt remote immutable");
        let error = release(&options).expect_err("stable phase must verify every immutable object");
        assert!(error.contains("already exists with different bytes"));
        assert!(!directory.join("axe/stable/x86_64-linux").exists());
        fs::write(&immutable_arm, &original_arm).expect("restore remote immutable");
        release(&options).expect("publish stable objects after verifying immutable bytes");
        let original_x86 = fs::read(&x86).expect("read original x86");
        let stable_x86 = directory.join("axe/stable/x86_64-linux/axe");
        assert_eq!(
            fs::read(&stable_x86).expect("read stable x86"),
            original_x86
        );
        assert_eq!(
            fs::read(directory.join("axe/stable/aarch64-linux/axe")).expect("read stable aarch64"),
            original_arm
        );

        write_elf(&x86, goblin::elf::header::EM_X86_64, 3);
        let error = release(&options).expect_err("stable phase must match staged metadata");
        assert!(error.contains("does not match local version, URLs and hash"));
        options.phase = ReleasePhase::ImmutableOnly;
        let error = release(&options).expect_err("different bytes cannot reuse the same version");
        assert!(error.contains("already exists with different bytes"));
        assert_eq!(
            read_release_records(&metadata).expect("read metadata after conflict"),
            records
        );
        assert_eq!(
            fs::read(&stable_x86).expect("read unchanged stable x86"),
            original_x86
        );
    }
}
