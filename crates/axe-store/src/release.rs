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

pub struct ReleaseOptions<'a> {
    pub workspace: &'a Path,
    pub input: &'a Path,
    pub metadata: &'a Path,
    pub targets: &'a [Target],
    pub backend: Backend,
    pub directory: Option<&'a Path>,
    pub config: &'a Path,
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
        let (synthetic_version, versioned_key, stable_key) = release_keys(target, digest);
        let record = ReleaseRecord {
            version: synthetic_version,
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

    for artifact in &artifacts {
        eprintln!(
            "axe-store: release: publishing immutable {}",
            artifact.versioned_key
        );
        sink.put_immutable(&artifact.versioned_key, &artifact.path, artifact.digest)?;
    }
    for artifact in &artifacts {
        eprintln!(
            "axe-store: release: updating stable {}",
            artifact.stable_key
        );
        sink.put_stable(&artifact.stable_key, &artifact.path)?;
    }

    let mut records = read_release_records(options.metadata)?;
    for artifact in &artifacts {
        records.insert(artifact.target.as_str().to_owned(), artifact.record.clone());
    }
    write_release_records(options.metadata, &records)?;

    Ok(artifacts.len())
}

fn artifact_filename(target: Target) -> &'static str {
    match target {
        Target::X86_64Linux => "axe-x86_64-unknown-linux-musl",
        Target::Aarch64Linux => "axe-aarch64-unknown-linux-musl",
        Target::Aarch64Darwin => "axe-aarch64-apple-darwin",
    }
}

fn release_keys(target: Target, digest: Digest) -> (String, String, String) {
    let synthetic_version = format!("{}-{digest}", env!("CARGO_PKG_VERSION"));
    let installed_name = "axe";
    let versioned_key = format!(
        "releases/{synthetic_version}/{}/{installed_name}",
        target.as_str()
    );
    let stable_key = format!("stable/{}/{installed_name}", target.as_str());
    (synthetic_version, versioned_key, stable_key)
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

    fn put_stable(&mut self, key: &str, path: &Path) -> Result<(), String> {
        self.put_file(key, path, STABLE_CACHE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_paths_separate_stable_and_content_addressed_objects() {
        let target = Target::X86_64Linux;
        let digest: Digest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .parse()
            .unwrap();
        let (_, versioned, stable) = release_keys(target, digest);

        assert_eq!(
            versioned,
            format!(
                "releases/{}-{digest}/x86_64-linux/axe",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(stable, "stable/x86_64-linux/axe");
    }
}
