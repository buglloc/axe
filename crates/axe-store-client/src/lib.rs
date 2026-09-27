mod prepare;

use std::fmt;
use std::fs::{self, File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Read;
use std::io::{self, Write};
#[cfg(target_os = "linux")]
use std::net::TcpStream;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::time::Instant;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axe_artifact::{
    Artifact, KeyId, StoreConfig, StoreIndex, StoreIndexEntry, Target, ToolManifest, TrustedKeys,
    decode_hex_32, verify_document,
};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub use prepare::PreparedTool;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CachePolicy {
    CacheOnly,
    RevalidateIfStale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkPolicy {
    Online,
    Offline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureClass {
    Transient,
    Integrity,
    Configuration,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreStage {
    Index,
    Manifest,
    Object,
    Storage,
    Decode,
    Execute,
    Configuration,
}

impl fmt::Display for StoreStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Index => "index",
            Self::Manifest => "manifest",
            Self::Object => "object",
            Self::Storage => "storage",
            Self::Decode => "decode",
            Self::Execute => "execute",
            Self::Configuration => "configuration",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreError {
    pub stage: StoreStage,
    pub class: FailureClass,
    pub source: String,
}

impl StoreError {
    pub(crate) fn new(stage: StoreStage, class: FailureClass, source: impl Into<String>) -> Self {
        Self {
            stage,
            class,
            source: source.into(),
        }
    }

    pub(crate) fn transient(stage: StoreStage, source: impl Into<String>) -> Self {
        Self::new(stage, FailureClass::Transient, source)
    }

    pub(crate) fn integrity(stage: StoreStage, source: impl Into<String>) -> Self {
        Self::new(stage, FailureClass::Integrity, source)
    }

    pub(crate) fn configuration(stage: StoreStage, source: impl Into<String>) -> Self {
        Self::new(stage, FailureClass::Configuration, source)
    }

    pub(crate) fn unavailable(stage: StoreStage, source: impl Into<String>) -> Self {
        Self::new(stage, FailureClass::Unavailable, source)
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.stage, self.source)
    }
}

impl std::error::Error for StoreError {}

#[derive(Clone, Debug)]
pub enum DownloadEvent<'a> {
    Started {
        name: &'a str,
        version: &'a str,
        target: Target,
        total: u64,
    },
    Advanced {
        downloaded: u64,
        total: u64,
    },
    Finished {
        downloaded: u64,
    },
    Failed,
}

pub trait ProgressReporter {
    fn report(&mut self, event: DownloadEvent<'_>);
}

#[derive(Clone, Debug)]
pub struct ResolvedTool {
    pub name: String,
    pub version: String,
    pub target: Target,
    pub artifact: Artifact,
}

#[derive(Clone, Debug)]
pub struct IndexResult {
    pub index: StoreIndex,
    pub blocking_error: Option<StoreError>,
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub public_base_url: String,
    pub addresses: Vec<IpAddr>,
    pub max_object_bytes: u64,
    pub max_metadata_bytes: u64,
    pub metadata_timeout: Duration,
    pub object_timeout: Duration,
    pub transient_retry: Duration,
    pub free_space_reserve_bytes: u64,
    pub channel: String,
    pub persistent_roots: Vec<PathBuf>,
    pub tmpfs_roots: Vec<PathBuf>,
    pub trusted_keys: TrustedKeys,
    pub embedded_index: Vec<u8>,
}

impl ClientConfig {
    pub fn from_embedded(
        json: &str,
        trusted: &[(&str, &str)],
        embedded_index: &[u8],
    ) -> Result<Self, StoreError> {
        let file = StoreConfig::from_json(json.as_bytes()).map_err(|error| {
            StoreError::configuration(
                StoreStage::Configuration,
                format!("invalid config/store.json: {error}"),
            )
        })?;

        let mut trusted_keys = TrustedKeys::new();
        for (id, key) in trusted {
            insert_trusted_key(&mut trusted_keys, Some(id), key)?;
        }

        if let Ok(key) = std::env::var("AXE_STORE_TRUSTED_KEY")
            && !key.is_empty()
        {
            insert_trusted_key(&mut trusted_keys, None, &key)?;
        }
        if trusted_keys.is_empty() {
            return Err(StoreError::configuration(
                StoreStage::Configuration,
                "no trusted AXE Store public keys",
            ));
        }

        let override_url = std::env::var("AXE_STORE_URL")
            .ok()
            .filter(|value| !value.is_empty());
        let public_base_url = override_url
            .clone()
            .unwrap_or_else(|| file.storage.public_base_url());
        let consumer = file.consumer;

        let addresses = match std::env::var("AXE_STORE_ADDRESSES") {
            Ok(value) if !value.is_empty() => value
                .split(',')
                .map(|address| {
                    address.trim().parse::<IpAddr>().map_err(|error| {
                        StoreError::configuration(
                            StoreStage::Configuration,
                            format!("invalid AXE_STORE_ADDRESSES value {address:?}: {error}"),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => consumer.addresses,
        };
        validate_base_url(&public_base_url, override_url.is_some())?;
        if addresses.is_empty() {
            return Err(StoreError::configuration(
                StoreStage::Configuration,
                "AXE Store address list is empty",
            ));
        }

        Ok(Self {
            public_base_url: ensure_trailing_slash(public_base_url),
            addresses,
            max_object_bytes: consumer.max_object_bytes,
            max_metadata_bytes: consumer.max_metadata_bytes,
            metadata_timeout: Duration::from_secs(consumer.metadata_timeout_secs),
            object_timeout: Duration::from_secs(consumer.object_timeout_secs),
            transient_retry: Duration::from_secs(consumer.transient_retry_secs),
            free_space_reserve_bytes: consumer.free_space_reserve_bytes,
            channel: consumer.channel,
            persistent_roots: consumer.persistent_roots,
            tmpfs_roots: consumer.tmpfs_roots,
            trusted_keys,
            embedded_index: embedded_index.to_vec(),
        })
    }
}

fn validate_base_url(url: &str, explicit_override: bool) -> Result<(), StoreError> {
    let uri = url.parse::<ureq::http::Uri>().map_err(|error| {
        StoreError::configuration(
            StoreStage::Configuration,
            format!("invalid AXE Store URL: {error}"),
        )
    })?;

    let local_http = explicit_override
        && uri.scheme_str() == Some("http")
        && matches!(uri.host(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (uri.scheme_str() != Some("https") && !local_http) || uri.host().is_none() {
        return Err(StoreError::configuration(
            StoreStage::Configuration,
            "AXE Store URL must use HTTPS (explicit loopback overrides may use HTTP)",
        ));
    }
    Ok(())
}

fn ensure_trailing_slash(mut value: String) -> String {
    if !value.ends_with('/') {
        value.push('/');
    }
    value
}

fn metadata_namespace(config: &ClientConfig) -> Result<String, StoreError> {
    // Length framing keeps the URL, key set and channel unambiguous.
    let uri = config
        .public_base_url
        .parse::<ureq::http::Uri>()
        .map_err(|error| {
            StoreError::configuration(
                StoreStage::Configuration,
                format!("invalid AXE Store URL: {error}"),
            )
        })?;
    let scheme = uri.scheme_str().ok_or_else(|| {
        StoreError::configuration(StoreStage::Configuration, "AXE Store URL has no scheme")
    })?;
    let authority = uri.authority().ok_or_else(|| {
        StoreError::configuration(StoreStage::Configuration, "AXE Store URL has no host")
    })?;
    if uri
        .path_and_query()
        .is_some_and(|path| path.query().is_some())
    {
        return Err(StoreError::configuration(
            StoreStage::Configuration,
            "AXE Store base URL must not contain a query",
        ));
    }

    let default_port = uri.port_u16().is_some_and(|port| {
        scheme.eq_ignore_ascii_case("https") && port == 443
            || scheme.eq_ignore_ascii_case("http") && port == 80
    });
    let authority = authority.as_str();
    let authority = if default_port {
        authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host)
    } else {
        authority
    };
    let url = format!(
        "{}://{}{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase(),
        uri.path()
    );

    let mut hash = Sha256::new();
    hash.update(b"axe-store-metadata-v1");
    hash.update((url.len() as u64).to_be_bytes());
    hash.update(url.as_bytes());
    hash.update((config.trusted_keys.ids().count() as u64).to_be_bytes());
    for id in config.trusted_keys.ids() {
        hash.update(id.0);
    }
    hash.update((config.channel.len() as u64).to_be_bytes());
    hash.update(config.channel.as_bytes());
    Ok(axe_artifact::Digest(hash.finalize().into()).to_string())
}

fn insert_trusted_key(
    keys: &mut TrustedKeys,
    named_id: Option<&str>,
    encoded: &str,
) -> Result<(), StoreError> {
    let bytes = decode_hex_32(encoded, "trusted AXE Store public key")
        .map_err(|error| StoreError::configuration(StoreStage::Configuration, error.to_string()))?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|error| {
        StoreError::configuration(
            StoreStage::Configuration,
            format!("invalid trusted key: {error}"),
        )
    })?;
    let id = match named_id {
        Some(value) => value.parse::<KeyId>().map_err(|error| {
            StoreError::configuration(StoreStage::Configuration, error.to_string())
        })?,
        None => KeyId::for_key(&key),
    };
    keys.insert_named(id, key)
        .map_err(|error| StoreError::configuration(StoreStage::Configuration, error.to_string()))
}

pub struct Client {
    pub(crate) config: ClientConfig,
    metadata_agent: ureq::Agent,
    pub(crate) object_agent: ureq::Agent,
    network_policy: NetworkPolicy,
    embedded: StoreIndex,
    state: Mutex<Option<IndexState>>,
    network_backoff: Mutex<Option<NetworkBackoffState>>,
    metadata_namespace: String,
    #[cfg(test)]
    storage_roots_override: Option<Vec<PathBuf>>,
}

impl Client {
    pub fn new(
        mut config: ClientConfig,
        network_policy: NetworkPolicy,
    ) -> Result<Self, StoreError> {
        config.public_base_url = ensure_trailing_slash(config.public_base_url);
        let embedded = if config.embedded_index.is_empty() {
            StoreIndex {
                schema_version: axe_artifact::SCHEMA_VERSION,
                generation: 1,
                ttl_secs: axe_artifact::INDEX_TTL_SECS,
                tools: Default::default(),
            }
        } else {
            decode_index(
                &config.embedded_index,
                &config.trusted_keys,
                StoreStage::Index,
                config.max_metadata_bytes,
            )?
        };

        let metadata_namespace = metadata_namespace(&config)?;
        let metadata_agent = build_agent(&config, config.metadata_timeout)?;
        let object_agent = build_agent(&config, config.object_timeout)?;
        Ok(Self {
            config,
            network_policy,
            metadata_agent,
            object_agent,
            embedded,
            state: Mutex::new(None),
            network_backoff: Mutex::new(None),
            metadata_namespace,
            #[cfg(test)]
            storage_roots_override: None,
        })
    }

    #[must_use]
    pub fn channel(&self) -> &str {
        &self.config.channel
    }

    pub fn index(&self, policy: CachePolicy) -> Result<IndexResult, StoreError> {
        self.load_index(policy, false)
    }

    pub fn refresh_index(&self) -> Result<StoreIndex, StoreError> {
        if self.network_policy == NetworkPolicy::Offline {
            return Err(StoreError::transient(
                StoreStage::Index,
                "network access is disabled by AXE_STORE_MODE=cache-only",
            ));
        }

        let result = self.load_index(CachePolicy::RevalidateIfStale, true)?;
        if let Some(error) = result.blocking_error {
            return Err(error);
        }
        Ok(result.index)
    }

    fn load_index(&self, policy: CachePolicy, force: bool) -> Result<IndexResult, StoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StoreError::transient(StoreStage::Index, "Index state lock poisoned"))?;
        if !force
            && let Some(memo) = state.as_ref()
            && (policy == CachePolicy::CacheOnly
                || self.network_policy == NetworkPolicy::Offline
                || memo
                    .freshness
                    .as_ref()
                    .is_some_and(|freshness| !freshness.is_stale()))
        {
            return Ok(memo.result.clone());
        }

        let cache = self.metadata_root().map(|root| root.join("index"));
        let cached = cache
            .as_deref()
            .map(|directory| self.read_cached_index(directory))
            .transpose();
        let (cached_index, cache_state, cached_error) = match cached {
            Ok(Some(Some((index, state)))) => (Some(index), Some(state), None),
            Ok(Some(None) | None) => (None, None, None),
            Err(error) => {
                if let Some(directory) = &cache {
                    remove_metadata_cache(directory);
                }
                (None, None, Some(error))
            }
        };
        let stale = force || cache_state.as_ref().is_none_or(MetadataState::is_stale);
        let should_fetch = self.network_policy == NetworkPolicy::Online
            && policy == CachePolicy::RevalidateIfStale
            && stale;

        let mut remote = None;
        let mut refresh_error = cached_error;
        let mut freshness = cache_state;
        if should_fetch {
            match self.fetch_index(freshness.as_ref().and_then(|state| state.etag.as_deref())) {
                Ok(Fetch::NotModified) => {
                    if let (Some(directory), Some(metadata_state)) = (&cache, &mut freshness) {
                        metadata_state.checked_at = now_secs();
                        if let Err(error) = write_state(directory, metadata_state)
                            && refresh_error.is_none()
                        {
                            refresh_error = Some(error);
                        }
                    }
                }
                Ok(Fetch::Content { bytes, etag }) => {
                    match decode_index(
                        &bytes,
                        &self.config.trusted_keys,
                        StoreStage::Index,
                        self.config.max_metadata_bytes,
                    ) {
                        Ok(index) => {
                            if cached_index
                                .as_ref()
                                .is_some_and(|cached| index.generation < cached.generation)
                            {
                                refresh_error = Some(StoreError::integrity(
                                    StoreStage::Index,
                                    "remote Index generation decreased",
                                ));
                            } else {
                                let metadata_state = MetadataState {
                                    etag,
                                    checked_at: now_secs(),
                                    ttl: index.ttl_secs,
                                };
                                if let Some(directory) = &cache
                                    && let Err(error) =
                                        write_metadata(directory, &bytes, &metadata_state)
                                    && error.class != FailureClass::Transient
                                {
                                    refresh_error = Some(error);
                                }
                                freshness = Some(metadata_state);
                                remote = Some(index);
                            }
                        }
                        Err(error) => refresh_error = Some(error),
                    }
                }
                Err(error) => refresh_error = Some(error),
            }
        }

        let verified = remote.or(cached_index);
        let index = merge_indexes(&self.embedded, verified.as_ref());
        let blocking_error = refresh_error.filter(|error| error.class != FailureClass::Transient);
        let result = IndexResult {
            index,
            blocking_error,
        };

        *state = Some(IndexState {
            result: result.clone(),
            freshness,
        });
        Ok(result)
    }

    pub fn resolve(&self, name: &str, channel: &str) -> Result<ResolvedTool, StoreError> {
        let result = self.index(CachePolicy::RevalidateIfStale)?;
        if let Some(error) = result.blocking_error {
            return Err(error);
        }
        let (canonical, entry) = find_index_entry(&result.index, name).ok_or_else(|| {
            StoreError::configuration(
                StoreStage::Index,
                format!("unknown AXE Store tool {name:?}"),
            )
        })?;

        let target = Target::detect().map_err(|error| {
            StoreError::configuration(StoreStage::Configuration, error.to_string())
        })?;
        if !entry
            .channels
            .get(channel)
            .is_some_and(|targets| targets.contains(target.as_str()))
        {
            return Err(StoreError::configuration(
                StoreStage::Index,
                format!("{canonical} is unavailable for channel {channel:?} on {target}"),
            ));
        }

        let manifest = self.load_manifest(entry)?;
        resolve_manifest(&manifest, canonical, entry, channel, target)
    }

    pub fn prepare(
        &self,
        resolved: &ResolvedTool,
        progress: &mut dyn ProgressReporter,
    ) -> Result<PreparedTool, StoreError> {
        prepare::prepare(self, resolved, progress)
    }

    pub fn clean(&self) -> Result<Vec<PathBuf>, StoreError> {
        let explicit = std::env::var_os("AXE_STORE_DIR")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let mut removed_roots = Vec::new();
        for root in self.storage_roots() {
            let path = root.join("metadata").join(&self.metadata_namespace);
            match fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::PermissionDenied
                        && explicit.as_deref() != Some(root.as_path())
                        && fs::read_dir(&root).is_err_and(|root_error| {
                            root_error.kind() == io::ErrorKind::PermissionDenied
                        }) =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(StoreError::transient(
                        StoreStage::Storage,
                        format!("inspect {}: {error}", path.display()),
                    ));
                }
            }

            prepare::remove_cache_entry(&path).map_err(|error| {
                StoreError::transient(
                    StoreStage::Storage,
                    format!("remove {}: {error}", path.display()),
                )
            })?;
            removed_roots.push(root);
        }
        Ok(removed_roots)
    }

    fn load_manifest(&self, entry: &StoreIndexEntry) -> Result<ToolManifest, StoreError> {
        let cache = self.metadata_root().map(|root| {
            root.join("tools")
                .join(&entry.id)
                .join("manifests")
                .join(entry.manifest_sha256.to_string())
        });
        let cached = cache
            .as_deref()
            .map(|directory| self.read_cached_manifest(directory, entry))
            .transpose();
        let (cached_manifest, cache_state, corruption) = match cached {
            Ok(Some(Some((manifest, state)))) => (Some(manifest), Some(state), None),
            Ok(Some(None) | None) => (None, None, None),
            Err(error) => {
                if let Some(directory) = &cache {
                    remove_metadata_cache(directory);
                }
                (None, None, Some(error))
            }
        };

        if cache_state.as_ref().is_some_and(|state| !state.is_stale()) {
            return cached_manifest.ok_or_else(|| {
                StoreError::integrity(
                    StoreStage::Manifest,
                    "fresh manifest cache is missing content",
                )
            });
        }
        if self.network_policy == NetworkPolicy::Offline {
            return corruption.map_or_else(
                || {
                    cached_manifest.ok_or_else(|| {
                        StoreError::transient(
                            StoreStage::Manifest,
                            "manifest is not cached and network access is disabled by AXE_STORE_MODE=cache-only",
                        )
                    })
                },
                Err,
            );
        }

        let manifest_path = entry.manifest_path();
        let fetched = self.fetch(
            &manifest_path,
            cache_state.as_ref().and_then(|state| state.etag.as_deref()),
            StoreStage::Manifest,
            self.config.max_metadata_bytes,
        );
        match fetched {
            Ok(Fetch::NotModified) => {
                let manifest = cached_manifest.ok_or_else(|| {
                    StoreError::integrity(
                        StoreStage::Manifest,
                        "server returned 304 without cached manifest",
                    )
                })?;
                if let (Some(directory), Some(mut state)) = (&cache, cache_state) {
                    state.checked_at = now_secs();
                    write_state(directory, &state)?;
                }
                Ok(manifest)
            }
            Ok(Fetch::Content { bytes, etag }) => {
                verify_manifest_digest(&bytes, entry)?;
                let manifest: ToolManifest = verify_document(&bytes, &self.config.trusted_keys)
                    .map_err(|error| {
                        StoreError::integrity(StoreStage::Manifest, error.to_string())
                    })?;
                manifest.validate().map_err(|error| {
                    StoreError::integrity(StoreStage::Manifest, error.to_string())
                })?;
                if manifest.id != entry.id {
                    return Err(StoreError::integrity(
                        StoreStage::Manifest,
                        "manifest ID mismatch",
                    ));
                }

                if let Some(directory) = &cache {
                    write_metadata(
                        directory,
                        &bytes,
                        &MetadataState {
                            etag,
                            checked_at: now_secs(),
                            ttl: manifest.ttl_secs,
                        },
                    )?;
                }
                Ok(manifest)
            }
            Err(error) => {
                if let Some(corruption) = corruption {
                    Err(corruption)
                } else if error.class == FailureClass::Transient {
                    cached_manifest.ok_or(error)
                } else {
                    Err(error)
                }
            }
        }
    }

    fn read_cached_index(
        &self,
        directory: &Path,
    ) -> Result<Option<(StoreIndex, MetadataState)>, StoreError> {
        let Some((bytes, state)) = read_metadata(directory, StoreStage::Index)? else {
            return Ok(None);
        };
        let index = decode_index(
            &bytes,
            &self.config.trusted_keys,
            StoreStage::Index,
            self.config.max_metadata_bytes,
        )?;

        Ok(Some((index, state)))
    }

    fn read_cached_manifest(
        &self,
        directory: &Path,
        entry: &StoreIndexEntry,
    ) -> Result<Option<(ToolManifest, MetadataState)>, StoreError> {
        let Some((bytes, state)) = read_metadata(directory, StoreStage::Manifest)? else {
            return Ok(None);
        };

        verify_manifest_digest(&bytes, entry)?;
        let manifest: ToolManifest = verify_document(&bytes, &self.config.trusted_keys)
            .map_err(|error| StoreError::integrity(StoreStage::Manifest, error.to_string()))?;
        manifest
            .validate()
            .map_err(|error| StoreError::integrity(StoreStage::Manifest, error.to_string()))?;
        Ok(Some((manifest, state)))
    }

    fn fetch_index(&self, etag: Option<&str>) -> Result<Fetch, StoreError> {
        self.fetch_with_agent(
            &self.metadata_agent,
            "index.cbor.zst",
            etag,
            StoreStage::Index,
            self.config.max_metadata_bytes,
        )
    }

    fn fetch(
        &self,
        relative: &str,
        etag: Option<&str>,
        stage: StoreStage,
        limit: u64,
    ) -> Result<Fetch, StoreError> {
        self.fetch_with_agent(&self.metadata_agent, relative, etag, stage, limit)
    }

    pub(crate) fn fetch_with_agent(
        &self,
        agent: &ureq::Agent,
        relative: &str,
        etag: Option<&str>,
        stage: StoreStage,
        limit: u64,
    ) -> Result<Fetch, StoreError> {
        self.before_network(stage)?;

        let result = (|| {
            let url = format!("{}{}", self.config.public_base_url, relative);
            let mut request = agent
                .get(&url)
                .header("User-Agent", concat!("axe/", env!("CARGO_PKG_VERSION")));
            if let Some(etag) = etag {
                request = request.header("If-None-Match", etag);
            }
            let mut response = request
                .call()
                .map_err(|error| classify_http(stage, error))?;
            let status = response.status().as_u16();
            if status == 304 {
                return Ok(Fetch::NotModified);
            }
            if status == 404 || status == 410 {
                return Err(if stage == StoreStage::Index {
                    StoreError::transient(
                        stage,
                        format!("HTTP {status}: remote Index is unavailable"),
                    )
                } else {
                    StoreError::integrity(stage, format!("HTTP {status} for signed AXE Store path"))
                });
            }
            if status == 408 || status == 429 || status >= 500 {
                return Err(StoreError::transient(stage, format!("HTTP {status}")));
            }
            if !(200..300).contains(&status) {
                return Err(StoreError::configuration(stage, format!("HTTP {status}")));
            }
            if response
                .body()
                .content_length()
                .is_some_and(|length| length > limit)
            {
                return Err(StoreError::integrity(
                    stage,
                    format!("response exceeds {limit} byte limit"),
                ));
            }
            let etag = response
                .headers()
                .get("etag")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let bytes = response
                .body_mut()
                .with_config()
                .limit(limit)
                .read_to_vec()
                .map_err(|error| StoreError::transient(stage, format!("read response: {error}")))?;

            Ok(Fetch::Content { bytes, etag })
        })();

        self.record_network_result(&result);
        result
    }

    pub(crate) fn before_network(&self, stage: StoreStage) -> Result<(), StoreError> {
        if self.network_policy == NetworkPolicy::Offline {
            return Err(StoreError::transient(
                stage,
                "network access is disabled by AXE_STORE_MODE=cache-only",
            ));
        }

        let now = now_secs();
        let memory_retry = self
            .network_backoff
            .lock()
            .ok()
            .and_then(|retry| retry.clone());
        let persisted_retry = self.read_network_backoff(now);
        let retry = memory_retry
            .into_iter()
            .chain(persisted_retry)
            .max_by_key(|state| state.retry_after);
        if let Some(retry) = retry
            && retry.retry_after > now
        {
            let previous_failure = retry
                .failure
                .as_deref()
                .map(|failure| format!("; previous transient failure: {failure}"))
                .unwrap_or_default();
            return Err(StoreError::transient(
                stage,
                format!(
                    "network retry deferred for {} seconds{previous_failure}",
                    retry.retry_after - now
                ),
            ));
        }

        self.clear_network_backoff();
        Ok(())
    }

    pub(crate) fn record_network_result<T>(&self, result: &Result<T, StoreError>) {
        match result {
            Ok(_) => self.clear_network_backoff(),
            Err(error) if error.class == FailureClass::Transient => {
                self.defer_network_retry(error);
            }
            Err(_) => {}
        }
    }

    fn read_network_backoff(&self, now: u64) -> Option<NetworkBackoffState> {
        let path = self.network_backoff_path()?;
        let bytes = fs::read(&path).ok()?;
        let state: NetworkBackoffState = match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(_) => {
                let _ = fs::remove_file(path);
                return None;
            }
        };

        let latest_valid = now.saturating_add(self.config.transient_retry.as_secs());
        if state.retry_after > latest_valid {
            let _ = fs::remove_file(path);
            return None;
        }
        Some(state)
    }

    fn defer_network_retry(&self, error: &StoreError) {
        let state = NetworkBackoffState {
            retry_after: now_secs().saturating_add(self.config.transient_retry.as_secs()),
            failure: Some(error.to_string()),
        };
        if let Ok(mut current) = self.network_backoff.lock() {
            *current = Some(state.clone());
        }

        let Some(path) = self.network_backoff_path() else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(&state) else {
            return;
        };
        let _ = write_atomic(&path, &bytes);
    }

    fn clear_network_backoff(&self) {
        if let Ok(mut current) = self.network_backoff.lock() {
            *current = None;
        }
        if let Some(path) = self.network_backoff_path() {
            let _ = fs::remove_file(path);
        }
    }

    fn network_backoff_path(&self) -> Option<PathBuf> {
        self.metadata_root()
            .map(|root| root.join("network-backoff.json"))
    }

    fn metadata_root(&self) -> Option<PathBuf> {
        self.storage_roots()
            .into_iter()
            .find(|root| ensure_writable_root(root, self.config.max_metadata_bytes).is_ok())
            .map(|root| root.join("metadata").join(&self.metadata_namespace))
    }

    pub(crate) fn storage_roots(&self) -> Vec<PathBuf> {
        #[cfg(test)]
        if let Some(roots) = &self.storage_roots_override {
            return roots.clone();
        }
        storage_roots(&self.config)
    }
}

pub(crate) enum Fetch {
    NotModified,
    Content {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
}

struct IndexState {
    result: IndexResult,
    /// Freshness of the verified Index behind `result`, if any was verified.
    freshness: Option<MetadataState>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MetadataState {
    etag: Option<String>,
    checked_at: u64,
    ttl: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NetworkBackoffState {
    retry_after: u64,
    #[serde(default)]
    failure: Option<String>,
}

impl MetadataState {
    fn is_stale(&self) -> bool {
        let now = now_secs();
        now < self.checked_at || now.saturating_sub(self.checked_at) >= self.ttl
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn verify_manifest_digest(bytes: &[u8], entry: &StoreIndexEntry) -> Result<(), StoreError> {
    if axe_artifact::Digest::of(bytes) != entry.manifest_sha256 {
        return Err(StoreError::integrity(
            StoreStage::Manifest,
            "signed manifest SHA-256 does not match Index",
        ));
    }
    Ok(())
}

fn decode_index(
    bytes: &[u8],
    keys: &TrustedKeys,
    stage: StoreStage,
    limit: u64,
) -> Result<StoreIndex, StoreError> {
    if bytes.len() as u64 > limit {
        return Err(StoreError::integrity(
            stage,
            "Index exceeds metadata size limit",
        ));
    }
    let signed = zstd::stream::decode_all(bytes).map_err(|error| {
        StoreError::integrity(stage, format!("invalid Index Zstandard: {error}"))
    })?;
    if signed.len() as u64 > limit {
        return Err(StoreError::integrity(
            stage,
            "decoded Index exceeds metadata size limit",
        ));
    }

    let index: StoreIndex = verify_document(&signed, keys)
        .map_err(|error| StoreError::integrity(stage, error.to_string()))?;
    index
        .validate()
        .map_err(|error| StoreError::integrity(stage, error.to_string()))?;
    Ok(index)
}

fn resolve_manifest(
    manifest: &ToolManifest,
    canonical: &str,
    entry: &StoreIndexEntry,
    channel: &str,
    target: Target,
) -> Result<ResolvedTool, StoreError> {
    manifest
        .validate_index_identity(canonical, entry)
        .map_err(|error| StoreError::integrity(StoreStage::Manifest, error.to_string()))?;
    let unavailable = || {
        StoreError::unavailable(
            StoreStage::Manifest,
            format!("{canonical} is unavailable for channel {channel:?} on {target}"),
        )
    };

    let version = manifest.channels.get(channel).ok_or_else(unavailable)?;
    let artifact = manifest
        .versions
        .get(version)
        .and_then(|version| version.targets.get(target.as_str()))
        .cloned()
        .ok_or_else(unavailable)?;
    Ok(ResolvedTool {
        name: canonical.to_owned(),
        version: version.clone(),
        target,
        artifact,
    })
}

fn merge_indexes(embedded: &StoreIndex, remote: Option<&StoreIndex>) -> StoreIndex {
    let mut merged = embedded.clone();
    if let Some(remote) = remote
        && remote.generation >= embedded.generation
    {
        merged.generation = remote.generation;
        merged.ttl_secs = remote.ttl_secs;
        for (name, entry) in &remote.tools {
            merged.tools.insert(name.clone(), entry.clone());
        }
    }
    merged
}

fn find_index_entry<'a>(
    index: &'a StoreIndex,
    name: &str,
) -> Option<(&'a str, &'a StoreIndexEntry)> {
    index
        .tools
        .get_key_value(name)
        .map(|(name, entry)| (name.as_str(), entry))
        .or_else(|| {
            index
                .tools
                .iter()
                .find(|(_, entry)| entry.aliases.iter().any(|alias| alias == name))
                .map(|(name, entry)| (name.as_str(), entry))
        })
}

fn read_metadata(
    directory: &Path,
    stage: StoreStage,
) -> Result<Option<(Vec<u8>, MetadataState)>, StoreError> {
    let content = directory.join("content");
    let state = directory.join("state.json");
    if !content.exists() && !state.exists() {
        return Ok(None);
    }

    let bytes = fs::read(&content).map_err(|error| {
        StoreError::integrity(stage, format!("read cached {}: {error}", content.display()))
    })?;
    let metadata: MetadataState = serde_json::from_slice(&fs::read(&state).map_err(|error| {
        StoreError::integrity(stage, format!("read cached {}: {error}", state.display()))
    })?)
    .map_err(|error| StoreError::integrity(stage, format!("invalid cache state: {error}")))?;
    Ok(Some((bytes, metadata)))
}

fn write_metadata(directory: &Path, bytes: &[u8], state: &MetadataState) -> Result<(), StoreError> {
    fs::create_dir_all(directory).map_err(|error| storage_error(directory, error))?;
    write_atomic(&directory.join("content"), bytes)?;
    write_state(directory, state)
}

fn write_state(directory: &Path, state: &MetadataState) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec(state).map_err(|error| {
        StoreError::configuration(
            StoreStage::Configuration,
            format!("encode metadata state: {error}"),
        )
    })?;
    write_atomic(&directory.join("state.json"), &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().expect("cache file has parent");
    fs::create_dir_all(parent).map_err(|error| storage_error(parent, error))?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));

    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| storage_error(path, error))
}

fn remove_metadata_cache(directory: &Path) {
    let _ = fs::remove_file(directory.join("content"));
    let _ = fs::remove_file(directory.join("state.json"));
}

pub(crate) fn storage_error(path: &Path, error: io::Error) -> StoreError {
    StoreError::transient(StoreStage::Storage, format!("{}: {error}", path.display()))
}

fn build_agent(config: &ClientConfig, timeout: Duration) -> Result<ureq::Agent, StoreError> {
    use ureq::unversioned::transport::{Connector, RustlsConnector};

    let uri = config
        .public_base_url
        .parse::<ureq::http::Uri>()
        .map_err(|error| {
            StoreError::configuration(
                StoreStage::Configuration,
                format!("invalid AXE Store URL: {error}"),
            )
        })?;
    let host = uri
        .host()
        .ok_or_else(|| {
            StoreError::configuration(StoreStage::Configuration, "AXE Store URL has no host")
        })?
        .to_owned();

    let certificates = axe_tls_roots::certificates()
        .iter()
        .map(|certificate| ureq::tls::Certificate::from_der(certificate.as_ref()).to_owned())
        .collect::<Vec<_>>();
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .root_certs(ureq::tls::RootCerts::from(certificates))
        .build();
    let agent_config = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build();
    #[cfg(target_os = "linux")]
    let connector = ().chain(HappyEyeballsConnector).chain(RustlsConnector::default());
    #[cfg(not(target_os = "linux"))]
    let connector =
        ().chain(ureq::unversioned::transport::TcpConnector::default())
            .chain(RustlsConnector::default());

    Ok(ureq::Agent::with_parts(
        agent_config,
        connector,
        FixedResolver {
            host,
            addresses: config.addresses.clone(),
        },
    ))
}

#[cfg(target_os = "linux")]
const HAPPY_EYEBALLS_DELAY: Duration = Duration::from_millis(250);

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct HappyEyeballsConnector;

#[cfg(target_os = "linux")]
impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Connector<In>
    for HappyEyeballsConnector
{
    type Out = ureq::unversioned::transport::Either<In, StoreTcpTransport>;

    fn connect(
        &self,
        details: &ureq::unversioned::transport::ConnectionDetails<'_>,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        use ureq::unversioned::transport::{Either, LazyBuffers};

        if let Some(transport) = chained {
            return Ok(Some(Either::A(transport)));
        }

        let stream = happy_eyeballs_connect(&details.addrs, details.timeout, details.config)?;
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Either::B(StoreTcpTransport {
            stream,
            buffers,
            read_timeout: None,
            write_timeout: None,
        })))
    }
}

#[cfg(target_os = "linux")]
struct PendingConnect {
    descriptor: std::os::fd::OwnedFd,
    address: SocketAddr,
}

#[cfg(target_os = "linux")]
enum ConnectAttempt {
    Connected(std::os::fd::OwnedFd),
    Pending(std::os::fd::OwnedFd),
}

#[cfg(target_os = "linux")]
fn happy_eyeballs_connect(
    addresses: &ureq::unversioned::resolver::ResolvedSocketAddrs,
    timeout: ureq::unversioned::transport::NextTimeout,
    config: &ureq::config::Config,
) -> Result<TcpStream, ureq::Error> {
    let started = Instant::now();
    let deadline = timeout
        .not_zero()
        .and_then(|duration| started.checked_add(*duration));
    let mut next_address = 0;
    let mut next_launch = started;
    let mut pending = Vec::<PendingConnect>::new();
    let mut poll_descriptors = Vec::<libc::pollfd>::new();
    let mut failures = Vec::<(SocketAddr, io::Error)>::new();

    loop {
        let now = Instant::now();
        if next_address < addresses.len() && (pending.is_empty() || now >= next_launch) {
            let address = addresses[next_address];
            next_address += 1;

            match start_nonblocking_connect(address) {
                Ok(ConnectAttempt::Connected(descriptor)) => {
                    return finish_connection(descriptor, config).map_err(ureq::Error::Io);
                }
                Ok(ConnectAttempt::Pending(descriptor)) => {
                    pending.push(PendingConnect {
                        descriptor,
                        address,
                    });
                    next_launch = now + HAPPY_EYEBALLS_DELAY;
                }
                Err(error) => {
                    failures.push((address, error));
                    next_launch = now;
                }
            }
            continue;
        }

        if pending.is_empty() && next_address == addresses.len() {
            break;
        }

        let now = Instant::now();
        if deadline.is_some_and(|deadline| now >= deadline) {
            failures.extend(pending.drain(..).map(|attempt| {
                (
                    attempt.address,
                    io::Error::new(io::ErrorKind::TimedOut, "connection attempt timed out"),
                )
            }));
            break;
        }

        let wake_at = match (deadline, next_address < addresses.len()) {
            (Some(deadline), true) => Some(deadline.min(next_launch)),
            (Some(deadline), false) => Some(deadline),
            (None, true) => Some(next_launch),
            (None, false) => None,
        };
        poll_descriptors.clear();
        poll_descriptors.extend(pending.iter().map(|attempt| libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(&attempt.descriptor),
            events: libc::POLLOUT,
            revents: 0,
        }));

        let ready = loop {
            // SAFETY: poll_descriptors owns a contiguous writable pollfd array for this call.
            let result = unsafe {
                libc::poll(
                    poll_descriptors.as_mut_ptr(),
                    poll_descriptors.len() as libc::nfds_t,
                    poll_timeout(now, wake_at),
                )
            };
            if result >= 0 {
                break result;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                failures.push((
                    pending[0].address,
                    io::Error::other(format!("poll connection attempts: {error}")),
                ));
                return Err(connect_failures(failures));
            }
        };

        if ready == 0 {
            continue;
        }

        for index in (0..pending.len()).rev() {
            if poll_descriptors[index].revents == 0 {
                continue;
            }
            let attempt = pending.swap_remove(index);
            match pending_socket_error(&attempt.descriptor) {
                Ok(None) => {
                    return finish_connection(attempt.descriptor, config).map_err(ureq::Error::Io);
                }
                Ok(Some(error)) | Err(error) => failures.push((attempt.address, error)),
            }
        }

        if pending.is_empty() {
            next_launch = Instant::now();
        }
    }

    Err(connect_failures(failures))
}

#[cfg(target_os = "linux")]
fn poll_timeout(now: Instant, wake_at: Option<Instant>) -> libc::c_int {
    let Some(wake_at) = wake_at else {
        return -1;
    };
    let wait = wake_at.saturating_duration_since(now);
    if wait.is_zero() {
        0
    } else {
        wait.as_millis()
            .saturating_add(1)
            .min(libc::c_int::MAX as u128) as libc::c_int
    }
}

#[cfg(target_os = "linux")]
fn start_nonblocking_connect(address: SocketAddr) -> io::Result<ConnectAttempt> {
    use std::os::fd::FromRawFd;

    let domain = if address.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    // SAFETY: socket returns a new descriptor or -1 and does not access Rust memory.
    let raw_descriptor = unsafe {
        libc::socket(
            domain,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::IPPROTO_TCP,
        )
    };
    if raw_descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: raw_descriptor is newly owned and transferred exactly once.
    let descriptor = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_descriptor) };
    let result = match address {
        SocketAddr::V4(address) => {
            let raw = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: address.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(address.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: raw is a fully initialized IPv4 socket address with the matching length.
            unsafe {
                libc::connect(
                    raw_descriptor,
                    (&raw as *const libc::sockaddr_in).cast(),
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
        }
        SocketAddr::V6(address) => {
            let raw = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: address.port().to_be(),
                sin6_flowinfo: address.flowinfo().to_be(),
                sin6_addr: libc::in6_addr {
                    s6_addr: address.ip().octets(),
                },
                sin6_scope_id: address.scope_id(),
            };
            // SAFETY: raw is a fully initialized IPv6 socket address with the matching length.
            unsafe {
                libc::connect(
                    raw_descriptor,
                    (&raw as *const libc::sockaddr_in6).cast(),
                    std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        }
    };
    if result == 0 {
        return Ok(ConnectAttempt::Connected(descriptor));
    }

    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::EINPROGRESS | libc::EWOULDBLOCK | libc::EALREADY)
    ) {
        Ok(ConnectAttempt::Pending(descriptor))
    } else {
        Err(error)
    }
}

#[cfg(target_os = "linux")]
fn pending_socket_error(descriptor: &std::os::fd::OwnedFd) -> io::Result<Option<io::Error>> {
    use std::os::fd::AsRawFd;

    let mut socket_error = 0;
    let mut length = std::mem::size_of_val(&socket_error) as libc::socklen_t;
    // SAFETY: socket_error and length are writable and descriptor remains valid for the call.
    let result = unsafe {
        libc::getsockopt(
            descriptor.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&mut socket_error as *mut libc::c_int).cast(),
            &mut length,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else if socket_error == 0 {
        Ok(None)
    } else {
        Ok(Some(io::Error::from_raw_os_error(socket_error)))
    }
}

#[cfg(target_os = "linux")]
fn finish_connection(
    descriptor: std::os::fd::OwnedFd,
    config: &ureq::config::Config,
) -> io::Result<TcpStream> {
    let stream = TcpStream::from(descriptor);
    stream.set_nonblocking(false)?;
    if config.no_delay() {
        stream.set_nodelay(true)?;
    }
    Ok(stream)
}

#[cfg(target_os = "linux")]
fn connect_failures(failures: Vec<(SocketAddr, io::Error)>) -> ureq::Error {
    use std::fmt::Write as _;

    let mut message = String::from("all AXE Store addresses failed");
    for (address, error) in failures {
        let _ = write!(message, "; {address}: {error}");
    }
    ureq::Error::Io(io::Error::other(message))
}

#[cfg(target_os = "linux")]
struct StoreTcpTransport {
    stream: TcpStream,
    buffers: ureq::unversioned::transport::LazyBuffers,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

#[cfg(target_os = "linux")]
impl StoreTcpTransport {
    fn update_timeout(
        stream: &TcpStream,
        timeout: ureq::unversioned::transport::NextTimeout,
        previous: &mut Option<Duration>,
        update: impl Fn(&TcpStream, Option<Duration>) -> io::Result<()>,
    ) -> Result<(), ureq::Error> {
        let current = timeout.not_zero().map(|duration| *duration);
        if current != *previous {
            update(stream, current).map_err(ureq::Error::Io)?;
            *previous = current;
        }
        Ok(())
    }

    fn map_io(error: io::Error, timeout: ureq::unversioned::transport::NextTimeout) -> ureq::Error {
        if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            ureq::Error::Timeout(timeout.reason)
        } else {
            ureq::Error::Io(error)
        }
    }
}

#[cfg(target_os = "linux")]
impl fmt::Debug for StoreTcpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreTcpTransport")
            .field("address", &self.stream.peer_addr().ok())
            .finish()
    }
}

#[cfg(target_os = "linux")]
impl ureq::unversioned::transport::Transport for StoreTcpTransport {
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        &mut self.buffers
    }

    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<(), ureq::Error> {
        use ureq::unversioned::transport::Buffers as _;

        Self::update_timeout(
            &self.stream,
            timeout,
            &mut self.write_timeout,
            TcpStream::set_write_timeout,
        )?;
        self.stream
            .write_all(&self.buffers.output()[..amount])
            .map_err(|error| Self::map_io(error, timeout))
    }

    fn await_input(
        &mut self,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<bool, ureq::Error> {
        use ureq::unversioned::transport::Buffers as _;

        Self::update_timeout(
            &self.stream,
            timeout,
            &mut self.read_timeout,
            TcpStream::set_read_timeout,
        )?;
        let input = self.buffers.input_append_buf();
        let amount = self
            .stream
            .read(input)
            .map_err(|error| Self::map_io(error, timeout))?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut byte = [0_u8; 1];
        let probe = self.stream.peek(&mut byte);
        let restored = self.stream.set_nonblocking(false).is_ok();
        restored && matches!(probe, Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    }
}

#[derive(Clone, Debug)]
struct FixedResolver {
    host: String,
    addresses: Vec<IpAddr>,
}

impl ureq::unversioned::resolver::Resolver for FixedResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        if uri.host() != Some(&self.host) {
            return Err(ureq::Error::HostNotFound);
        }

        let mut resolved = self.empty();
        let port = uri.port_u16().unwrap_or(443);
        let prefer_ipv4 = self.addresses.first().is_none_or(IpAddr::is_ipv4);
        let mut ipv4_cursor = 0;
        let mut ipv6_cursor = 0;
        loop {
            let preferred = next_family_address(
                &self.addresses,
                prefer_ipv4,
                if prefer_ipv4 {
                    &mut ipv4_cursor
                } else {
                    &mut ipv6_cursor
                },
            );
            let alternate = next_family_address(
                &self.addresses,
                !prefer_ipv4,
                if prefer_ipv4 {
                    &mut ipv6_cursor
                } else {
                    &mut ipv4_cursor
                },
            );
            if preferred.is_none() && alternate.is_none() {
                break;
            }
            for address in preferred.into_iter().chain(alternate) {
                resolved.push(SocketAddr::new(address, port));
            }
        }

        if resolved.is_empty() {
            Err(ureq::Error::HostNotFound)
        } else {
            Ok(resolved)
        }
    }
}

fn next_family_address(addresses: &[IpAddr], ipv4: bool, cursor: &mut usize) -> Option<IpAddr> {
    while let Some(address) = addresses.get(*cursor).copied() {
        *cursor += 1;
        if address.is_ipv4() == ipv4 {
            return Some(address);
        }
    }
    None
}

pub(crate) fn classify_http(stage: StoreStage, error: ureq::Error) -> StoreError {
    match error {
        ureq::Error::StatusCode(404 | 410) if stage == StoreStage::Index => {
            StoreError::transient(stage, format!("HTTP {error}: remote Index is unavailable"))
        }
        ureq::Error::StatusCode(404 | 410) => StoreError::integrity(stage, format!("HTTP {error}")),
        ureq::Error::StatusCode(408 | 429 | 500..=599)
        | ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed => StoreError::transient(stage, error.to_string()),
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => {
            StoreError::integrity(stage, error.to_string())
        }
        ureq::Error::StatusCode(_) | ureq::Error::BadUri(_) | ureq::Error::RequireHttpsOnly(_) => {
            StoreError::configuration(stage, error.to_string())
        }
        _ => StoreError::transient(stage, error.to_string()),
    }
}

fn storage_roots(config: &ClientConfig) -> Vec<PathBuf> {
    let explicit = std::env::var_os("AXE_STORE_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    axe_paths::store_candidate_roots(explicit, &config.persistent_roots, &config.tmpfs_roots)
}

pub(crate) fn ensure_writable_root(root: &Path, required: u64) -> Result<(), StoreError> {
    axe_paths::ensure_writable_root(root, required).map_err(|error| storage_error(root, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axe_artifact::{ArtifactKind, Digest, SCHEMA_VERSION, ToolVersion, sign_document};
    use ed25519_dalek::SigningKey;
    use std::collections::{BTreeMap, BTreeSet};

    fn index_with_target(name: &str, generation: u64, target: Target) -> StoreIndex {
        StoreIndex {
            schema_version: SCHEMA_VERSION,
            generation,
            ttl_secs: 60,
            tools: BTreeMap::from([(
                name.into(),
                StoreIndexEntry {
                    id: format!("test/{name}"),
                    aliases: Vec::new(),
                    manifest_sha256: Digest::of(name.as_bytes()),
                    channels: BTreeMap::from([(
                        "stable".into(),
                        BTreeSet::from([target.to_string()]),
                    )]),
                    synopsis: None,
                },
            )]),
        }
    }

    fn manifest_with_targets(name: &str, targets: &[Target]) -> ToolManifest {
        let targets = targets
            .iter()
            .map(|target| {
                (
                    target.to_string(),
                    Artifact {
                        kind: ArtifactKind::SingleBinary,
                        object_sha256: Digest([1; 32]),
                        compressed_size: 1,
                        unpacked_size: 1,
                        unpacked_sha256: Digest([2; 32]),
                        fully_static: false,
                    },
                )
            })
            .collect();
        ToolManifest {
            schema_version: SCHEMA_VERSION,
            id: format!("test/{name}"),
            name: name.into(),
            ttl_secs: 60,
            channels: BTreeMap::from([("stable".into(), "1.0.0".into())]),
            versions: BTreeMap::from([("1.0.0".into(), ToolVersion { targets })]),
        }
    }

    fn another_target(current: Target) -> Target {
        Target::ALL
            .into_iter()
            .find(|target| *target != current)
            .expect("another supported target exists")
    }

    fn client_for(index: &StoreIndex, public_base_url: String) -> Client {
        client_for_policy(index, public_base_url, NetworkPolicy::Online)
    }

    fn client_for_policy(
        index: &StoreIndex,
        public_base_url: String,
        network_policy: NetworkPolicy,
    ) -> Client {
        let signing = SigningKey::from_bytes(&[17; 32]);
        let mut trusted_keys = TrustedKeys::new();
        trusted_keys.insert(signing.verifying_key());
        let signed = sign_document(index, &signing).expect("sign embedded Index");
        let embedded_index =
            zstd::stream::encode_all(signed.as_slice(), 3).expect("compress embedded Index");

        let mut client = Client::new(
            ClientConfig {
                public_base_url,
                addresses: vec!["127.0.0.1".parse().expect("parse loopback")],
                max_object_bytes: 1024 * 1024,
                max_metadata_bytes: 1024 * 1024,
                metadata_timeout: Duration::from_millis(10),
                object_timeout: Duration::from_secs(2),
                transient_retry: Duration::from_secs(30),
                free_space_reserve_bytes: 1,
                channel: "stable".into(),
                persistent_roots: Vec::new(),
                tmpfs_roots: Vec::new(),
                trusted_keys,
                embedded_index,
            },
            network_policy,
        )
        .expect("create client");
        client.storage_roots_override = Some(Vec::new());
        client
    }

    fn empty_embedded_client(
        index: &StoreIndex,
        url: String,
        network_policy: NetworkPolicy,
    ) -> Client {
        let mut config = client_for_policy(index, url, network_policy).config;
        config.embedded_index.clear();
        let mut client = Client::new(config, network_policy).expect("create development client");
        client.storage_roots_override = Some(Vec::new());
        client
    }

    fn cache_signed_manifest(client: &Client, entry: &StoreIndexEntry, bytes: &[u8]) {
        let directory = client
            .metadata_root()
            .expect("select metadata root")
            .join("tools")
            .join(&entry.id)
            .join("manifests")
            .join(entry.manifest_sha256.to_string());
        write_metadata(
            &directory,
            bytes,
            &MetadataState {
                etag: Some("\"manifest\"".into()),
                checked_at: now_secs(),
                ttl: 60,
            },
        )
        .expect("cache signed manifest");
    }

    fn cache_signed_index(client: &Client, index: &StoreIndex) {
        let signed =
            sign_document(index, &SigningKey::from_bytes(&[17; 32])).expect("sign cached Index");
        let compressed =
            zstd::stream::encode_all(signed.as_slice(), 3).expect("compress cached Index");
        write_metadata(
            &client
                .metadata_root()
                .expect("select metadata root")
                .join("index"),
            &compressed,
            &MetadataState {
                etag: Some("\"index\"".into()),
                checked_at: now_secs(),
                ttl: index.ttl_secs,
            },
        )
        .expect("cache signed Index");
    }

    #[test]
    fn metadata_namespace_normalizes_store_identity_and_sorts_key_ids() {
        let index = index_with_target("namespace", 1, Target::detect().expect("supported target"));
        let mut config = client_for(&index, "https://example.com/store/".into()).config;
        let initial = metadata_namespace(&config).expect("compute namespace");
        config.public_base_url = "HTTPS://EXAMPLE.COM:443/store".into();
        assert_eq!(
            Client::new(config.clone(), NetworkPolicy::Offline)
                .expect("normalize URL at client boundary")
                .metadata_namespace,
            initial
        );
        config.public_base_url = ensure_trailing_slash(config.public_base_url);
        config.channel = "testing".into();
        assert_ne!(
            metadata_namespace(&config).expect("channel namespace"),
            initial
        );
        config.channel = "stable".into();

        let first = SigningKey::from_bytes(&[19; 32]).verifying_key();
        let second = SigningKey::from_bytes(&[23; 32]).verifying_key();
        let mut reordered = config.clone();
        config.trusted_keys.insert(first);
        config.trusted_keys.insert(second);
        reordered.trusted_keys.insert(second);
        reordered.trusted_keys.insert(first);
        assert_ne!(
            metadata_namespace(&config).expect("rotated key set namespace"),
            initial
        );
        assert_eq!(
            metadata_namespace(&config).expect("first key ordering"),
            metadata_namespace(&reordered).expect("second key ordering")
        );
    }

    #[test]
    fn removed_unrelated_target_does_not_block_current_target() {
        let current = Target::detect().expect("supported test target");
        let other = another_target(current);
        let mut index = index_with_target("tool", 1, current);
        let entry = index.tools.get_mut("tool").expect("Index entry");
        entry
            .channels
            .get_mut("stable")
            .expect("stable channel")
            .insert(other.to_string());
        let manifest = manifest_with_targets("tool", &[current]);

        let resolved = resolve_manifest(&manifest, "tool", &index.tools["tool"], "stable", current)
            .expect("resolve current target");

        assert_eq!(resolved.target, current);
        assert_eq!(resolved.version, "1.0.0");
    }

    #[test]
    fn removed_current_target_is_fallback_eligible() {
        let current = Target::detect().expect("supported test target");
        let other = another_target(current);
        let mut index = index_with_target("tool", 1, current);
        index
            .tools
            .get_mut("tool")
            .expect("Index entry")
            .channels
            .get_mut("stable")
            .expect("stable channel")
            .insert(other.to_string());
        let manifest = manifest_with_targets("tool", &[other]);

        let error = resolve_manifest(&manifest, "tool", &index.tools["tool"], "stable", current)
            .expect_err("removed current target must be unavailable");

        assert_eq!(error.class, FailureClass::Unavailable);
        assert_eq!(error.stage, StoreStage::Manifest);
    }

    #[cfg(target_os = "linux")]
    fn assert_family_fallback(listener: std::net::TcpListener, failed_address: IpAddr) {
        use std::io::{Read as _, Write as _};

        let listening_address = listener.local_addr().expect("read Store probe address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept fallback request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read fallback request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("write fallback response");
        });
        let target = Target::detect().expect("supported test target");
        let index = index_with_target("fallback", 1, target);
        let client = client_for(
            &index,
            format!("http://localhost:{}/", listening_address.port()),
        );
        let mut config = client.config.clone();
        config.addresses = vec![failed_address, listening_address.ip()];
        config.metadata_timeout = Duration::from_secs(2);
        let agent =
            build_agent(&config, config.metadata_timeout).expect("build fallback Store agent");

        let mut response = agent
            .get(format!(
                "http://localhost:{}/probe",
                listening_address.port()
            ))
            .call()
            .expect("fall back to the working address family");

        assert_eq!(response.status(), 200);
        assert_eq!(
            response.body_mut().read_to_string().expect("read response"),
            "ok"
        );
        server.join().expect("join fallback server");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_ipv6_connection_races_working_ipv4() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind IPv4 Store probe");
        assert_family_fallback(
            listener,
            "2001:db8::1".parse().expect("parse unreachable IPv6"),
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_ipv4_connection_races_working_ipv6() {
        let Ok(listener) = std::net::TcpListener::bind("[::1]:0") else {
            return;
        };
        assert_family_fallback(
            listener,
            "192.0.2.1".parse().expect("parse unreachable IPv4"),
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_connection_reports_every_attempted_address() {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve unused Store port");
        let port = listener.local_addr().expect("read unused port").port();
        drop(listener);
        let target = Target::detect().expect("supported test target");
        let index = index_with_target("failure-details", 1, target);
        let client = client_for(&index, format!("http://localhost:{port}/"));
        let mut config = client.config.clone();
        config.addresses = vec![
            "127.0.0.1".parse().expect("parse IPv4 loopback"),
            "::1".parse().expect("parse IPv6 loopback"),
        ];
        config.metadata_timeout = Duration::from_secs(2);
        let agent =
            build_agent(&config, config.metadata_timeout).expect("build failing Store agent");

        let error = agent
            .get(format!("http://localhost:{port}/probe"))
            .call()
            .expect_err("both connection attempts must fail");
        let message = error.to_string();

        assert!(message.contains(&format!("127.0.0.1:{port}")), "{message}");
        assert!(message.contains(&format!("[::1]:{port}")), "{message}");
    }

    #[test]
    fn merge_keeps_bootstrap_safety_net_and_rejects_generation_downgrade() {
        let target = Target::detect().expect("supported test target");
        let embedded = index_with_target("bootstrap", 2, target);
        let older = index_with_target("older", 1, target);
        assert_eq!(merge_indexes(&embedded, Some(&older)), embedded);

        let remote = index_with_target("remote", 3, target);
        let merged = merge_indexes(&embedded, Some(&remote));
        assert_eq!(merged.generation, 3);
        assert!(merged.tools.contains_key("bootstrap"));
        assert!(merged.tools.contains_key("remote"));
    }

    #[test]
    fn signed_entry_for_another_target_is_not_dispatchable() {
        let current = Target::detect().expect("supported test target");
        let other = Target::ALL
            .into_iter()
            .find(|target| *target != current)
            .expect("another target exists");
        let client = client_for(
            &index_with_target("other-only", 1, other),
            "http://127.0.0.1:9/".into(),
        );
        client
            .index(CachePolicy::CacheOnly)
            .expect("load embedded Index");
        let error = client
            .resolve("other-only", "stable")
            .expect_err("current target must be absent");
        assert_eq!(error.class, FailureClass::Configuration);
        assert_eq!(error.stage, StoreStage::Index);
    }

    #[test]
    fn offline_policy_uses_embedded_index_without_fetching_missing_manifest() {
        use std::io;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind Store probe");
        listener
            .set_nonblocking(true)
            .expect("make Store probe nonblocking");
        let target = Target::detect().expect("supported test target");
        let client = client_for_policy(
            &index_with_target("offline", 1, target),
            format!(
                "http://{}/",
                listener.local_addr().expect("read probe address")
            ),
            NetworkPolicy::Offline,
        );

        client
            .index(CachePolicy::RevalidateIfStale)
            .expect("load embedded Index offline");
        let error = client
            .resolve("offline", "stable")
            .expect_err("missing manifest must fail offline");

        assert_eq!(error.class, FailureClass::Transient);
        assert_eq!(error.stage, StoreStage::Manifest);
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn signed_manifest_digest_blocks_swapped_network_bytes_and_cache_only() {
        use std::io::{Read as _, Write as _};

        let target = Target::detect().expect("supported target");
        let signing = SigningKey::from_bytes(&[17; 32]);
        let manifest = manifest_with_targets("swap", &[target]);
        let expected = sign_document(&manifest, &signing).expect("sign expected manifest");
        let mut swapped = manifest.clone();
        swapped.ttl_secs = 120;
        let swapped = sign_document(&swapped, &signing).expect("sign swapped manifest");
        let mut index = index_with_target("swap", 1, target);
        index
            .tools
            .get_mut("swap")
            .expect("Index entry")
            .manifest_sha256 = Digest::of(&expected);
        let entry = &index.tools["swap"];

        let directory =
            std::env::temp_dir().join(format!("axe-store-manifest-swap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create cache fixture");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind manifest server");
        let url = format!("http://{}/", listener.local_addr().expect("server address"));
        let mut client = client_for(&index, url);
        client.storage_roots_override = Some(vec![directory.clone()]);
        cache_signed_index(&client, &index);
        client
            .index(CachePolicy::CacheOnly)
            .expect("load signed Index");
        let expected_path = format!("/{}", entry.manifest_path());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept manifest request");
            let mut request = [0; 2048];
            let length = stream.read(&mut request).expect("read manifest request");
            assert!(
                String::from_utf8_lossy(&request[..length])
                    .starts_with(&format!("GET {expected_path} HTTP/1.1")),
                "request must use immutable digest path"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                swapped.len()
            )
            .expect("write manifest response headers");
            stream
                .write_all(&swapped)
                .expect("write swapped signed manifest");
        });

        let error = client
            .resolve("swap", "stable")
            .expect_err("reject swapped signed bytes");
        server.join().expect("join manifest server");
        assert_eq!(
            (error.stage, error.class),
            (StoreStage::Manifest, FailureClass::Integrity)
        );

        let mut offline =
            client_for_policy(&index, "http://127.0.0.1:9/".into(), NetworkPolicy::Offline);
        offline.storage_roots_override = Some(vec![directory.clone()]);
        cache_signed_manifest(&offline, entry, &expected);
        assert_eq!(
            offline
                .resolve("swap", "stable")
                .expect("matching signed cache")
                .version,
            "1.0.0"
        );

        let cache = offline
            .metadata_root()
            .expect("own cache root")
            .join("tools")
            .join(&entry.id)
            .join("manifests")
            .join(entry.manifest_sha256.to_string());
        let mut altered = manifest;
        altered.ttl_secs = 180;
        let altered = sign_document(&altered, &signing).expect("sign alternate cached manifest");
        fs::write(cache.join("content"), altered).expect("swap cached manifest");
        let error = offline
            .resolve("swap", "stable")
            .expect_err("reject swapped cache bytes");
        assert_eq!(
            (error.stage, error.class),
            (StoreStage::Manifest, FailureClass::Integrity)
        );
        fs::remove_dir_all(directory).expect("remove cache fixture");
    }

    #[test]
    fn conditional_304_never_uses_mismatched_cached_manifest() {
        use std::io::{Read as _, Write as _};

        let directory =
            std::env::temp_dir().join(format!("axe-store-manifest-304-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create cache fixture");
        let target = Target::detect().expect("supported target");
        let signing = SigningKey::from_bytes(&[17; 32]);
        let manifest = manifest_with_targets("conditional", &[target]);
        let expected = sign_document(&manifest, &signing).expect("sign expected manifest");
        let mut index = index_with_target("conditional", 1, target);
        index
            .tools
            .get_mut("conditional")
            .expect("Index entry")
            .manifest_sha256 = Digest::of(&expected);
        let entry = &index.tools["conditional"];

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind manifest server");
        let mut client = client_for(
            &index,
            format!("http://{}/", listener.local_addr().expect("server address")),
        );
        client.storage_roots_override = Some(vec![directory.clone()]);
        cache_signed_manifest(&client, entry, &expected);
        cache_signed_index(&client, &index);
        let cache = client
            .metadata_root()
            .expect("own cache root")
            .join("tools")
            .join(&entry.id)
            .join("manifests")
            .join(entry.manifest_sha256.to_string());
        let mut swapped = manifest;
        swapped.ttl_secs = 120;
        fs::write(
            cache.join("content"),
            sign_document(&swapped, &signing).expect("sign swapped manifest"),
        )
        .expect("replace cached bytes");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept manifest revalidation");
            let mut request = [0; 2048];
            let length = stream.read(&mut request).expect("read manifest request");
            assert!(
                !String::from_utf8_lossy(&request[..length])
                    .to_ascii_lowercase()
                    .contains("if-none-match")
            );
            stream
                .write_all(
                    b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write 304 response");
        });
        client
            .index(CachePolicy::CacheOnly)
            .expect("load signed embedded Index");
        let error = client
            .resolve("conditional", "stable")
            .expect_err("304 cannot restore corrupt cache");
        server.join().expect("join manifest server");

        assert_eq!(
            (error.stage, error.class),
            (StoreStage::Manifest, FailureClass::Integrity)
        );
        fs::remove_dir_all(directory).expect("remove cache fixture");
    }

    #[test]
    fn stale_memoized_index_is_revalidated() {
        use std::io::{Read as _, Write as _};

        let target = Target::detect().expect("supported target");
        let signing = SigningKey::from_bytes(&[17; 32]);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind Index server");
        let url = format!("http://{}/", listener.local_addr().expect("server address"));
        let mut config = client_for(&index_with_target("embedded", 1, target), url).config;
        config.metadata_timeout = Duration::from_secs(5);
        let mut client = Client::new(config, NetworkPolicy::Online).expect("create client");
        client.storage_roots_override = Some(Vec::new());
        let server = std::thread::spawn(move || {
            for generation in [2, 3] {
                let (mut stream, _) = listener.accept().expect("accept Index request");
                let mut request = [0; 2048];
                let length = stream.read(&mut request).expect("read Index request");
                assert!(
                    String::from_utf8_lossy(&request[..length])
                        .starts_with("GET /index.cbor.zst HTTP/1.1")
                );
                let signed =
                    sign_document(&index_with_target("remote", generation, target), &signing)
                        .expect("sign remote Index");
                let body =
                    zstd::stream::encode_all(signed.as_slice(), 3).expect("compress remote Index");
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .expect("write Index response headers");
                stream.write_all(&body).expect("write remote Index");
            }
        });
        let generation = |policy| client.index(policy).expect("load Index").index.generation;

        assert_eq!(generation(CachePolicy::CacheOnly), 1);
        assert_eq!(generation(CachePolicy::RevalidateIfStale), 2);
        assert_eq!(
            generation(CachePolicy::RevalidateIfStale),
            2,
            "a fresh memo must not refetch the Index"
        );

        {
            let mut state = client.state.lock().expect("lock Index state");
            let freshness = state
                .as_mut()
                .and_then(|memo| memo.freshness.as_mut())
                .expect("remote Index freshness");
            freshness.checked_at -= freshness.ttl;
        }
        assert_eq!(generation(CachePolicy::CacheOnly), 2);
        assert_eq!(generation(CachePolicy::RevalidateIfStale), 3);
        server.join().expect("join Index server");
    }

    #[test]
    fn fallback_root_isolates_two_editions_metadata_and_clean() {
        let directory =
            std::env::temp_dir().join(format!("axe-store-editions-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create editions fixture");
        let blocked = directory.join("explicit-store");
        fs::write(&blocked, b"not a directory").expect("block explicit Store roots");
        let shared = directory.join("fallback");
        let target = Target::detect().expect("supported target");
        let signing = SigningKey::from_bytes(&[17; 32]);
        let first_manifest =
            sign_document(&manifest_with_targets("edition-a", &[target]), &signing)
                .expect("sign first manifest");
        let second_manifest =
            sign_document(&manifest_with_targets("edition-b", &[target]), &signing)
                .expect("sign second manifest");
        let mut first_index = index_with_target("edition-a", 2, target);
        let mut second_index = index_with_target("edition-b", 3, target);
        first_index
            .tools
            .get_mut("edition-a")
            .expect("first entry")
            .manifest_sha256 = Digest::of(&first_manifest);
        second_index
            .tools
            .get_mut("edition-b")
            .expect("second entry")
            .manifest_sha256 = Digest::of(&second_manifest);
        let mut first = empty_embedded_client(
            &first_index,
            "https://one.example/store/".into(),
            NetworkPolicy::Offline,
        );
        let mut second = empty_embedded_client(
            &second_index,
            "https://two.example/store/".into(),
            NetworkPolicy::Offline,
        );
        first.storage_roots_override = Some(vec![blocked.clone(), shared.clone()]);
        second.storage_roots_override = Some(vec![blocked.clone(), shared.clone()]);
        fs::create_dir_all(&shared).expect("create fallback root");
        assert_ne!(first.metadata_namespace, second.metadata_namespace);
        assert!(
            first
                .index(CachePolicy::CacheOnly)
                .expect("empty development Index")
                .index
                .tools
                .is_empty()
        );

        for (client, index, bytes) in [
            (&first, &first_index, &first_manifest),
            (&second, &second_index, &second_manifest),
        ] {
            let metadata = client.metadata_root().expect("select fallback metadata");
            let signed = sign_document(index, &signing).expect("sign edition Index");
            let compressed =
                zstd::stream::encode_all(signed.as_slice(), 3).expect("compress edition Index");
            write_metadata(
                &metadata.join("index"),
                &compressed,
                &MetadataState {
                    etag: None,
                    checked_at: now_secs(),
                    ttl: 60,
                },
            )
            .expect("cache edition Index");
            let entry = index.tools.values().next().expect("one edition tool");
            cache_signed_manifest(client, entry, bytes);
        }

        let other_metadata = second.metadata_root().expect("second metadata root");
        let mut first = Client::new(first.config.clone(), NetworkPolicy::Offline)
            .expect("restart first edition");
        first.storage_roots_override = Some(vec![blocked.clone(), shared.clone()]);
        let mut second = Client::new(second.config.clone(), NetworkPolicy::Offline)
            .expect("restart second edition");
        second.storage_roots_override = Some(vec![blocked, shared.clone()]);
        assert_eq!(
            first
                .resolve("edition-a", "stable")
                .expect("first edition cache")
                .name,
            "edition-a"
        );
        assert_eq!(
            second
                .resolve("edition-b", "stable")
                .expect("second edition cache")
                .name,
            "edition-b"
        );
        let error = first
            .resolve("edition-b", "stable")
            .expect_err("cross-edition tool cannot leak");
        assert_eq!(
            (error.stage, error.class),
            (StoreStage::Index, FailureClass::Configuration)
        );

        first.defer_network_retry(&StoreError::transient(
            StoreStage::Index,
            "first edition failure",
        ));
        assert!(second.read_network_backoff(now_secs()).is_none());
        let objects = shared.join("objects");
        fs::create_dir_all(&objects).expect("create shared objects");
        fs::write(objects.join("object"), b"shared").expect("write shared object");
        assert_eq!(
            first.clean().expect("clean first edition"),
            vec![shared.clone()]
        );
        assert!(other_metadata.exists());
        assert!(objects.join("object").exists());

        let mut restarted = Client::new(second.config.clone(), NetworkPolicy::Offline)
            .expect("restart surviving edition");
        restarted.storage_roots_override = Some(vec![shared.clone()]);
        assert_eq!(
            restarted
                .resolve("edition-b", "stable")
                .expect("surviving edition cache")
                .name,
            "edition-b"
        );
        fs::remove_dir_all(directory).expect("remove editions fixture");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn offline_policy_rejects_missing_object_without_starting_download() {
        use axe_artifact::{ArtifactKind, Digest};
        use std::io;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind Store probe");
        listener
            .set_nonblocking(true)
            .expect("make Store probe nonblocking");
        let target = Target::detect().expect("supported test target");
        let client = client_for_policy(
            &index_with_target("offline-object", 1, target),
            format!(
                "http://{}/",
                listener.local_addr().expect("read probe address")
            ),
            NetworkPolicy::Offline,
        );
        let resolved = ResolvedTool {
            name: "offline-object".into(),
            version: "1.0.0".into(),
            target,
            artifact: Artifact {
                kind: ArtifactKind::SingleBinary,
                object_sha256: Digest::of(b"object"),
                compressed_size: 1,
                unpacked_size: 1,
                unpacked_sha256: Digest::of(b"payload"),
                fully_static: true,
            },
        };
        struct NoProgress;
        impl ProgressReporter for NoProgress {
            fn report(&mut self, _: DownloadEvent<'_>) {
                panic!("offline cache miss must not start a download");
            }
        }

        let error = match client.prepare(&resolved, &mut NoProgress) {
            Ok(_) => panic!("missing object must fail offline"),
            Err(error) => error,
        };
        assert_eq!(error.class, FailureClass::Transient);
        assert_eq!(error.stage, StoreStage::Object);
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn transient_failure_defers_network_across_clients() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let directory =
            std::env::temp_dir().join(format!("axe-store-backoff-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create backoff fixture directory");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind Store probe");
        let address = listener.local_addr().expect("read Store probe address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept Index request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read Index request");
            stream
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("write transient response");
        });

        let target = Target::detect().expect("supported test target");
        let index = index_with_target("backoff", 1, target);
        let mut first = client_for(&index, format!("http://{address}/"));
        first.storage_roots_override = Some(vec![directory.clone()]);

        let first_error = match first.fetch_index(None) {
            Ok(_) => panic!("first Index request must fail"),
            Err(error) => error,
        };
        assert_eq!(first_error.class, FailureClass::Transient);
        server.join().expect("join Store probe");

        let mut second = client_for(&index, format!("http://{address}/"));
        second.storage_roots_override = Some(vec![directory.clone()]);
        let second_error = match second.fetch_index(None) {
            Ok(_) => panic!("persistent backoff must suppress a second request"),
            Err(error) => error,
        };
        assert_eq!(second_error.class, FailureClass::Transient);
        assert!(
            second_error
                .source
                .contains("previous transient failure: index: HTTP 503"),
            "{}",
            second_error.source
        );

        fs::remove_dir_all(directory).expect("remove backoff fixture directory");
    }

    #[test]
    fn clock_rollback_marks_metadata_stale() {
        let state = MetadataState {
            etag: Some("\"etag\"".into()),
            checked_at: now_secs().saturating_add(60),
            ttl: 3600,
        };
        assert!(state.is_stale());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn empty_filesystem_candidates_use_sealed_memfd_with_ordered_progress() {
        use axe_artifact::{ArtifactKind, Digest, sign_artifact};
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let directory =
            std::env::temp_dir().join(format!("axe-store-memfd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create memfd fixture directory");
        let source = directory.join("fixture.c");
        let executable_path = directory.join("fixture");
        fs::write(&source, b"int main(void) { return 23; }\n").expect("write memfd fixture source");
        let compiled = std::process::Command::new("cc")
            .arg("-Os")
            .arg(&source)
            .arg("-o")
            .arg(&executable_path)
            .status()
            .expect("compile memfd fixture");
        assert!(compiled.success(), "compile memfd fixture");
        let executable = fs::read(&executable_path).expect("read memfd fixture");

        let signing = SigningKey::from_bytes(&[17; 32]);
        let compressed =
            zstd::stream::encode_all(executable.as_slice(), 3).expect("compress fixture");
        let mut object = std::io::Cursor::new(Vec::new());
        let written = sign_artifact(&mut compressed.as_slice(), &mut object, &signing)
            .expect("sign fixture object");
        let object = object.into_inner();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind object server");
        let address = listener.local_addr().expect("read object server address");
        let response_object = object.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept object request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read object request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_object.len()
            )
            .expect("write object response headers");
            stream
                .write_all(&response_object)
                .expect("write object response");
        });

        let target = Target::detect().expect("supported test target");
        let index = index_with_target("fixture", 1, target);
        let client = client_for(&index, format!("http://{address}/"));
        let resolved = ResolvedTool {
            name: "fixture".into(),
            version: "1.0.0".into(),
            target,
            artifact: Artifact {
                kind: ArtifactKind::SingleBinary,
                object_sha256: written.object_sha256,
                compressed_size: written.compressed_size,
                unpacked_size: executable.len() as u64,
                unpacked_sha256: Digest::of(&executable),
                fully_static: false,
            },
        };

        #[derive(Debug, Eq, PartialEq)]
        enum Event {
            Started(u64),
            Advanced(u64, u64),
            Finished(u64),
            Failed,
        }
        struct Recorder(Vec<Event>);
        impl ProgressReporter for Recorder {
            fn report(&mut self, event: DownloadEvent<'_>) {
                self.0.push(match event {
                    DownloadEvent::Started { total, .. } => Event::Started(total),
                    DownloadEvent::Advanced { downloaded, total } => {
                        Event::Advanced(downloaded, total)
                    }
                    DownloadEvent::Finished { downloaded } => Event::Finished(downloaded),
                    DownloadEvent::Failed => Event::Failed,
                });
            }
        }

        let mut progress = Recorder(Vec::new());
        let prepared = client
            .prepare(&resolved, &mut progress)
            .expect("prepare memory-backed executable");
        server.join().expect("object server thread");
        assert!(prepared.is_memory_backed());
        assert_eq!(
            progress.0,
            vec![
                Event::Started(written.compressed_size),
                Event::Advanced(written.compressed_size, written.compressed_size),
                Event::Finished(written.compressed_size),
            ]
        );

        let status = prepared.run("fixture", &[]).expect("run sealed memfd");
        assert_eq!(status.code(), Some(23));
        fs::remove_dir_all(&directory).expect("remove memfd fixture directory");
    }

    #[test]
    fn clean_skips_inaccessible_candidates_and_preserves_unowned_cache() {
        let directory =
            std::env::temp_dir().join(format!("axe-store-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).expect("create clean fixture directory");

        let blocker = directory.join("not-a-directory");
        fs::write(&blocker, b"fixture").expect("create inaccessible candidate parent");
        let store = directory.join("store");
        let objects = store.join("objects");
        fs::create_dir_all(&objects).expect("create shared object directory");
        fs::write(objects.join("object"), b"fixture").expect("create shared object");

        let target = Target::detect().expect("supported test target");
        let index = index_with_target("fixture", 1, target);
        let mut client = client_for(&index, "http://127.0.0.1/".into());
        client.storage_roots_override = Some(vec![blocker.join(".axe-store"), store.clone()]);
        let metadata = client.metadata_root().expect("select own metadata root");
        fs::create_dir_all(&metadata).expect("create managed metadata directory");
        fs::write(metadata.join("index"), b"fixture").expect("create managed metadata");

        assert_eq!(
            client.clean().expect("clean Store roots"),
            vec![store.clone()]
        );
        assert!(!metadata.exists());
        assert!(objects.join("object").exists());
        assert!(store.join("metadata").exists());
        assert!(client.clean().expect("clean absent namespace").is_empty());

        fs::remove_dir_all(directory).expect("remove clean fixture directory");
    }
}
