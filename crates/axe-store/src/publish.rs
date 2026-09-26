use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axe_artifact::{
    Digest, KeyId, StoreIndex, StoreIndexEntry, TrustedKeys, sign_document, verify_document,
};
use rusty_s3::actions::CreateMultipartUpload;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use serde::{Deserialize, Serialize};

use crate::build_pipeline::index_json;
use crate::fs_atomic::{self, InstallMode};
use crate::keys::{load_signing_key, load_trusted_keys};

const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
const METADATA_CACHE: &str = "public, max-age=0, must-revalidate";
const INDEX_JSON_CAS_ATTEMPTS: usize = 8;
const S3_DIAGNOSTIC_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Directory,
    S3,
}

pub struct PublishOptions<'a> {
    pub workspace: &'a Path,
    pub input: &'a Path,
    pub backend: Backend,
    pub directory: Option<&'a Path>,
    pub config: &'a Path,
    pub allow_target_removal: bool,
}

pub struct DiagnoseUploadOptions<'a> {
    pub workspace: &'a Path,
    pub config: &'a Path,
    pub sizes: &'a [u64],
    pub rounds: usize,
    pub timeout_secs: u64,
}

pub struct DiagnoseUploadResult {
    pub samples: usize,
    pub failures: usize,
    pub cleanup_failures: usize,
}

#[derive(Clone, Copy, Debug)]
enum DiagnosticConnection {
    Pooled,
    Fresh,
}

impl DiagnosticConnection {
    const ALL: [Self; 2] = [Self::Pooled, Self::Fresh];

    fn name(self) -> &'static str {
        match self {
            Self::Pooled => "pooled",
            Self::Fresh => "fresh",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Pooled => 0,
            Self::Fresh => 1,
        }
    }
}

struct DiagnosticFailure {
    class: &'static str,
    detail: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PublishedIndex {
    schema_version: u32,
    generation: u64,
    ttl_secs: u64,
    tools: BTreeMap<String, PublishedIndexEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PublishedIndexEntry {
    id: String,
    aliases: Vec<String>,
    manifest_sha256: Digest,
    channels: BTreeMap<String, BTreeSet<String>>,
    synopsis: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoreConfig {
    consumer: ConsumerConfig,
    pub(crate) storage: StorageConfig,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumerConfig {
    addresses: Vec<String>,
    max_object_bytes: u64,
    max_metadata_bytes: u64,
    index_ttl_secs: u64,
    manifest_ttl_secs: u64,
    metadata_timeout_secs: u64,
    object_timeout_secs: u64,
    transient_retry_secs: u64,
    free_space_reserve_bytes: u64,
    channel: String,
    persistent_roots: Vec<PathBuf>,
    tmpfs_roots: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StorageConfig {
    pub(crate) endpoint: String,
    region: String,
    pub(crate) bucket: String,
    prefix: String,
    pub(crate) release_prefix: String,
}

pub fn publish(options: &PublishOptions<'_>) -> Result<u64, String> {
    eprintln!(
        "axe-store: publish: loading configuration from {}",
        options.config.display()
    );
    let config = read_config(options.config)?;

    let backend_name = match options.backend {
        Backend::Directory => "directory",
        Backend::S3 => "S3",
    };
    eprintln!("axe-store: publish: initializing {backend_name} backend");
    let mut sink: Box<dyn ObjectSink> = match options.backend {
        Backend::Directory => Box::new(DirectorySink::new(
            options
                .directory
                .ok_or_else(|| "--directory is required for directory backend".to_string())?,
            &config.storage.prefix,
        )?),
        Backend::S3 => Box::new(S3Sink::new(options.workspace, &config)?),
    };
    eprintln!("axe-store: publish: {backend_name} backend ready");

    publish_to(options, sink.as_mut())
}

pub fn diagnose_upload(
    options: &DiagnoseUploadOptions<'_>,
) -> Result<DiagnoseUploadResult, String> {
    if options.sizes.is_empty()
        || options.sizes.contains(&0)
        || options.rounds == 0
        || options.timeout_secs == 0
    {
        return Err("diagnostic sizes, rounds, and timeout must be positive".into());
    }
    let largest = *options.sizes.iter().max().expect("sizes are not empty");
    if largest > S3_DIAGNOSTIC_MAX_BYTES {
        return Err(format!(
            "diagnostic payload {largest} exceeds the {} byte safety limit",
            S3_DIAGNOSTIC_MAX_BYTES
        ));
    }
    let largest = usize::try_from(largest)
        .map_err(|_| "diagnostic payload does not fit this platform".to_string())?;
    let timeout = Duration::from_secs(options.timeout_secs);
    let config = read_config(options.config)?;
    let mut sink = S3Sink::new(options.workspace, &config)?;
    sink.agent = s3_agent(Some(timeout));

    eprintln!(
        "axe-store: diagnose-upload: endpoint={} bucket={} prefix={} rounds={} timeout={}s retries=disabled",
        config.storage.endpoint,
        config.storage.bucket,
        config.storage.prefix,
        options.rounds,
        options.timeout_secs
    );
    eprintln!(
        "axe-store: diagnose-upload: comparing pooled HTTP connections with a fresh agent per PUT"
    );

    run_upload_diagnostics(&sink, options.sizes, options.rounds, timeout, largest)
}

fn run_upload_diagnostics(
    sink: &S3Sink,
    sizes: &[u64],
    rounds: usize,
    timeout: Duration,
    largest: usize,
) -> Result<DiagnoseUploadResult, String> {
    let mut payload = vec![0_u8; largest];
    let mut state = 0x6a09_e667_f3bc_c909_u64;
    for byte in &mut payload {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }

    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("read system clock: {error}"))?
        .as_nanos();
    let run_id = format!("{}-{started_at}", std::process::id());
    let mut cleanup_keys = Vec::new();
    let mut samples = [0_usize; 2];
    let mut failures = [0_usize; 2];

    for round in 1..=rounds {
        for &size in sizes {
            let size_usize = usize::try_from(size)
                .map_err(|_| format!("diagnostic payload {size} does not fit this platform"))?;
            for mode in DiagnosticConnection::ALL {
                let key = format!(
                    ".diagnostics/upload/{run_id}/{}-{round}-{size}",
                    mode.name()
                );
                cleanup_keys.push(key.clone());
                let sample = mode.index();
                samples[sample] += 1;
                eprintln!(
                    "axe-store: diagnose-upload: mode={} round={round}/{rounds} size={size}: starting",
                    mode.name()
                );
                let started = Instant::now();
                let result = match mode {
                    DiagnosticConnection::Pooled => {
                        sink.diagnostic_put(&sink.agent, &key, &payload[..size_usize])
                    }
                    DiagnosticConnection::Fresh => {
                        let agent = s3_agent(Some(timeout));
                        sink.diagnostic_put(&agent, &key, &payload[..size_usize])
                    }
                };
                let elapsed = started.elapsed();
                match result {
                    Ok((status, request_id)) => {
                        let mib_per_second =
                            size as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64().max(0.000_001);
                        let request_id = request_id.as_deref().unwrap_or("-");
                        eprintln!(
                            "axe-store: diagnose-upload: mode={} round={round}/{rounds} size={size}: ok status={status} elapsed_ms={} throughput_mib_s={mib_per_second:.2} request_id={request_id}",
                            mode.name(),
                            elapsed.as_millis()
                        );
                    }
                    Err(failure) => {
                        failures[sample] += 1;
                        eprintln!(
                            "axe-store: diagnose-upload: mode={} round={round}/{rounds} size={size}: failed class={} elapsed_ms={} detail={:?}",
                            mode.name(),
                            failure.class,
                            elapsed.as_millis(),
                            failure.detail
                        );
                    }
                }
            }
        }
    }

    let cleanup_agent = s3_agent(Some(timeout));
    let mut cleanup_failures = 0_usize;
    for key in &cleanup_keys {
        if let Err(error) = sink.delete_diagnostic(&cleanup_agent, key) {
            cleanup_failures += 1;
            eprintln!("axe-store: diagnose-upload: cleanup failed for {key}: {error}");
        }
    }
    for mode in DiagnosticConnection::ALL {
        let index = mode.index();
        eprintln!(
            "axe-store: diagnose-upload: summary mode={} succeeded={} failed={}",
            mode.name(),
            samples[index] - failures[index],
            failures[index]
        );
    }
    eprintln!(
        "axe-store: diagnose-upload: cleanup attempted={} failed={cleanup_failures}",
        cleanup_keys.len()
    );

    Ok(DiagnoseUploadResult {
        samples: samples.iter().sum(),
        failures: failures.iter().sum(),
        cleanup_failures,
    })
}

pub(crate) fn read_config(path: &Path) -> Result<StoreConfig, String> {
    let config = serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("parse {}: {error}", path.display()))?;
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &StoreConfig) -> Result<(), String> {
    if config.consumer.addresses.is_empty()
        || config.consumer.max_object_bytes == 0
        || config.consumer.max_metadata_bytes == 0
        || config.consumer.index_ttl_secs == 0
        || config.consumer.manifest_ttl_secs == 0
        || config.consumer.metadata_timeout_secs == 0
        || config.consumer.object_timeout_secs == 0
        || config.consumer.transient_retry_secs == 0
        || config.consumer.free_space_reserve_bytes == 0
        || config.consumer.channel.is_empty()
        || !config.storage.endpoint.starts_with("https://")
        || config.storage.region.is_empty()
        || config.storage.bucket.is_empty()
        || config.storage.prefix.is_empty()
        || config.storage.release_prefix.is_empty()
    {
        return Err("invalid AXE Store configuration".into());
    }
    let _ = (
        &config.consumer.persistent_roots,
        &config.consumer.tmpfs_roots,
    );
    Ok(())
}

fn publish_to(options: &PublishOptions<'_>, sink: &mut dyn ObjectSink) -> Result<u64, String> {
    eprintln!("axe-store: publish: validating signing keys and staged Index");
    let signing = load_signing_key(options.workspace)?;
    let trusted = load_trusted_keys(&options.workspace.join("keys/store/trusted"))?;
    let signing_id = KeyId::for_key(&signing.verifying_key());
    if trusted.get(&signing_id).is_none() {
        return Err(format!(
            "signing key {signing_id} is not present in trusted keys"
        ));
    }

    let staged_index = read_index(&options.input.join("index.cbor.zst"), &trusted)?;
    let manifests = read_staged_manifests(options.input, &staged_index, &trusted)?;
    eprintln!("axe-store: publish: reading current remote Index");
    let remote = sink.get("index.cbor.zst")?;
    let (generation, etag) = if let Some(remote) = remote {
        let current = decode_published_index(&remote.bytes, &trusted)?;
        if !options.allow_target_removal {
            reject_target_removals(&current, &staged_index)?;
        }
        (
            current
                .generation
                .checked_add(1)
                .ok_or_else(|| "Index generation overflow".to_string())?,
            Some(remote.etag),
        )
    } else {
        (1, None)
    };

    eprintln!("axe-store: publish: preparing Index generation {generation}");

    let objects = files_under(&options.input.join("objects"))?;
    let object_count = objects.len();
    eprintln!("axe-store: publish: publishing {object_count} immutable objects");
    for (index, path) in objects.into_iter().enumerate() {
        let relative = relative_key(options.input, &path)?;
        let expected = relative
            .rsplit('/')
            .next()
            .ok_or_else(|| format!("invalid object path {relative}"))?
            .parse::<Digest>()
            .map_err(|error| error.to_string())?;
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if Digest::of(&bytes) != expected {
            return Err(format!(
                "staged object {} does not match its CAS key",
                path.display()
            ));
        }
        eprintln!(
            "axe-store: publish: object [{}/{}]: {relative} ({} bytes)",
            index + 1,
            object_count,
            bytes.len()
        );
        sink.put_immutable(&relative, &bytes)?;
    }

    let manifest_count = manifests.len();
    eprintln!("axe-store: publish: publishing {manifest_count} immutable manifests");
    for (index, (relative, bytes)) in manifests.into_iter().enumerate() {
        eprintln!(
            "axe-store: publish: manifest [{}/{}]: {relative} ({} bytes)",
            index + 1,
            manifest_count,
            bytes.len()
        );
        sink.put_immutable(&relative, &bytes)?;
    }

    let mut index = staged_index;
    index.generation = generation;
    let json = index_json(&index)?;
    let signed = sign_document(&index, &signing).map_err(|error| format!("sign Index: {error}"))?;
    let compressed = zstd::stream::encode_all(signed.as_slice(), 9)
        .map_err(|error| format!("compress Index: {error}"))?;
    eprintln!("axe-store: publish: committing Index generation {generation}");
    sink.compare_and_swap_index(&compressed, etag.as_deref())?;
    sink.put_index_json(&json, generation)?;
    eprintln!("axe-store: publish: Index generation {generation} committed as CBOR and JSON");
    Ok(generation)
}

fn read_staged_manifests(
    input: &Path,
    index: &StoreIndex,
    trusted: &TrustedKeys,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut expected = BTreeMap::new();
    for (name, entry) in &index.tools {
        let path = entry.manifest_path();
        if expected
            .insert(path.clone(), (name.as_str(), entry))
            .is_some()
        {
            return Err(format!("Index references manifest {path} more than once"));
        }
    }

    let mut manifests = Vec::with_capacity(expected.len());
    for path in files_under(&input.join("tools"))? {
        let relative = relative_key(input, &path)?;
        let (name, entry) = expected
            .remove(&relative)
            .ok_or_else(|| format!("unreferenced staged manifest {}", path.display()))?;
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        let actual = Digest::of(&bytes);
        if actual != entry.manifest_sha256 {
            return Err(format!(
                "staged manifest {} does not match Index digest: got {actual}, expected {}",
                path.display(),
                entry.manifest_sha256
            ));
        }
        let manifest: axe_artifact::ToolManifest = verify_document(&bytes, trusted)
            .map_err(|error| format!("verify staged manifest {}: {error}", path.display()))?;
        manifest
            .validate()
            .and_then(|()| manifest.validate_index_identity(name, entry))
            .map_err(|error| format!("validate staged manifest {}: {error}", path.display()))?;
        manifests.push((relative, bytes));
    }
    if let Some((path, _)) = expected.first_key_value() {
        return Err(format!("Index references missing staged manifest {path}"));
    }
    Ok(manifests)
}

fn reject_target_removals(current: &PublishedIndex, staged: &StoreIndex) -> Result<(), String> {
    let staged_by_id = staged
        .tools
        .values()
        .map(|entry| (entry.id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut removed = Vec::new();

    for entry in current.tools.values() {
        let staged_entry = staged_by_id.get(entry.id.as_str());
        for (channel, targets) in &entry.channels {
            let staged_targets = staged_entry.and_then(|entry| entry.channels.get(channel));
            for target in targets {
                if staged_targets.is_none_or(|targets| !targets.contains(target)) {
                    removed.push(format!("{} {channel} {target}", entry.id));
                }
            }
        }
    }
    if removed.is_empty() {
        return Ok(());
    }

    Err(format!(
        "new Index removes previously published targets: {}; pass --allow-target-removal to publish this change",
        removed.join(", ")
    ))
}

fn read_index(path: &Path, trusted: &TrustedKeys) -> Result<StoreIndex, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    decode_index(&bytes, trusted)
}

fn decode_index(bytes: &[u8], trusted: &TrustedKeys) -> Result<StoreIndex, String> {
    let signed =
        zstd::stream::decode_all(bytes).map_err(|error| format!("decompress Index: {error}"))?;
    let index: StoreIndex =
        verify_document(&signed, trusted).map_err(|error| format!("verify Index: {error}"))?;
    index.validate().map_err(|error| error.to_string())?;
    Ok(index)
}

fn decode_published_index(bytes: &[u8], trusted: &TrustedKeys) -> Result<PublishedIndex, String> {
    let signed =
        zstd::stream::decode_all(bytes).map_err(|error| format!("decompress Index: {error}"))?;
    let index: PublishedIndex =
        verify_document(&signed, trusted).map_err(|error| format!("verify Index: {error}"))?;

    let tools = index
        .tools
        .iter()
        .filter_map(|(name, entry)| {
            let channels = entry
                .channels
                .iter()
                .filter(|(_, targets)| !targets.is_empty())
                .map(|(channel, targets)| (channel.clone(), targets.clone()))
                .collect::<BTreeMap<_, _>>();
            (!channels.is_empty()).then(|| {
                (
                    name.clone(),
                    StoreIndexEntry {
                        id: entry.id.clone(),
                        aliases: entry.aliases.clone(),
                        manifest_sha256: entry.manifest_sha256,
                        channels,
                        synopsis: entry.synopsis.clone(),
                    },
                )
            })
        })
        .collect();
    StoreIndex {
        schema_version: index.schema_version,
        generation: index.generation,
        ttl_secs: index.ttl_secs,
        tools,
    }
    .validate()
    .map_err(|error| error.to_string())?;

    Ok(index)
}

fn should_replace_index_json(
    current: &[u8],
    desired: &[u8],
    generation: u64,
) -> Result<bool, String> {
    let Ok(current): Result<StoreIndex, _> = serde_json::from_slice(current) else {
        return Ok(true);
    };
    if current.validate().is_err() {
        return Ok(true);
    }
    if current.generation > generation {
        return Ok(false);
    }
    if current.generation == generation {
        if current
            == serde_json::from_slice(desired).map_err(|error| {
                format!("parse generated Index JSON generation {generation}: {error}")
            })?
        {
            return Ok(false);
        }
        return Err(format!(
            "Index JSON generation {generation} already exists with different contents"
        ));
    }
    Ok(true)
}

struct RemoteObject {
    bytes: Vec<u8>,
    etag: String,
}

trait ObjectSink {
    fn get(&mut self, key: &str) -> Result<Option<RemoteObject>, String>;
    fn put_immutable(&mut self, key: &str, bytes: &[u8]) -> Result<(), String>;
    fn compare_and_swap_index(&mut self, bytes: &[u8], etag: Option<&str>) -> Result<(), String>;
    fn put_index_json(&mut self, bytes: &[u8], generation: u64) -> Result<(), String>;
}

struct DirectorySink {
    root: PathBuf,
}

impl DirectorySink {
    fn new(directory: &Path, prefix: &str) -> Result<Self, String> {
        let root = directory.join(prefix.trim_matches('/'));
        fs::create_dir_all(&root).map_err(|error| format!("create {}: {error}", root.display()))?;
        Ok(Self { root })
    }

    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    fn put(&self, key: &str, bytes: &[u8], install: InstallMode) -> io::Result<()> {
        fs_atomic::write(&self.path(key), install, None, |file| file.write_all(bytes))
    }

    fn lock_index(&self) -> Result<File, String> {
        let path = self.path(".index.cbor.zst.lock");
        let lock = File::options()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        lock.lock()
            .map_err(|error| format!("lock {}: {error}", path.display()))?;
        Ok(lock)
    }
}

impl ObjectSink for DirectorySink {
    fn get(&mut self, key: &str) -> Result<Option<RemoteObject>, String> {
        let path = self.path(key);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("read {}: {error}", path.display())),
        };
        Ok(Some(RemoteObject {
            etag: format!("{}", Digest::of(&bytes)),
            bytes,
        }))
    }

    fn put_immutable(&mut self, key: &str, bytes: &[u8]) -> Result<(), String> {
        match self.put(key, bytes, InstallMode::NoReplace) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = self
                    .get(key)?
                    .ok_or_else(|| format!("immutable object {key} disappeared"))?;
                if Digest::of(&existing.bytes) == Digest::of(bytes) {
                    Ok(())
                } else {
                    Err(format!(
                        "immutable object {key} already exists with different bytes"
                    ))
                }
            }
            Err(error) => Err(format!("write {}: {error}", self.path(key).display())),
        }
    }

    fn compare_and_swap_index(&mut self, bytes: &[u8], etag: Option<&str>) -> Result<(), String> {
        let _lock = self.lock_index()?;
        let current = self.get("index.cbor.zst")?;
        match (etag, current.as_ref()) {
            (None, None) => {}
            (Some(expected), Some(current)) if current.etag == expected => {}
            _ => return Err("Index changed concurrently; refusing to overwrite it".into()),
        }
        self.put("index.cbor.zst", bytes, InstallMode::Replace)
            .map_err(|error| format!("write {}: {error}", self.path("index.cbor.zst").display()))
    }

    fn put_index_json(&mut self, bytes: &[u8], generation: u64) -> Result<(), String> {
        let _lock = self.lock_index()?;
        if let Some(current) = self.get("index.json")?
            && !should_replace_index_json(&current.bytes, bytes, generation)?
        {
            return Ok(());
        }
        self.put("index.json", bytes, InstallMode::Replace)
            .map_err(|error| format!("write {}: {error}", self.path("index.json").display()))
    }
}

const S3_SIGNED_URL_TTL: Duration = Duration::from_secs(15 * 60);
const S3_ERROR_BODY_LIMIT: u64 = 64 * 1024;
const S3_IMMUTABLE_PUT_ATTEMPTS: usize = 12;
const S3_MULTIPART_MIN_PART_BYTES: u64 = 5 * 1024 * 1024;
const S3_MULTIPART_MAX_PART_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const S3_MULTIPART_MAX_PARTS: u64 = 10_000;
const S3_RETRY_MAX_DELAY_SECS: u64 = 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MultipartCompletion {
    Completed,
    ExistingExact,
    ExistingDifferent,
}

fn multipart_part_bytes(total_bytes: u64) -> Result<usize, String> {
    let required = total_bytes.div_ceil(S3_MULTIPART_MAX_PARTS);
    let part_bytes = required.max(S3_MULTIPART_MIN_PART_BYTES);
    if part_bytes > S3_MULTIPART_MAX_PART_BYTES {
        return Err(format!(
            "S3 object is too large for multipart upload: {total_bytes} bytes"
        ));
    }
    usize::try_from(part_bytes)
        .map_err(|_| format!("S3 multipart part does not fit usize: {part_bytes} bytes"))
}

fn s3_retry_delay(attempt: usize) -> Duration {
    let exponential = 1_u64 << attempt.saturating_sub(1).min(5);
    Duration::from_secs(exponential.min(S3_RETRY_MAX_DELAY_SECS))
}

fn retryable_s3_request_error(error: &ureq::Error) -> bool {
    matches!(
        error,
        ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::Protocol(_)
            | ureq::Error::ConnectProxyFailed(_)
    )
}

fn retryable_s3_status(status: ureq::http::StatusCode) -> bool {
    status == ureq::http::StatusCode::REQUEST_TIMEOUT
        || status == ureq::http::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn s3_agent(timeout: Option<Duration>) -> ureq::Agent {
    let builder = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0);
    let builder = match timeout {
        Some(timeout) => builder.timeout_global(Some(timeout)),
        None => builder,
    };
    builder.build().into()
}

fn diagnostic_request_error_class(error: &ureq::Error) -> &'static str {
    match error {
        ureq::Error::Io(error) => match error.kind() {
            io::ErrorKind::BrokenPipe => "io/broken-pipe",
            io::ErrorKind::ConnectionReset => "io/connection-reset",
            io::ErrorKind::ConnectionAborted => "io/connection-aborted",
            io::ErrorKind::UnexpectedEof => "io/unexpected-eof",
            io::ErrorKind::TimedOut => "io/timed-out",
            _ => "io/other",
        },
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::HostNotFound => "dns",
        ureq::Error::ConnectionFailed | ureq::Error::ConnectProxyFailed(_) => "connect",
        ureq::Error::Protocol(_) => "http/protocol",
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "tls",
        _ => "request",
    }
}

pub(crate) struct S3Sink {
    agent: ureq::Agent,
    bucket: Bucket,
    credentials: Credentials,
    prefix: String,
    max_metadata_bytes: u64,
}

impl S3Sink {
    fn new(workspace: &Path, config: &StoreConfig) -> Result<Self, String> {
        Self::for_prefix(workspace, config, &config.storage.prefix)
    }

    pub(crate) fn for_prefix(
        workspace: &Path,
        config: &StoreConfig,
        prefix: &str,
    ) -> Result<Self, String> {
        Self::with_credentials(
            &config.storage,
            prefix,
            load_s3_credentials(workspace)?,
            config.consumer.max_metadata_bytes,
        )
    }

    fn with_credentials(
        config: &StorageConfig,
        prefix: &str,
        credentials: Credentials,
        max_metadata_bytes: u64,
    ) -> Result<Self, String> {
        let endpoint = config
            .endpoint
            .parse()
            .map_err(|error| format!("parse S3 endpoint: {error}"))?;
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::Path,
            config.bucket.clone(),
            config.region.clone(),
        )
        .map_err(|error| format!("configure S3 bucket: {error}"))?;
        Ok(Self {
            agent: s3_agent(None),
            bucket,
            credentials,
            prefix: prefix.trim_matches('/').to_owned(),
            max_metadata_bytes,
        })
    }

    fn key(&self, key: &str) -> String {
        format!("{}/{key}", self.prefix)
    }

    fn signed_put_url(&self, key: &str, headers: &[(&str, &str)]) -> String {
        let mut action = self.bucket.put_object(Some(&self.credentials), key);
        for &(name, value) in headers {
            action
                .headers_mut()
                .insert(name.to_owned(), value.to_owned());
        }
        action.sign(S3_SIGNED_URL_TTL).into()
    }

    fn request_error(&self, operation: &str, key: &str, error: ureq::Error) -> String {
        let detail = match error {
            ureq::Error::BadUri(_) | ureq::Error::RequireHttpsOnly(_) => {
                "invalid signed S3 URI".to_string()
            }
            error => error.to_string(),
        };
        format!("{operation} s3://{}/{key}: {detail}", self.bucket.name())
    }

    fn response_error(
        &self,
        operation: &str,
        key: &str,
        response: &mut ureq::http::Response<ureq::Body>,
    ) -> String {
        let status = response.status();
        let detail = response
            .body_mut()
            .with_config()
            .limit(S3_ERROR_BODY_LIMIT)
            .read_to_string()
            .unwrap_or_default();
        let detail = detail.trim();
        if detail.is_empty() {
            format!(
                "{operation} s3://{}/{key}: HTTP {status}",
                self.bucket.name()
            )
        } else {
            format!(
                "{operation} s3://{}/{key}: HTTP {status}: {detail:?}",
                self.bucket.name()
            )
        }
    }

    fn expect_success(
        &self,
        operation: &str,
        key: &str,
        mut response: ureq::http::Response<ureq::Body>,
    ) -> Result<(), String> {
        if response.status().is_success() {
            return Ok(());
        }
        Err(self.response_error(operation, key, &mut response))
    }

    fn diagnostic_put(
        &self,
        agent: &ureq::Agent,
        key: &str,
        bytes: &[u8],
    ) -> Result<(u16, Option<String>), DiagnosticFailure> {
        let headers = [
            ("cache-control", "no-store"),
            ("content-type", "application/octet-stream"),
        ];
        let object = self.key(key);
        let url = self.signed_put_url(&object, &headers);
        let mut request = agent.put(url.as_str());
        for &(name, value) in &headers {
            request = request.header(name, value);
        }
        let mut response = request.send(bytes).map_err(|error| DiagnosticFailure {
            class: diagnostic_request_error_class(&error),
            detail: self.request_error("diagnostic PUT", key, error),
        })?;
        let status = response.status().as_u16();
        let request_id = [
            "x-yandex-cloud-request-id",
            "x-amz-request-id",
            "x-amz-id-2",
        ]
        .iter()
        .find_map(|name| response.headers().get(*name))
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
        if response.status().is_success() {
            Ok((status, request_id))
        } else {
            Err(DiagnosticFailure {
                class: "http/status",
                detail: self.response_error("diagnostic PUT", key, &mut response),
            })
        }
    }

    fn delete_diagnostic(&self, agent: &ureq::Agent, key: &str) -> Result<(), String> {
        let object = self.key(key);
        let action = self.bucket.delete_object(Some(&self.credentials), &object);
        let url = action.sign(S3_SIGNED_URL_TTL);
        let mut response = agent
            .delete(url.as_str())
            .call()
            .map_err(|error| self.request_error("diagnostic DELETE", key, error))?;
        if response.status().is_success() || response.status() == ureq::http::StatusCode::NOT_FOUND
        {
            Ok(())
        } else {
            Err(self.response_error("diagnostic DELETE", key, &mut response))
        }
    }

    fn send_put_bytes(
        &self,
        key: &str,
        bytes: &[u8],
        headers: &[(&str, &str)],
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let object = self.key(key);
        let url = self.signed_put_url(&object, headers);
        let mut request = self.agent.put(url.as_str());
        for &(name, value) in headers {
            request = request.header(name, value);
        }
        request.send(bytes)
    }

    fn put_bytes_response(
        &self,
        key: &str,
        bytes: &[u8],
        headers: &[(&str, &str)],
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        self.send_put_bytes(key, bytes, headers)
            .map_err(|error| self.request_error("PUT", key, error))
    }

    fn send_put_file(
        &self,
        key: &str,
        path: &Path,
        headers: &[(&str, &str)],
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let object = self.key(key);
        let url = self.signed_put_url(&object, headers);
        let mut request = self.agent.put(url.as_str());
        for &(name, value) in headers {
            request = request.header(name, value);
        }
        let file = File::open(path).map_err(ureq::Error::Io)?;
        request.send(file)
    }

    fn put_file_response(
        &self,
        key: &str,
        path: &Path,
        headers: &[(&str, &str)],
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        self.send_put_file(key, path, headers)
            .map_err(|error| self.request_error("PUT", key, error))
    }

    fn retryable_put_response(
        &self,
        operation: &str,
        key: &str,
        mut send: impl FnMut() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        for attempt in 1..=S3_IMMUTABLE_PUT_ATTEMPTS {
            let failure = match send() {
                Ok(mut response)
                    if retryable_s3_status(response.status())
                        && attempt < S3_IMMUTABLE_PUT_ATTEMPTS =>
                {
                    self.response_error(operation, key, &mut response)
                }
                Ok(response) => return Ok(response),
                Err(error)
                    if retryable_s3_request_error(&error)
                        && attempt < S3_IMMUTABLE_PUT_ATTEMPTS =>
                {
                    self.request_error(operation, key, error)
                }
                Err(error) => return Err(self.request_error(operation, key, error)),
            };
            let delay = s3_retry_delay(attempt);
            eprintln!(
                "axe-store: {failure}; retrying {operation} in {}s (attempt {}/{})",
                delay.as_secs(),
                attempt + 1,
                S3_IMMUTABLE_PUT_ATTEMPTS
            );
            std::thread::sleep(delay);
        }
        unreachable!("PUT retry loop always returns")
    }

    fn create_multipart_upload(
        &self,
        key: &str,
        headers: &[(&str, &str)],
    ) -> Result<String, String> {
        let object = self.key(key);
        let mut action = self
            .bucket
            .create_multipart_upload(Some(&self.credentials), &object);
        for &(name, value) in headers {
            action
                .headers_mut()
                .insert(name.to_owned(), value.to_owned());
        }
        let url = action.sign(S3_SIGNED_URL_TTL);
        let mut request = self.agent.post(url.as_str());
        for &(name, value) in headers {
            request = request.header(name, value);
        }
        let mut response = request
            .send_empty()
            .map_err(|error| self.request_error("create multipart upload", key, error))?;
        if !response.status().is_success() {
            return Err(self.response_error("create multipart upload", key, &mut response));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(S3_ERROR_BODY_LIMIT)
            .read_to_string()
            .map_err(|error| {
                format!(
                    "read create multipart response s3://{}/{key}: {error}",
                    self.bucket.name()
                )
            })?;
        let upload = CreateMultipartUpload::parse_response(&body).map_err(|error| {
            format!(
                "parse create multipart response s3://{}/{key}: {error}",
                self.bucket.name()
            )
        })?;
        Ok(upload.upload_id().to_owned())
    }

    fn send_multipart_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: u16,
        bytes: &[u8],
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let object = self.key(key);
        let action =
            self.bucket
                .upload_part(Some(&self.credentials), &object, part_number, upload_id);
        let url = action.sign(S3_SIGNED_URL_TTL);
        self.agent.put(url.as_str()).send(bytes)
    }

    fn upload_multipart_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: u16,
        bytes: &[u8],
    ) -> Result<String, String> {
        let operation = format!("multipart part {part_number} PUT");
        let mut response = self.retryable_put_response(&operation, key, || {
            self.send_multipart_part(key, upload_id, part_number, bytes)
        })?;
        if !response.status().is_success() {
            return Err(self.response_error(&operation, key, &mut response));
        }
        response
            .headers()
            .get("etag")
            .ok_or_else(|| {
                format!(
                    "{operation} s3://{}/{key}: response has no ETag",
                    self.bucket.name()
                )
            })?
            .to_str()
            .map(str::to_owned)
            .map_err(|error| {
                format!(
                    "{operation} s3://{}/{key}: invalid ETag: {error}",
                    self.bucket.name()
                )
            })
    }

    fn send_complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        etags: &[String],
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let object = self.key(key);
        let mut action = self.bucket.complete_multipart_upload(
            Some(&self.credentials),
            &object,
            upload_id,
            etags.iter().map(String::as_str),
        );
        action
            .headers_mut()
            .insert("if-none-match".to_owned(), "*".to_owned());
        action
            .headers_mut()
            .insert("content-type".to_owned(), "application/xml".to_owned());
        let url = action.sign(S3_SIGNED_URL_TTL);
        let body = action.body();
        self.agent
            .post(url.as_str())
            .header("if-none-match", "*")
            .header("content-type", "application/xml")
            .send(body.as_bytes())
    }

    fn remote_digest_matches(
        &self,
        key: &str,
        expected_size: u64,
        expected_digest: Digest,
    ) -> Result<Option<bool>, String> {
        self.get_limited(key, expected_size.saturating_add(1))
            .map(|object| {
                object.map(|object| {
                    u64::try_from(object.bytes.len()) == Ok(expected_size)
                        && Digest::of(&object.bytes) == expected_digest
                })
            })
    }

    fn complete_multipart_immutable(
        &self,
        key: &str,
        upload_id: &str,
        etags: &[String],
        expected_size: u64,
        expected_digest: Digest,
    ) -> Result<MultipartCompletion, String> {
        for attempt in 1..=S3_IMMUTABLE_PUT_ATTEMPTS {
            let (failure, retryable) = match self.send_complete_multipart(key, upload_id, etags) {
                Ok(response) if response.status().is_success() => {
                    return Ok(MultipartCompletion::Completed);
                }
                Ok(response)
                    if response.status() == ureq::http::StatusCode::PRECONDITION_FAILED =>
                {
                    return match self.remote_digest_matches(key, expected_size, expected_digest)? {
                        Some(true) => Ok(MultipartCompletion::ExistingExact),
                        Some(false) => Ok(MultipartCompletion::ExistingDifferent),
                        None => Err(format!(
                            "immutable object {key} disappeared after multipart conflict"
                        )),
                    };
                }
                Ok(mut response) => {
                    let retryable = retryable_s3_status(response.status());
                    (
                        self.response_error("complete multipart upload", key, &mut response),
                        retryable,
                    )
                }
                Err(error) => {
                    let retryable = retryable_s3_request_error(&error);
                    (
                        self.request_error("complete multipart upload", key, error),
                        retryable,
                    )
                }
            };

            match self.remote_digest_matches(key, expected_size, expected_digest) {
                Ok(Some(true)) => return Ok(MultipartCompletion::ExistingExact),
                Ok(Some(false)) => return Ok(MultipartCompletion::ExistingDifferent),
                Ok(None) => {}
                Err(reconcile_error) if !retryable || attempt == S3_IMMUTABLE_PUT_ATTEMPTS => {
                    return Err(format!(
                        "{failure}; reconciliation failed: {reconcile_error}"
                    ));
                }
                Err(_) => {}
            }

            if !retryable || attempt == S3_IMMUTABLE_PUT_ATTEMPTS {
                return Err(failure);
            }
            let delay = s3_retry_delay(attempt);
            eprintln!(
                "axe-store: {failure}; retrying multipart completion in {}s (attempt {}/{})",
                delay.as_secs(),
                attempt + 1,
                S3_IMMUTABLE_PUT_ATTEMPTS
            );
            std::thread::sleep(delay);
        }
        unreachable!("multipart completion retry loop always returns")
    }

    fn abort_multipart_upload(&self, key: &str, upload_id: &str) {
        let object = self.key(key);
        let action =
            self.bucket
                .abort_multipart_upload(Some(&self.credentials), &object, upload_id);
        let url = action.sign(S3_SIGNED_URL_TTL);
        match self.agent.delete(url.as_str()).call() {
            Ok(response)
                if response.status().is_success()
                    || response.status() == ureq::http::StatusCode::NOT_FOUND => {}
            Ok(mut response) => eprintln!(
                "axe-store: warning: {}",
                self.response_error("abort multipart upload", key, &mut response)
            ),
            Err(error) => eprintln!(
                "axe-store: warning: {}",
                self.request_error("abort multipart upload", key, error)
            ),
        }
    }

    fn finish_multipart(
        &self,
        key: &str,
        upload_id: &str,
        result: Result<MultipartCompletion, String>,
        different_bytes: impl FnOnce() -> String,
    ) -> Result<(), String> {
        if !matches!(result, Ok(MultipartCompletion::Completed)) {
            self.abort_multipart_upload(key, upload_id);
        }
        match result {
            Ok(MultipartCompletion::Completed | MultipartCompletion::ExistingExact) => Ok(()),
            Ok(MultipartCompletion::ExistingDifferent) => Err(different_bytes()),
            Err(error) => Err(error),
        }
    }

    fn put_multipart_bytes_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        digest: Digest,
        headers: &[(&str, &str)],
    ) -> Result<(), String> {
        let expected_size = u64::try_from(bytes.len())
            .map_err(|_| format!("immutable object {key} is too large"))?;
        let part_bytes = multipart_part_bytes(expected_size)?;
        let upload_id = self.create_multipart_upload(key, headers)?;
        let result = (|| {
            let mut etags = Vec::with_capacity(bytes.len().div_ceil(part_bytes));
            for (index, part) in bytes.chunks(part_bytes).enumerate() {
                let part_number = u16::try_from(index + 1)
                    .map_err(|_| format!("immutable object {key} has too many multipart parts"))?;
                etags.push(self.upload_multipart_part(key, &upload_id, part_number, part)?);
            }
            self.complete_multipart_immutable(key, &upload_id, &etags, expected_size, digest)
        })();
        self.finish_multipart(key, &upload_id, result, || {
            format!("immutable object {key} already exists with different bytes")
        })
    }

    fn put_multipart_file_immutable(
        &self,
        key: &str,
        path: &Path,
        expected_size: u64,
        digest: Digest,
        headers: &[(&str, &str)],
    ) -> Result<(), String> {
        let part_bytes = multipart_part_bytes(expected_size)?;
        let upload_id = self.create_multipart_upload(key, headers)?;
        let result = (|| {
            let mut file =
                File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
            let mut buffer = vec![0_u8; part_bytes];
            let mut remaining = expected_size;
            let mut part_number = 1_u16;
            let mut etags = Vec::with_capacity(
                usize::try_from(expected_size.div_ceil(part_bytes as u64)).unwrap_or(0),
            );
            while remaining != 0 {
                let count = usize::try_from(remaining.min(part_bytes as u64))
                    .expect("multipart read size fits usize");
                file.read_exact(&mut buffer[..count])
                    .map_err(|error| format!("read {}: {error}", path.display()))?;
                etags.push(self.upload_multipart_part(
                    key,
                    &upload_id,
                    part_number,
                    &buffer[..count],
                )?);
                remaining -= count as u64;
                part_number = part_number
                    .checked_add(1)
                    .ok_or_else(|| format!("immutable file {key} has too many multipart parts"))?;
            }
            self.complete_multipart_immutable(key, &upload_id, &etags, expected_size, digest)
        })();
        self.finish_multipart(key, &upload_id, result, || {
            format!("immutable Axe release {key} already exists with different bytes")
        })
    }

    fn get_limited(&self, key: &str, limit: u64) -> Result<Option<RemoteObject>, String> {
        let object = self.key(key);
        let action = self.bucket.get_object(Some(&self.credentials), &object);
        let url = action.sign(S3_SIGNED_URL_TTL);
        let mut response = self
            .agent
            .get(url.as_str())
            .call()
            .map_err(|error| self.request_error("GET", key, error))?;

        if response.status() == ureq::http::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(self.response_error("GET", key, &mut response));
        }

        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|error| format!("read s3://{}/{key}: {error}", self.bucket.name()))?;
        Ok(Some(RemoteObject { bytes, etag }))
    }

    pub(crate) fn put_file(
        &self,
        key: &str,
        path: &Path,
        cache_control: &str,
    ) -> Result<(), String> {
        let headers = [
            ("cache-control", cache_control),
            ("content-type", "application/octet-stream"),
        ];
        let response = self.put_file_response(key, path, &headers)?;
        self.expect_success("PUT", key, response)
    }

    pub(crate) fn put_file_immutable(
        &mut self,
        key: &str,
        path: &Path,
        digest: Digest,
        cache_control: &str,
    ) -> Result<(), String> {
        let object_headers = [
            ("cache-control", cache_control),
            ("content-type", "application/octet-stream"),
        ];
        let expected_size = fs::metadata(path)
            .map_err(|error| format!("inspect {}: {error}", path.display()))?
            .len();
        if expected_size > S3_MULTIPART_MIN_PART_BYTES {
            return self.put_multipart_file_immutable(
                key,
                path,
                expected_size,
                digest,
                &object_headers,
            );
        }
        let headers = [object_headers[0], object_headers[1], ("if-none-match", "*")];
        let mut response = self.retryable_put_response("immutable PUT", key, || {
            self.send_put_file(key, path, &headers)
        })?;
        if response.status().is_success() {
            return Ok(());
        }
        if response.status() != ureq::http::StatusCode::PRECONDITION_FAILED {
            return Err(self.response_error("conditional PUT", key, &mut response));
        }

        let limit = expected_size.saturating_add(1);
        let existing = self
            .get_limited(key, limit)?
            .ok_or_else(|| format!("immutable Axe release {key} disappeared after conflict"))?;
        if Digest::of(&existing.bytes) == digest {
            Ok(())
        } else {
            Err(format!(
                "immutable Axe release {key} already exists with different bytes"
            ))
        }
    }

    pub(crate) fn verify_file_immutable(
        &self,
        key: &str,
        path: &Path,
        digest: Digest,
    ) -> Result<(), String> {
        let size = fs::metadata(path)
            .map_err(|error| format!("inspect {}: {error}", path.display()))?
            .len();
        match self.remote_digest_matches(key, size, digest)? {
            Some(true) => Ok(()),
            Some(false) => Err(format!(
                "immutable Axe release {key} already exists with different bytes"
            )),
            None => Err(format!("immutable Axe release {key} does not exist")),
        }
    }

    #[cfg(test)]
    fn create_bucket(&self) -> Result<(), String> {
        let action = self.bucket.create_bucket(&self.credentials);
        let url = action.sign(S3_SIGNED_URL_TTL);
        let response = self
            .agent
            .put(url.as_str())
            .send_empty()
            .map_err(|error| self.request_error("PUT", "", error))?;
        self.expect_success("PUT", "", response)
    }

    #[cfg(test)]
    fn head_header(&self, key: &str, name: &str) -> Result<Option<String>, String> {
        let object = self.key(key);
        let action = self.bucket.head_object(Some(&self.credentials), &object);
        let url = action.sign(S3_SIGNED_URL_TTL);
        let mut response = self
            .agent
            .head(url.as_str())
            .call()
            .map_err(|error| self.request_error("HEAD", key, error))?;
        if !response.status().is_success() {
            return Err(self.response_error("HEAD", key, &mut response));
        }

        response
            .headers()
            .get(name)
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .map_err(|error| format!("invalid {name} response header: {error}"))
            })
            .transpose()
    }
}

impl ObjectSink for S3Sink {
    fn get(&mut self, key: &str) -> Result<Option<RemoteObject>, String> {
        self.get_limited(key, self.max_metadata_bytes)
    }

    fn put_immutable(&mut self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let digest = Digest::of(bytes);
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > S3_MULTIPART_MIN_PART_BYTES {
            let headers = [("cache-control", IMMUTABLE_CACHE)];
            return self.put_multipart_bytes_immutable(key, bytes, digest, &headers);
        }
        let headers = [("cache-control", IMMUTABLE_CACHE), ("if-none-match", "*")];
        let mut response = self.retryable_put_response("immutable PUT", key, || {
            self.send_put_bytes(key, bytes, &headers)
        })?;
        if response.status().is_success() {
            return Ok(());
        }
        if response.status() != ureq::http::StatusCode::PRECONDITION_FAILED {
            return Err(self.response_error("conditional PUT", key, &mut response));
        }

        let existing = self
            .get_limited(
                key,
                u64::try_from(bytes.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )?
            .ok_or_else(|| format!("immutable object {key} disappeared after conflict"))?;
        if Digest::of(&existing.bytes) == digest {
            Ok(())
        } else {
            Err(format!(
                "immutable object {key} already exists with different bytes"
            ))
        }
    }

    fn compare_and_swap_index(&mut self, bytes: &[u8], etag: Option<&str>) -> Result<(), String> {
        let condition = match etag {
            Some(etag) => ("if-match", etag),
            None => ("if-none-match", "*"),
        };
        let headers = [("cache-control", METADATA_CACHE), condition];
        let mut response = self.put_bytes_response("index.cbor.zst", bytes, &headers)?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(self.response_error("conditional Index PUT", "index.cbor.zst", &mut response))
    }

    fn put_index_json(&mut self, bytes: &[u8], generation: u64) -> Result<(), String> {
        for _ in 0..INDEX_JSON_CAS_ATTEMPTS {
            let current = self.get("index.json")?;
            if let Some(current) = current.as_ref()
                && !should_replace_index_json(&current.bytes, bytes, generation)?
            {
                return Ok(());
            }

            let condition = match current.as_ref() {
                Some(current) => ("if-match", current.etag.as_str()),
                None => ("if-none-match", "*"),
            };
            let headers = [
                ("cache-control", METADATA_CACHE),
                ("content-type", "application/json"),
                condition,
            ];
            let mut response = self.put_bytes_response("index.json", bytes, &headers)?;
            if response.status().is_success() {
                return Ok(());
            }
            if response.status() != ureq::http::StatusCode::PRECONDITION_FAILED {
                return Err(self.response_error(
                    "conditional Index JSON PUT",
                    "index.json",
                    &mut response,
                ));
            }
        }
        Err("Index JSON changed concurrently too many times; refusing to overwrite it".into())
    }
}

fn load_s3_credentials(workspace: &Path) -> Result<Credentials, String> {
    let access = std::env::var("AWS_ACCESS_KEY_ID")
        .ok()
        .filter(|value| !value.is_empty());
    let secret = std::env::var("AWS_SECRET_ACCESS_KEY")
        .ok()
        .filter(|value| !value.is_empty());
    let (access, secret) = match (access, secret) {
        (Some(access), Some(secret)) => (access, secret),
        (None, None) => (
            fs::read_to_string(workspace.join("keys/store/s3_access_key_id"))
                .map_err(|error| format!("read keys/store/s3_access_key_id: {error}"))?
                .trim()
                .to_owned(),
            fs::read_to_string(workspace.join("keys/store/s3_secret_access_key"))
                .map_err(|error| format!("read keys/store/s3_secret_access_key: {error}"))?
                .trim()
                .to_owned(),
        ),
        _ => return Err("AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY must be set together".into()),
    };
    if access.is_empty() || secret.is_empty() {
        return Err("S3 credentials must not be empty".into());
    }

    Ok(match std::env::var("AWS_SESSION_TOKEN") {
        Ok(token) if !token.is_empty() => Credentials::new_with_token(access, secret, token),
        _ => Credentials::new(access, secret),
    })
}

fn files_under(root: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read {}: {error}", root.display())),
    };
    let mut files = Vec::new();
    collect_file_entries(entries, &mut files).map_err(|error| error.to_string())?;
    files.sort_unstable();
    Ok(files)
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = fs::read_dir(directory).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read {}: {error}", directory.display()),
        )
    })?;
    collect_file_entries(entries, files)
}

fn collect_file_entries(entries: fs::ReadDir, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), files)?;
        } else {
            files.push(entry.path());
        }
    }
    Ok(())
}
fn relative_key(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map_err(|error| error.to_string())?
        .to_str()
        .ok_or_else(|| format!("AXE Store path {} is not UTF-8", path.display()))
        .map(|path| path.replace(std::path::MAIN_SEPARATOR, "/"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    use testcontainers::{
        GenericImage, ImageExt,
        core::{IntoContainerPort, WaitFor},
        runners::SyncRunner,
    };

    const MINIO_IMAGE_TAG: &str = "RELEASE.2025-04-22T22-12-26Z";
    const ACCESS_KEY: &str = "axe-store-test";
    const SECRET_KEY: &str = "axe-store-test-secret";
    const BUCKET: &str = "axe-store";

    fn sink(endpoint: String) -> S3Sink {
        S3Sink::with_credentials(
            &StorageConfig {
                endpoint,
                region: "us-east-1".into(),
                bucket: BUCKET.into(),
                prefix: "tools".into(),
                release_prefix: "axe".into(),
            },
            "tools",
            Credentials::new(ACCESS_KEY, SECRET_KEY),
            1024 * 1024,
        )
        .expect("create S3 sink")
    }

    fn read_http_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set request read timeout");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).expect("read HTTP request");
            assert!(count > 0, "HTTP request ended before its body");
            request.extend_from_slice(&buffer[..count]);

            let Some(header_end) = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|offset| offset + 4)
            else {
                continue;
            };
            let headers =
                std::str::from_utf8(&request[..header_end]).expect("HTTP headers are UTF-8");
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("valid Content-Length"))
                })
                .unwrap_or(0);
            if request.len() >= header_end + content_length {
                let request_line = headers
                    .lines()
                    .next()
                    .expect("HTTP request line")
                    .to_owned();
                return (
                    request_line,
                    request[header_end..header_end + content_length].to_vec(),
                );
            }
        }
    }

    #[test]
    fn upload_diagnostics_continue_after_failure_and_clean_every_key() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test S3 server");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("read test S3 address")
        );
        let server = std::thread::spawn(move || {
            let (mut pooled, _) = listener.accept().expect("accept pooled diagnostic PUT");
            let (request, body) = read_http_request(&mut pooled);
            assert!(request.starts_with("PUT "));
            assert!(request.contains("/.diagnostics/upload/"));
            assert!(request.contains("/pooled-1-1024"));
            assert_eq!(body.len(), 1024);
            drop(pooled);

            let (mut fresh, _) = listener.accept().expect("accept fresh diagnostic PUT");
            let (request, body) = read_http_request(&mut fresh);
            assert!(request.starts_with("PUT "));
            assert!(request.contains("/fresh-1-1024"));
            assert_eq!(body.len(), 1024);
            fresh
                .write_all(
                    b"HTTP/1.1 200 OK\r\nX-Amz-Request-Id: probe-1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write diagnostic success");

            for mode in ["pooled", "fresh"] {
                let (mut cleanup, _) = listener.accept().expect("accept diagnostic cleanup");
                let (request, body) = read_http_request(&mut cleanup);
                assert!(request.starts_with("DELETE "));
                assert!(request.contains("/.diagnostics/upload/"));
                assert!(request.contains(&format!("/{mode}-1-1024")));
                assert!(body.is_empty());
                cleanup
                    .write_all(
                        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .expect("write diagnostic cleanup response");
            }
        });

        let sink = sink(endpoint);
        let result = run_upload_diagnostics(&sink, &[1024], 1, Duration::from_secs(5), 1024)
            .expect("diagnostics must finish after a failed probe");
        assert_eq!(result.samples, 2);
        assert_eq!(result.failures, 1);
        assert_eq!(result.cleanup_failures, 0);
        server.join().expect("join test S3 server");
    }

    #[test]
    fn immutable_put_retries_an_ambiguous_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test S3 server");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("read test S3 address")
        );
        let object = b"immutable object".to_vec();
        let served_object = object.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("accept first PUT");
            let (request, body) = read_http_request(&mut first);
            assert!(request.starts_with("PUT "));
            assert_eq!(body, served_object);
            drop(first);

            let (mut second, _) = listener.accept().expect("accept retried PUT");
            let (request, body) = read_http_request(&mut second);
            assert!(request.starts_with("PUT "));
            assert_eq!(body, served_object);
            second
                .write_all(
                    b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write conflict response");

            let (mut third, _) = listener.accept().expect("accept reconciliation GET");
            let (request, body) = read_http_request(&mut third);
            assert!(request.starts_with("GET "));
            assert!(body.is_empty());
            write!(
                third,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"test\"\r\nConnection: close\r\n\r\n",
                served_object.len()
            )
            .expect("write reconciliation headers");
            third
                .write_all(&served_object)
                .expect("write reconciliation body");
        });

        let mut sink = sink(endpoint);
        let key = "objects/sha256/00/test";
        sink.put_immutable(key, &object)
            .expect("ambiguous successful PUT must reconcile after retry");
        server.join().expect("join test S3 server");
    }

    #[test]
    fn multipart_put_retries_a_disconnected_part() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test S3 server");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("read test S3 address")
        );
        let object = vec![0x5a; usize::try_from(S3_MULTIPART_MIN_PART_BYTES).unwrap() + 1024];
        let served_object = object.clone();
        let server = std::thread::spawn(move || {
            let (mut create, _) = listener.accept().expect("accept multipart creation");
            let (request, body) = read_http_request(&mut create);
            assert!(request.starts_with("POST "));
            assert!(request.contains("uploads=1"));
            assert!(body.is_empty());
            let response = concat!(
                "<InitiateMultipartUploadResult ",
                "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
                "<UploadId>upload-1</UploadId>",
                "</InitiateMultipartUploadResult>"
            );
            write!(
                create,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .expect("write multipart creation response");

            let (mut first_part, _) = listener.accept().expect("accept first part PUT");
            let (request, body) = read_http_request(&mut first_part);
            assert!(request.starts_with("PUT "));
            assert!(request.contains("partNumber=1"));
            assert_eq!(
                body,
                served_object[..usize::try_from(S3_MULTIPART_MIN_PART_BYTES).unwrap()]
            );
            drop(first_part);

            let (mut retried_part, _) = listener.accept().expect("accept retried part PUT");
            let (request, body) = read_http_request(&mut retried_part);
            assert!(request.starts_with("PUT "));
            assert!(request.contains("partNumber=1"));
            assert_eq!(
                body,
                served_object[..usize::try_from(S3_MULTIPART_MIN_PART_BYTES).unwrap()]
            );
            retried_part
                .write_all(
                    b"HTTP/1.1 200 OK\r\nETag: \"part-1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write first part response");

            let (mut second_part, _) = listener.accept().expect("accept second part PUT");
            let (request, body) = read_http_request(&mut second_part);
            assert!(request.starts_with("PUT "));
            assert!(request.contains("partNumber=2"));
            assert_eq!(
                body,
                served_object[usize::try_from(S3_MULTIPART_MIN_PART_BYTES).unwrap()..]
            );
            second_part
                .write_all(
                    b"HTTP/1.1 200 OK\r\nETag: \"part-2\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write second part response");

            let (mut complete, _) = listener.accept().expect("accept multipart completion");
            let (request, body) = read_http_request(&mut complete);
            assert!(request.starts_with("POST "));
            assert!(request.contains("uploadId=upload-1"));
            let body = std::str::from_utf8(&body).expect("completion XML is UTF-8");
            assert!(body.contains("<ETag>&quot;part-1&quot;</ETag>"));
            assert!(body.contains("<PartNumber>1</PartNumber>"));
            assert!(body.contains("<ETag>&quot;part-2&quot;</ETag>"));
            assert!(body.contains("<PartNumber>2</PartNumber>"));
            complete
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("write multipart completion response");
        });

        let mut sink = sink(endpoint);
        let key = "objects/sha256/00/large";
        sink.put_immutable(key, &object)
            .expect("multipart PUT must retry only the disconnected part");
        server.join().expect("join test S3 server");
    }

    #[test]
    fn target_removal_requires_explicit_permission() {
        let staged = StoreIndex {
            schema_version: axe_artifact::SCHEMA_VERSION,
            generation: 1,
            ttl_secs: 3_600,
            tools: BTreeMap::from([(
                "tool".into(),
                StoreIndexEntry {
                    id: "test/tool".into(),
                    aliases: Vec::new(),
                    manifest_sha256: Digest([3; 32]),
                    channels: BTreeMap::from([(
                        "stable".into(),
                        BTreeSet::from([axe_artifact::Target::X86_64Linux.to_string()]),
                    )]),
                    synopsis: None,
                },
            )]),
        };
        let current = PublishedIndex {
            schema_version: axe_artifact::SCHEMA_VERSION,
            generation: 1,
            ttl_secs: 3_600,
            tools: BTreeMap::from([(
                "tool".into(),
                PublishedIndexEntry {
                    id: "test/tool".into(),
                    aliases: Vec::new(),
                    manifest_sha256: Digest([3; 32]),
                    channels: BTreeMap::from([(
                        "stable".into(),
                        BTreeSet::from(["retired-target".into(), "x86_64-linux".into()]),
                    )]),
                    synopsis: None,
                },
            )]),
        };

        let error = reject_target_removals(&current, &staged)
            .expect_err("retired target removal must be rejected");
        assert!(error.contains("test/tool stable retired-target"));
    }

    #[test]
    fn published_index_decoder_accepts_retired_targets() {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let mut trusted = TrustedKeys::new();
        trusted.insert(signing.verifying_key());
        let index = PublishedIndex {
            schema_version: axe_artifact::SCHEMA_VERSION,
            generation: 7,
            ttl_secs: 3_600,
            tools: BTreeMap::from([(
                "tool".into(),
                PublishedIndexEntry {
                    id: "test/tool".into(),
                    aliases: Vec::new(),
                    manifest_sha256: Digest([3; 32]),
                    channels: BTreeMap::from([(
                        "stable".into(),
                        BTreeSet::from(["retired-target".into(), "x86_64-linux".into()]),
                    )]),
                    synopsis: None,
                },
            )]),
        };
        let signed = sign_document(&index, &signing).expect("sign published Index");
        let compressed =
            zstd::stream::encode_all(signed.as_slice(), 1).expect("compress published Index");

        let decoded =
            decode_published_index(&compressed, &trusted).expect("decode published Index");
        assert_eq!(decoded.generation, 7);
        assert!(
            decoded.tools["tool"].channels["stable"].contains("retired-target"),
            "retired target must remain visible to removal checks"
        );
    }

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new(signing: &ed25519_dalek::SigningKey) -> Self {
            let root = std::env::temp_dir().join(format!(
                "axe-store-publish-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after epoch")
                    .as_nanos()
            ));
            let keys = root.join("keys/store");
            fs::create_dir_all(keys.join("trusted")).expect("create test keys");
            let hex = |bytes: &[u8]| {
                bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            };
            fs::write(keys.join("signing.key"), hex(&signing.to_bytes()))
                .expect("write test signing key");
            let id = KeyId::for_key(&signing.verifying_key());
            fs::write(
                keys.join(format!("trusted/{id}.pub")),
                hex(signing.verifying_key().as_bytes()),
            )
            .expect("write trusted test key");
            Self(root)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove test workspace");
        }
    }

    fn stage_test_index(
        workspace: &Path,
        signing: &ed25519_dalek::SigningKey,
        name: &str,
        variant: u8,
    ) -> (PathBuf, StoreIndex, Vec<u8>) {
        let input = workspace.join(name);
        fs::create_dir(&input).expect("create test stage");
        let version = format!("1.0.{variant}");
        let manifest = axe_artifact::ToolManifest {
            schema_version: axe_artifact::SCHEMA_VERSION,
            id: "test/tool".into(),
            name: "tool".into(),
            ttl_secs: 60,
            channels: BTreeMap::from([("stable".into(), version.clone())]),
            versions: BTreeMap::from([(
                version,
                axe_artifact::ToolVersion {
                    targets: BTreeMap::from([(
                        axe_artifact::Target::X86_64Linux.to_string(),
                        axe_artifact::Artifact {
                            kind: axe_artifact::ArtifactKind::SingleBinary,
                            object_sha256: Digest([variant; 32]),
                            compressed_size: 1,
                            unpacked_size: 1,
                            unpacked_sha256: Digest([variant; 32]),
                            fully_static: true,
                        },
                    )]),
                },
            )]),
        };
        let bytes = sign_document(&manifest, signing).expect("sign staged manifest");
        let entry = StoreIndexEntry {
            id: manifest.id.clone(),
            aliases: Vec::new(),
            manifest_sha256: Digest::of(&bytes),
            channels: BTreeMap::from([(
                "stable".into(),
                BTreeSet::from([axe_artifact::Target::X86_64Linux.to_string()]),
            )]),
            synopsis: None,
        };
        let path = input.join(entry.manifest_path());
        fs::create_dir_all(path.parent().unwrap()).expect("create manifest directory");
        fs::write(path, &bytes).expect("stage manifest");
        let index = StoreIndex {
            schema_version: axe_artifact::SCHEMA_VERSION,
            generation: 1,
            ttl_secs: 60,
            tools: BTreeMap::from([("tool".into(), entry)]),
        };
        let signed = sign_document(&index, signing).expect("sign staged Index");
        fs::write(
            input.join("index.cbor.zst"),
            zstd::stream::encode_all(signed.as_slice(), 1).expect("compress staged Index"),
        )
        .expect("stage Index");
        (input, index, bytes)
    }

    #[test]
    fn publication_rejects_tampered_and_unreferenced_manifests_before_upload() {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let workspace = TempWorkspace::new(&signing);
        let (input, index, bytes) = stage_test_index(&workspace.0, &signing, "stage", 1);
        let path = input.join(index.tools["tool"].manifest_path());
        let mut sink = DirectorySink::new(&workspace.0.join("published"), "tools").unwrap();
        let options = PublishOptions {
            workspace: &workspace.0,
            input: &input,
            backend: Backend::Directory,
            directory: None,
            config: Path::new("unused"),
            allow_target_removal: false,
        };

        fs::write(&path, b"tampered").expect("corrupt signed manifest");
        assert!(
            publish_to(&options, &mut sink)
                .unwrap_err()
                .contains("Index digest")
        );
        assert!(sink.get("index.cbor.zst").unwrap().is_none());
        fs::write(&path, &bytes).expect("restore signed manifest");
        let legacy = input.join("tools/test/tool/manifest.cbor");
        fs::write(&legacy, &bytes).expect("stage legacy mutable manifest");
        assert!(
            publish_to(&options, &mut sink)
                .unwrap_err()
                .contains("unreferenced staged manifest")
        );
        assert!(sink.get("index.cbor.zst").unwrap().is_none());

        fs::remove_file(&legacy).expect("remove legacy manifest");
        fs::remove_file(&path).expect("remove original manifest");
        let mut trusted = TrustedKeys::new();
        trusted.insert(signing.verifying_key());
        let mut other: axe_artifact::ToolManifest =
            verify_document(&bytes, &trusted).expect("decode original manifest");
        other.name = "different".into();
        let signed_other = sign_document(&other, &signing).expect("sign mismatched identity");
        let mut mismatched = index;
        mismatched.tools.get_mut("tool").unwrap().manifest_sha256 = Digest::of(&signed_other);
        let path = input.join(mismatched.tools["tool"].manifest_path());
        fs::write(path, signed_other).expect("stage correctly hashed, mismatched manifest");
        let signed_index = sign_document(&mismatched, &signing).expect("sign mismatched Index");
        fs::write(
            input.join("index.cbor.zst"),
            zstd::stream::encode_all(signed_index.as_slice(), 1).expect("compress Index"),
        )
        .expect("stage mismatched Index");
        assert!(
            publish_to(&options, &mut sink)
                .unwrap_err()
                .contains("identity mismatch")
        );
        assert!(sink.get("index.cbor.zst").unwrap().is_none());
    }

    struct RacingSink<'a> {
        inner: DirectorySink,
        winner: PublishOptions<'a>,
    }

    impl ObjectSink for RacingSink<'_> {
        fn get(&mut self, key: &str) -> Result<Option<RemoteObject>, String> {
            self.inner.get(key)
        }

        fn put_immutable(&mut self, key: &str, bytes: &[u8]) -> Result<(), String> {
            self.inner.put_immutable(key, bytes)
        }

        fn compare_and_swap_index(
            &mut self,
            bytes: &[u8],
            etag: Option<&str>,
        ) -> Result<(), String> {
            publish_to(&self.winner, &mut self.inner)?;
            self.inner.compare_and_swap_index(bytes, etag)
        }

        fn put_index_json(&mut self, bytes: &[u8], generation: u64) -> Result<(), String> {
            self.inner.put_index_json(bytes, generation)
        }
    }

    #[test]
    fn concurrent_publish_loser_cannot_change_winners_committed_manifest() {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
        let workspace = TempWorkspace::new(&signing);
        let (first, _, _) = stage_test_index(&workspace.0, &signing, "first", 1);
        let (winner, winning_index, winning_bytes) =
            stage_test_index(&workspace.0, &signing, "winner", 2);
        let (loser, losing_index, _) = stage_test_index(&workspace.0, &signing, "loser", 3);
        let options = |input| PublishOptions {
            workspace: &workspace.0,
            input,
            backend: Backend::Directory,
            directory: None,
            config: Path::new("unused"),
            allow_target_removal: false,
        };
        let mut sink = DirectorySink::new(&workspace.0.join("published"), "tools").unwrap();
        assert_eq!(publish_to(&options(&first), &mut sink).unwrap(), 1);
        let mut race = RacingSink {
            inner: sink,
            winner: options(&winner),
        };
        assert!(
            publish_to(&options(&loser), &mut race)
                .unwrap_err()
                .contains("Index changed concurrently")
        );
        let mut trusted = TrustedKeys::new();
        trusted.insert(signing.verifying_key());
        let published = decode_index(
            &race
                .get("index.cbor.zst")
                .unwrap()
                .expect("published Index")
                .bytes,
            &trusted,
        )
        .expect("verified winner Index");
        assert_eq!(published.generation, 2);
        assert_eq!(
            published.tools["tool"].manifest_sha256,
            winning_index.tools["tool"].manifest_sha256
        );
        assert_ne!(
            published.tools["tool"].manifest_sha256,
            losing_index.tools["tool"].manifest_sha256
        );
        let immutable = race
            .get(&published.tools["tool"].manifest_path())
            .unwrap()
            .expect("winner signed manifest")
            .bytes;
        assert_eq!(immutable, winning_bytes);
        let manifest: axe_artifact::ToolManifest =
            verify_document(&immutable, &trusted).expect("verify winner manifest");
        manifest
            .validate_index_identity("tool", &published.tools["tool"])
            .expect("winner Index references winner's manifest");
        assert_eq!(manifest.channels["stable"], "1.0.2");
    }

    #[test]
    #[ignore = "requires a Docker-compatible container runtime"]
    fn minio_enforces_immutable_objects_and_index_cas() {
        let container = GenericImage::new("minio/minio", MINIO_IMAGE_TAG)
            .with_exposed_port(9000.tcp())
            .with_wait_for(WaitFor::message_on_stderr("API:"))
            .with_env_var("MINIO_ROOT_USER", ACCESS_KEY)
            .with_env_var("MINIO_ROOT_PASSWORD", SECRET_KEY)
            .with_cmd(["server", "/data", "--address", ":9000"])
            .start()
            .expect("start MinIO");
        let host = container.get_host().expect("MinIO host");
        let port = container
            .get_host_port_ipv4(9000.tcp())
            .expect("mapped MinIO port");
        let endpoint = format!("http://{host}:{port}");

        let mut first = sink(endpoint.clone());
        first.create_bucket().expect("create test bucket");

        let object_key =
            "objects/sha256/aa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let immutable = vec![0x5a; usize::try_from(S3_MULTIPART_MIN_PART_BYTES).unwrap() + 1024];
        let different = vec![0xa5; usize::try_from(S3_MULTIPART_MIN_PART_BYTES).unwrap() + 1024];
        first
            .put_immutable(object_key, &immutable)
            .expect("create immutable multipart object");
        first
            .put_immutable(object_key, &immutable)
            .expect("idempotent immutable multipart object");
        assert!(
            first.put_immutable(object_key, &different).is_err(),
            "a multipart CAS key must never be overwritten with different bytes"
        );
        assert_eq!(
            first
                .head_header(object_key, "cache-control")
                .expect("inspect immutable object")
                .as_deref(),
            Some(IMMUTABLE_CACHE)
        );

        let manifest = b"signed manifest";
        let manifest_key = format!("tools/test/manifests/{}.cbor", Digest::of(manifest));
        first
            .put_immutable(&manifest_key, manifest)
            .expect("create immutable manifest");
        first
            .put_immutable(&manifest_key, manifest)
            .expect("idempotent immutable manifest");
        assert!(
            first.put_immutable(&manifest_key, b"changed").is_err(),
            "a concurrent publisher cannot overwrite committed manifest bytes"
        );
        assert_eq!(
            first
                .head_header(&manifest_key, "cache-control")
                .expect("inspect manifest")
                .as_deref(),
            Some(IMMUTABLE_CACHE)
        );

        let human_index = |generation| {
            index_json(&StoreIndex {
                schema_version: axe_artifact::SCHEMA_VERSION,
                generation,
                ttl_secs: 3_600,
                tools: std::collections::BTreeMap::new(),
            })
            .expect("serialize human-readable Index")
        };
        let generation_two_json = human_index(2);
        first
            .put_index_json(&generation_two_json, 2)
            .expect("create JSON Index");
        first
            .put_index_json(&human_index(1), 1)
            .expect("ignore stale JSON Index");
        assert_eq!(
            first
                .get("index.json")
                .expect("read JSON Index")
                .expect("JSON Index exists")
                .bytes,
            generation_two_json
        );
        assert_eq!(
            first
                .head_header("index.json", "cache-control")
                .expect("inspect JSON Index cache control")
                .as_deref(),
            Some(METADATA_CACHE)
        );
        assert_eq!(
            first
                .head_header("index.json", "content-type")
                .expect("inspect JSON Index content type")
                .as_deref(),
            Some("application/json")
        );

        first
            .compare_and_swap_index(b"generation-1", None)
            .expect("create first Index");
        assert!(
            first.compare_and_swap_index(b"lost-create", None).is_err(),
            "If-None-Match must reject a second generation-1 publisher"
        );

        let stale_etag = first
            .get("index.cbor.zst")
            .expect("read Index")
            .expect("Index exists")
            .etag;
        let mut second = sink(endpoint);
        first
            .compare_and_swap_index(b"generation-2", Some(&stale_etag))
            .expect("advance Index with matching ETag");
        assert!(
            second
                .compare_and_swap_index(b"lost-generation", Some(&stale_etag))
                .is_err(),
            "If-Match must reject a stale concurrent publisher"
        );
        assert_eq!(
            second
                .get("index.cbor.zst")
                .expect("read final Index")
                .expect("Index exists")
                .bytes,
            b"generation-2"
        );
    }
}
