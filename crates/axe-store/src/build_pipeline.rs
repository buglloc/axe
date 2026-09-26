use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use axe_artifact::{
    Artifact, ArtifactKind, CanonicalTree, CanonicalTreeEntryKind, Digest, SCHEMA_VERSION,
    StoreIndex, StoreIndexEntry, Target, ToolManifest, ToolVersion, sign_artifact, sign_document,
};
use ed25519_dalek::SigningKey;
use serde::Deserialize;

use crate::fs_atomic::{self, InstallMode};
use crate::keys::load_signing_key;
use crate::package::{ArtifactSpec, PackageDefinition, parse_package_set};
use crate::validation::validate_executable;

#[derive(Debug)]
pub struct BuildOptions<'a> {
    pub workspace: &'a Path,
    pub flake: &'a Path,
    pub package: Option<&'a str>,
    pub target: Option<Target>,
    pub output: &'a Path,
}

#[derive(Debug)]
pub struct BuildResult {
    pub packages: usize,
    pub targets: usize,
    pub failures: Vec<String>,
}

struct PackageOutput<'a> {
    stage: &'a Path,
    signing: &'a SigningKey,
}

#[derive(Debug, Deserialize)]
struct NixBuildOutput {
    #[serde(rename = "drvPath")]
    drv_path: String,
    outputs: BTreeMap<String, Option<PathBuf>>,
}

#[derive(Debug)]
struct NixBuildJob {
    attribute: String,
    target: Target,
    reference: String,
}

type AcquiredPackages = BTreeMap<String, BTreeMap<Target, Result<PathBuf, String>>>;

pub fn build(options: &BuildOptions<'_>) -> Result<BuildResult, String> {
    eprintln!("axe-store: build: evaluating AXE Store package metadata");
    let packages = load_packages(options)?;
    let intended_target_count = packages
        .iter()
        .map(|(_, package)| {
            package
                .targets
                .iter()
                .filter(|target| options.target.is_none_or(|selected| selected == **target))
                .count()
        })
        .sum::<usize>();
    eprintln!(
        "axe-store: build: selected {} packages with {intended_target_count} targets",
        packages.len()
    );

    eprintln!("axe-store: build: loading signing key");
    let signing = load_signing_key(options.workspace)?;
    let parent = options
        .output
        .parent()
        .ok_or_else(|| "output directory has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;

    let work = parent.join(format!(".axe-store-work-{}", std::process::id()));
    let stage = parent.join(format!(".axe-store-stage-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&work).map_err(|error| format!("create {}: {error}", work.display()))?;
    fs::create_dir_all(&stage).map_err(|error| format!("create {}: {error}", stage.display()))?;
    let cleanup = Cleanup([work.clone(), stage.clone()]);
    let package_output = PackageOutput {
        stage: &stage,
        signing: &signing,
    };

    let mut acquired = acquire_nix_packages(options, &packages)?;
    let mut tools = BTreeMap::new();
    let mut failures = Vec::new();
    let mut attempted_targets = 0_usize;
    let mut target_count = 0_usize;

    for (attribute, package) in &packages {
        let version = package.exact_version()?.to_owned();
        let intended = package
            .targets
            .iter()
            .copied()
            .filter(|target| options.target.is_none_or(|selected| selected == *target))
            .collect::<Vec<_>>();
        let mut artifacts = BTreeMap::new();

        for target in intended {
            attempted_targets += 1;
            let acquisition = acquired
                .get_mut(attribute)
                .and_then(|targets| targets.remove(&target))
                .ok_or_else(|| format!("missing Nix build result for {} {target}", package.id))?;
            let result = match acquisition {
                Ok(acquired) => {
                    eprintln!(
                        "axe-store: build [{attempted_targets}/{intended_target_count}]: {} {target}: validating and packaging",
                        package.id
                    );
                    package_acquired(
                        package,
                        target,
                        &acquired,
                        &work.join(format!("{}-{target}", package.name)),
                        package_output.stage,
                        package_output.signing,
                    )
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(artifact) => {
                    eprintln!(
                        "axe-store: build [{attempted_targets}/{intended_target_count}]: {} {target}: ready",
                        package.id
                    );
                    artifacts.insert(target, artifact);
                    target_count += 1;
                }
                Err(error) => {
                    let failure = format!("{} {target}: {error}", package.id);
                    eprintln!("axe-store: build: unsupported: {failure}");
                    failures.push(failure);
                }
            }
        }

        if artifacts.is_empty() {
            let failure = format!("{}: omitted because no target succeeded", package.id);
            eprintln!("axe-store: build: unsupported: {failure}");
            failures.push(failure);
            continue;
        }

        let artifacts = artifacts
            .into_iter()
            .map(|(target, artifact)| (target.to_string(), artifact))
            .collect();
        let manifest = ToolManifest {
            schema_version: SCHEMA_VERSION,
            id: package.id.clone(),
            name: package.name.clone(),
            ttl_secs: 86_400,
            channels: package.channels.clone(),
            versions: BTreeMap::from([(version, ToolVersion { targets: artifacts })]),
        };
        manifest.validate().map_err(|error| error.to_string())?;

        let entry = write_manifest(&stage, package, &manifest, &signing)?;
        tools.insert(package.name.clone(), entry);
    }

    eprintln!("axe-store: build: signing Index and installing snapshot");

    let package_count = tools.len();
    write_index(&stage, tools, &signing, 1)?;
    sync_tree(&stage).map_err(|error| format!("sync staging tree: {error}"))?;

    if options.output.exists() {
        fs::remove_dir_all(options.output)
            .map_err(|error| format!("replace {}: {error}", options.output.display()))?;
    }
    fs::rename(&stage, options.output)
        .map_err(|error| format!("install {}: {error}", options.output.display()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync {}: {error}", parent.display()))?;

    eprintln!(
        "axe-store: build: snapshot installed at {}",
        options.output.display()
    );

    drop(cleanup);
    Ok(BuildResult {
        packages: package_count,
        targets: target_count,
        failures,
    })
}

fn load_packages(options: &BuildOptions<'_>) -> Result<Vec<(String, PackageDefinition)>, String> {
    let reference = flake_reference(options, "lib.axeStoreMetadata")?;
    let output = nix_command()
        .args(["eval", "--json"])
        .arg(&reference)
        .output()
        .map_err(|error| format!("start nix eval: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "nix eval {reference} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_package_set(&output.stdout, options.package)
}

fn acquire_nix_packages(
    options: &BuildOptions<'_>,
    packages: &[(String, PackageDefinition)],
) -> Result<AcquiredPackages, String> {
    let mut jobs = Vec::new();
    for (attribute, package) in packages {
        for target in package
            .targets
            .iter()
            .copied()
            .filter(|target| options.target.is_none_or(|selected| selected == *target))
        {
            jobs.push(NixBuildJob {
                attribute: attribute.clone(),
                target,
                reference: flake_reference(
                    options,
                    &format!("packages.{}.{attribute}", nix_system(target)?),
                )?,
            });
        }
    }

    if jobs.is_empty() {
        return Ok(BTreeMap::new());
    }

    eprintln!(
        "axe-store: build: planning {} package targets for batched Nix realization",
        jobs.len()
    );
    let plans = match plan_nix_builds(&jobs) {
        Ok(plans) => plans,
        Err(error) => {
            eprintln!(
                "axe-store: build: batched Nix planning failed ({error}); building package targets individually"
            );
            return acquire_nix_packages_individually(jobs);
        }
    };

    let mut acquired = BTreeMap::new();
    let mut batch = Vec::new();
    for (job, plan) in jobs.into_iter().zip(plans) {
        match plan {
            Ok(output) => batch.push((job, output)),
            Err(error) => {
                eprintln!(
                    "axe-store: build: {} {} cannot be batched ({error}); running individual Nix build",
                    job.attribute, job.target
                );
                let result = build_nix_reference(&job.reference);
                record_acquisition(&mut acquired, job, result)?;
            }
        }
    }

    if batch.is_empty() {
        return Ok(acquired);
    }

    eprintln!(
        "axe-store: build: realizing {} package targets in one Nix build",
        batch.len()
    );
    let status = realise_nix_batch(&batch)?;
    let validity = query_nix_output_validity(&batch)?;

    for (job, output) in batch {
        let output_text = output
            .to_str()
            .ok_or_else(|| format!("Nix output path {} is not UTF-8", output.display()))?;
        let result = match validity.get(output_text) {
            Some(Some(_)) => Ok(output),
            Some(None) if status.success() => Err(format!(
                "nix build {} succeeded without realising {}",
                job.reference,
                output.display()
            )),
            Some(None) => Err(format!("nix build {} failed with {status}", job.reference)),
            None => {
                return Err(format!(
                    "nix path-info omitted expected output {}",
                    output.display()
                ));
            }
        };
        record_acquisition(&mut acquired, job, result)?;
    }

    Ok(acquired)
}

fn plan_nix_builds(jobs: &[NixBuildJob]) -> Result<Vec<Result<PathBuf, String>>, String> {
    let output = nix_command()
        .args(["build", "--dry-run", "--no-link", "--json"])
        .args(jobs.iter().map(|job| job.reference.as_str()))
        .output()
        .map_err(|error| format!("start Nix batch planning: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "nix build --dry-run failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let plans: Vec<NixBuildOutput> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse Nix batch plan: {error}"))?;

    if plans.len() != jobs.len() {
        return Err(format!(
            "Nix batch plan returned {} derivations, expected {}",
            plans.len(),
            jobs.len()
        ));
    }

    Ok(plans.into_iter().map(nix_output_path).collect())
}

fn realise_nix_batch(jobs: &[(NixBuildJob, PathBuf)]) -> Result<std::process::ExitStatus, String> {
    nix_command()
        .args(["build", "--no-link", "--keep-going"])
        .args(jobs.iter().map(|(job, _)| job.reference.as_str()))
        .status()
        .map_err(|error| format!("start batched nix build: {error}"))
}

fn query_nix_output_validity(
    jobs: &[(NixBuildJob, PathBuf)],
) -> Result<BTreeMap<String, Option<serde_json::Value>>, String> {
    let output = nix_command()
        .args(["path-info", "--json"])
        .args(jobs.iter().map(|(_, output)| output))
        .output()
        .map_err(|error| format!("start nix path-info: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "nix path-info failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse nix path-info output: {error}"))
}

fn acquire_nix_packages_individually(jobs: Vec<NixBuildJob>) -> Result<AcquiredPackages, String> {
    let mut acquired = BTreeMap::new();
    for job in jobs {
        eprintln!(
            "axe-store: build: {} {}: running individual Nix build",
            job.attribute, job.target
        );
        let result = build_nix_reference(&job.reference);
        record_acquisition(&mut acquired, job, result)?;
    }
    Ok(acquired)
}

fn record_acquisition(
    acquired: &mut AcquiredPackages,
    job: NixBuildJob,
    result: Result<PathBuf, String>,
) -> Result<(), String> {
    match acquired.entry(job.attribute) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(BTreeMap::from([(job.target, result)]));
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            if entry.get_mut().insert(job.target, result).is_some() {
                return Err(format!(
                    "duplicate Nix build result for {} {}",
                    entry.key(),
                    job.target
                ));
            }
            Ok(())
        }
    }
}

fn build_nix_reference(reference: &str) -> Result<PathBuf, String> {
    let output = nix_command()
        .args(["build", "--no-link", "--json"])
        .arg(reference)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("start nix build: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "nix build {reference} failed with {}",
            output.status
        ));
    }

    let mut builds: Vec<NixBuildOutput> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse nix build output: {error}"))?;
    if builds.len() != 1 {
        return Err(format!(
            "nix build returned {} derivations, expected one",
            builds.len()
        ));
    }
    nix_output_path(builds.pop().expect("one Nix build output was checked"))
}

fn nix_output_path(mut build: NixBuildOutput) -> Result<PathBuf, String> {
    if let Some(output) = build.outputs.remove("out") {
        return output.ok_or_else(|| {
            format!(
                "Nix derivation {} has no statically known out path",
                build.drv_path
            )
        });
    }

    if build.outputs.len() == 1 {
        return build
            .outputs
            .into_values()
            .next()
            .expect("one Nix output was checked")
            .ok_or_else(|| {
                format!(
                    "Nix derivation {} has no statically known output path",
                    build.drv_path
                )
            });
    }

    Err(format!(
        "Nix derivation {} has ambiguous outputs: {}",
        build.drv_path,
        build.outputs.keys().cloned().collect::<Vec<_>>().join(", ")
    ))
}

fn nix_command() -> Command {
    let mut command = Command::new("nix");
    command.args(["--extra-experimental-features", "nix-command flakes"]);
    command
}

fn flake_reference(options: &BuildOptions<'_>, attribute: &str) -> Result<String, String> {
    let path = if options.flake.is_absolute() {
        options.flake.to_owned()
    } else {
        options.workspace.join(options.flake)
    };
    let path_text = path
        .to_str()
        .ok_or_else(|| format!("flake path {} is not UTF-8", path.display()))?;
    let reference = if path.join(".git").exists() {
        path_text.to_owned()
    } else {
        format!("path:{path_text}")
    };
    // Git worktree roots need implicit references so Nix filters ignored build output.
    // Standalone flakes, including untracked fixtures nested in a worktree, need `path:`.
    Ok(format!("{reference}#{attribute}"))
}

fn nix_system(target: Target) -> Result<&'static str, String> {
    match target {
        Target::X86_64Linux => Ok("x86_64-linux"),
        Target::Aarch64Linux => Ok("aarch64-linux"),
        Target::Aarch64Darwin => Ok("aarch64-darwin"),
    }
}

fn package_acquired(
    package: &PackageDefinition,
    target: Target,
    acquired: &Path,
    work: &Path,
    stage: &Path,
    signing: &SigningKey,
) -> Result<Artifact, String> {
    fs::create_dir_all(work).map_err(|error| format!("create {}: {error}", work.display()))?;

    let (kind, unpacked_size, unpacked_sha256, fully_static, input) = match &package.artifact {
        ArtifactSpec::SingleBinary { path } => {
            let executable = acquired.join(path);
            let fully_static = validate_executable(&executable, target, true)?;
            let metadata = fs::metadata(&executable)
                .map_err(|error| format!("inspect {}: {error}", executable.display()))?;
            let digest = Digest::from_file(&executable).map_err(io_string)?;
            (
                ArtifactKind::SingleBinary,
                metadata.len(),
                digest,
                fully_static,
                executable,
            )
        }
        ArtifactSpec::Package { entrypoint } => {
            let executable = acquired.join(entrypoint);
            validate_executable(&executable, target, false)?;
            let tree = CanonicalTree::collect(acquired).map_err(|error| error.to_string())?;
            let digest = tree.digest().map_err(|error| error.to_string())?;
            let tar_path = work.join("package.tar");
            write_deterministic_tar(&tree, &tar_path)?;
            let size = fs::metadata(&tar_path)
                .map_err(|error| format!("inspect package tar: {error}"))?
                .len();
            (
                ArtifactKind::Package {
                    entrypoint: entrypoint.clone(),
                },
                size,
                digest,
                false,
                tar_path,
            )
        }
    };

    let compressed = work.join("payload.zst");
    compress_file(&input, &compressed)?;
    let mut source =
        File::open(&compressed).map_err(|error| format!("open compressed payload: {error}"))?;
    let temporary = work.join("object");
    let mut object = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("create signed object: {error}"))?;
    let written = sign_artifact(&mut source, &mut object, signing)
        .map_err(|error| format!("sign artifact: {error}"))?;
    object
        .sync_all()
        .map_err(|error| format!("sync object: {error}"))?;
    drop(object);

    let relative = written.object_sha256.object_path();
    let destination = stage.join(&relative);
    match fs_atomic::install(&temporary, &destination, InstallMode::NoReplace) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = Digest::from_file(&destination).map_err(io_string)?;
            if existing != written.object_sha256 {
                return Err(format!("CAS collision at {}", destination.display()));
            }
            fs::remove_file(&temporary)
                .map_err(|error| format!("remove duplicate object: {error}"))?;
        }
        Err(error) => {
            return Err(format!("install {}: {error}", destination.display()));
        }
    }

    Ok(Artifact {
        kind,
        object_sha256: written.object_sha256,
        compressed_size: written.compressed_size,
        unpacked_size,
        unpacked_sha256,
        fully_static,
    })
}

fn write_manifest(
    stage: &Path,
    package: &PackageDefinition,
    manifest: &ToolManifest,
    signing: &SigningKey,
) -> Result<StoreIndexEntry, String> {
    let bytes =
        sign_document(manifest, signing).map_err(|error| format!("sign manifest: {error}"))?;
    let manifest_sha256 = Digest::of(&bytes);
    let channels = manifest
        .channels
        .iter()
        .map(|(channel, version)| {
            let targets = manifest.versions[version]
                .targets
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>();
            (channel.clone(), targets)
        })
        .collect();
    let entry = StoreIndexEntry {
        id: package.id.clone(),
        aliases: package.aliases.clone(),
        manifest_sha256,
        channels,
        synopsis: package.synopsis.clone(),
    };

    let path = stage.join(entry.manifest_path());
    fs_atomic::write(&path, InstallMode::NoReplace, None, |file| {
        file.write_all(&bytes)
    })
    .map_err(|error| format!("stage immutable manifest {}: {error}", path.display()))?;

    let staged_sha256 =
        Digest::from_file(&path).map_err(|error| format!("digest {}: {error}", path.display()))?;
    if staged_sha256 != manifest_sha256 {
        return Err(format!(
            "staged manifest {} changed while writing",
            path.display()
        ));
    }

    Ok(entry)
}

fn write_index(
    stage: &Path,
    tools: BTreeMap<String, StoreIndexEntry>,
    signing: &SigningKey,
    generation: u64,
) -> Result<(), String> {
    let index = StoreIndex {
        schema_version: SCHEMA_VERSION,
        generation,
        ttl_secs: 3_600,
        tools,
    };
    index.validate().map_err(|error| error.to_string())?;
    let json = index_json(&index)?;
    let signed = sign_document(&index, signing).map_err(|error| format!("sign Index: {error}"))?;
    let compressed = zstd::stream::encode_all(signed.as_slice(), 9)
        .map_err(|error| format!("compress Index: {error}"))?;

    write_file(&stage.join("index.cbor.zst"), &compressed)?;
    write_file(&stage.join("index.json"), &json)
}

pub(crate) fn index_json(index: &StoreIndex) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(index)
        .map_err(|error| format!("serialize Index as JSON: {error}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_deterministic_tar(tree: &CanonicalTree, destination: &Path) -> Result<(), String> {
    let file = File::create(destination).map_err(io_string)?;
    let mut builder = tar::Builder::new(file);
    builder.mode(tar::HeaderMode::Deterministic);
    for entry in tree.entries() {
        let mut header = tar::Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_mode(entry.mode());

        match entry.kind() {
            CanonicalTreeEntryKind::Directory => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                builder
                    .append_data(&mut header, entry.relative_path(), io::empty())
                    .map_err(io_string)?;
            }
            CanonicalTreeEntryKind::File => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(entry.size());
                header.set_cksum();
                let mut file = File::open(entry.source()).map_err(io_string)?;
                builder
                    .append_data(&mut header, entry.relative_path(), &mut file)
                    .map_err(io_string)?;
            }
            CanonicalTreeEntryKind::Symlink => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header
                    .set_link_name(
                        entry
                            .link_target()
                            .expect("symlink entries have link targets"),
                    )
                    .map_err(io_string)?;
                header.set_cksum();
                builder
                    .append_data(&mut header, entry.relative_path(), io::empty())
                    .map_err(io_string)?;
            }
        }
    }

    builder.finish().map_err(io_string)?;
    builder
        .into_inner()
        .map_err(io_string)?
        .sync_all()
        .map_err(io_string)
}

fn compress_file(source: &Path, destination: &Path) -> Result<(), String> {
    let mut input = File::open(source).map_err(io_string)?;
    let output = File::create(destination).map_err(io_string)?;
    let mut encoder = zstd::Encoder::new(output, 19).map_err(io_string)?;
    encoder.include_checksum(true).map_err(io_string)?;
    io::copy(&mut input, &mut encoder).map_err(io_string)?;
    encoder
        .finish()
        .map_err(io_string)?
        .sync_all()
        .map_err(io_string)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_string)?;
    }
    let mut file = File::create(path).map_err(io_string)?;
    file.write_all(bytes).map_err(io_string)?;
    file.sync_all().map_err(io_string)
}

fn sync_tree(root: &Path) -> io::Result<()> {
    let mut directories = vec![root.to_owned()];
    let mut index = 0;
    while index < directories.len() {
        for entry in fs::read_dir(&directories[index])? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                directories.push(entry.path());
            }
        }
        index += 1;
    }

    for directory in directories.into_iter().rev() {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

fn io_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}

struct Cleanup([PathBuf; 2]);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn staged_index_binds_exact_signed_manifest_bytes() {
        let stage = std::env::temp_dir().join(format!(
            "axe-store-manifest-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        ));
        fs::create_dir(&stage).expect("create staging directory");
        let signing = SigningKey::from_bytes(&[23; 32]);
        let target = Target::X86_64Linux;
        let package = PackageDefinition {
            id: "test/tool".into(),
            name: "tool".into(),
            aliases: vec!["alias".into()],
            synopsis: Some("A test tool".into()),
            channels: BTreeMap::from([("stable".into(), "1.0.0".into())]),
            targets: BTreeSet::from([target]),
            artifact: ArtifactSpec::SingleBinary {
                path: "bin/tool".into(),
            },
        };

        let manifest = ToolManifest {
            schema_version: SCHEMA_VERSION,
            id: package.id.clone(),
            name: package.name.clone(),
            ttl_secs: 60,
            channels: package.channels.clone(),
            versions: BTreeMap::from([(
                "1.0.0".into(),
                ToolVersion {
                    targets: BTreeMap::from([(
                        target.to_string(),
                        Artifact {
                            kind: ArtifactKind::SingleBinary,
                            object_sha256: Digest([4; 32]),
                            compressed_size: 1,
                            unpacked_size: 1,
                            unpacked_sha256: Digest([5; 32]),
                            fully_static: true,
                        },
                    )]),
                },
            )]),
        };

        let entry =
            write_manifest(&stage, &package, &manifest, &signing).expect("stage signed manifest");
        write_index(
            &stage,
            BTreeMap::from([(package.name.clone(), entry)]),
            &signing,
            1,
        )
        .expect("write signed Index");

        let mut trusted = axe_artifact::TrustedKeys::new();
        trusted.insert(signing.verifying_key());
        let signed_index =
            zstd::stream::decode_all(fs::read(stage.join("index.cbor.zst")).unwrap().as_slice())
                .expect("decompress Index");
        let index: StoreIndex =
            axe_artifact::verify_document(&signed_index, &trusted).expect("verify Index");
        let entry = &index.tools["tool"];
        let path = stage.join(entry.manifest_path());
        let signed_manifest = fs::read(&path).expect("read staged manifest");
        assert_eq!(Digest::of(&signed_manifest), entry.manifest_sha256);
        let verified: ToolManifest =
            axe_artifact::verify_document(&signed_manifest, &trusted).expect("verify manifest");
        assert_eq!(verified, manifest);
        assert_eq!(
            serde_json::from_slice::<StoreIndex>(&fs::read(stage.join("index.json")).unwrap())
                .expect("parse JSON inventory"),
            index
        );
        assert!(!stage.join("tools/test/tool/manifest.cbor").exists());
        fs::remove_dir_all(stage).expect("remove staging directory");
    }

    #[test]
    fn deterministic_tar_preserves_canonical_tree_digest() {
        let fixture = std::env::temp_dir().join(format!(
            "axe-store-tree-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        ));
        let source = fixture.join("source");
        let extracted = fixture.join("extracted");
        fs::create_dir_all(source.join("bin")).expect("create package tree");
        fs::create_dir(&extracted).expect("create extraction directory");
        fs::write(source.join("bin/tool"), b"#!/bin/sh\nexit 0\n").expect("write package tool");
        fs::set_permissions(source.join("bin/tool"), fs::Permissions::from_mode(0o755))
            .expect("make package tool executable");
        symlink("tool", source.join("bin/alias")).expect("create package symlink");
        fs::set_permissions(source.join("bin"), fs::Permissions::from_mode(0o555))
            .expect("make package directory read-only");

        let tree = CanonicalTree::collect(&source).expect("collect canonical tree");
        let expected = tree.digest().expect("digest canonical tree");
        let archive_path = fixture.join("package.tar");
        write_deterministic_tar(&tree, &archive_path).expect("write deterministic tar");
        let mut archive =
            tar::Archive::new(File::open(&archive_path).expect("open deterministic tar"));
        archive
            .unpack(&extracted)
            .expect("extract deterministic tar");

        assert_eq!(
            axe_artifact::canonical_tree_digest(&extracted).expect("digest extracted tree"),
            expected
        );

        fs::set_permissions(source.join("bin"), fs::Permissions::from_mode(0o755))
            .expect("restore source directory permissions");
        fs::set_permissions(extracted.join("bin"), fs::Permissions::from_mode(0o755))
            .expect("restore extracted directory permissions");
        fs::remove_dir_all(fixture).expect("remove package fixture");
    }
}
