mod store_config;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256, Sha512};

pub use store_config::{ConsumerConfig, StorageConfig, StoreConfig};

pub const SCHEMA_VERSION: u32 = 2;
/// Revalidation interval of every published Index.
pub const INDEX_TTL_SECS: u64 = 3_600;
pub const METADATA_CONTEXT: &[u8] = b"axe-metadata-v2";
pub const ARTIFACT_CONTEXT: &[u8] = b"axe-artifact-v1";
const ARTIFACT_SCHEMA_VERSION: u32 = 1;
pub const ARTIFACT_TRAILER_MAGIC: u32 = 0x184d_2a5e;
pub const ARTIFACT_TRAILER_PAYLOAD_SIZE: usize = 4 + 32 + 64;
pub const ARTIFACT_TRAILER_SIZE: usize = 8 + ARTIFACT_TRAILER_PAYLOAD_SIZE;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Cbor(String),
    Invalid(String),
    UnknownKey(KeyId),
    Signature,
    DigestMismatch { expected: Digest, actual: Digest },
    SizeMismatch { expected: u64, actual: u64 },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Cbor(error) => write!(formatter, "CBOR error: {error}"),
            Self::Invalid(error) => formatter.write_str(error),
            Self::UnknownKey(key) => write!(formatter, "metadata is signed by unknown key {key}"),
            Self::Signature => formatter.write_str("invalid Ed25519ph signature"),
            Self::DigestMismatch { expected, actual } => {
                write!(
                    formatter,
                    "SHA-256 mismatch: got {actual}, expected {expected}"
                )
            }
            Self::SizeMismatch { expected, actual } => {
                write!(
                    formatter,
                    "size mismatch: got {actual}, expected {expected}"
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    #[must_use]
    pub fn object_path(self) -> String {
        let digest = self.to_string();
        format!("objects/sha256/{}/{}", &digest[..2], digest)
    }

    pub fn from_reader(mut reader: impl Read) -> io::Result<Self> {
        let mut hash = Sha256::new();
        io::copy(&mut reader, &mut HashWriter(&mut hash))?;

        Ok(Self(hash.finalize().into()))
    }

    pub fn from_file(path: &Path) -> io::Result<Self> {
        Self::from_reader(fs::File::open(path)?)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(formatter, &self.0)
    }
}

impl FromStr for Digest {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        decode_hex_32(value, "digest").map(Self)
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeyId(pub [u8; 32]);

impl KeyId {
    #[must_use]
    pub fn for_key(key: &VerifyingKey) -> Self {
        Self(Sha256::digest(key.as_bytes()).into())
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(formatter, &self.0)
    }
}

impl FromStr for KeyId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        decode_hex_32(value, "key ID").map(Self)
    }
}

impl Serialize for KeyId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for KeyId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

fn write_hex(formatter: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }

    Ok(())
}

pub fn decode_hex_32(value: &str, label: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(Error::Invalid(format!(
            "{label} must be exactly 64 lowercase hexadecimal characters"
        )));
    }

    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        bytes[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("hex input was validated"),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Target {
    #[serde(rename = "x86_64-linux")]
    X86_64Linux,
    #[serde(rename = "aarch64-linux")]
    Aarch64Linux,
    #[serde(rename = "aarch64-darwin")]
    Aarch64Darwin,
}

impl Target {
    pub const ALL: [Self; 3] = [Self::X86_64Linux, Self::Aarch64Linux, Self::Aarch64Darwin];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-linux",
            Self::Aarch64Linux => "aarch64-linux",
            Self::Aarch64Darwin => "aarch64-darwin",
        }
    }

    #[must_use]
    pub const fn is_linux(self) -> bool {
        matches!(self, Self::X86_64Linux | Self::Aarch64Linux)
    }

    pub fn detect() -> Result<Self, Error> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => Ok(Self::X86_64Linux),
            ("linux", "aarch64") => Ok(Self::Aarch64Linux),
            ("macos", "aarch64") => Ok(Self::Aarch64Darwin),
            (os, arch) => Err(Error::Invalid(format!("unsupported target {arch}-{os}"))),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Target {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|target| target.as_str() == value)
            .ok_or_else(|| Error::Invalid(format!("unsupported target {value:?}")))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoreIndex {
    pub schema_version: u32,
    pub generation: u64,
    pub ttl_secs: u64,
    pub tools: BTreeMap<String, StoreIndexEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoreIndexEntry {
    pub id: String,
    pub aliases: Vec<String>,
    pub manifest_sha256: Digest,
    pub channels: BTreeMap<String, BTreeSet<String>>,
    pub synopsis: Option<String>,
}

impl StoreIndexEntry {
    #[must_use]
    pub fn manifest_path(&self) -> String {
        format!("tools/{}/manifests/{}.cbor", self.id, self.manifest_sha256)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub ttl_secs: u64,
    pub channels: BTreeMap<String, String>,
    pub versions: BTreeMap<String, ToolVersion>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolVersion {
    pub targets: BTreeMap<String, Artifact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub kind: ArtifactKind,
    pub object_sha256: Digest,
    pub compressed_size: u64,
    pub unpacked_size: u64,
    pub unpacked_sha256: Digest,
    pub fully_static: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactKind {
    SingleBinary,
    Package { entrypoint: String },
}

impl StoreIndex {
    pub fn validate(&self) -> Result<(), Error> {
        validate_schema(self.schema_version)?;
        if self.generation == 0 {
            return invalid("Index generation must be non-zero");
        }
        if self.ttl_secs == 0 {
            return invalid("Index TTL must be non-zero");
        }

        let mut claimed_names = BTreeSet::new();
        for (name, entry) in &self.tools {
            validate_name(name, "tool name")?;
            validate_id(&entry.id)?;
            if entry.channels.is_empty() {
                return invalid(format!("Index entry {name:?} has no channels"));
            }
            if !claimed_names.insert(name.as_str()) {
                return invalid(format!("duplicate tool name {name:?}"));
            }

            let mut aliases = BTreeSet::new();
            for alias in &entry.aliases {
                validate_name(alias, "tool alias")?;
                if alias == name || !aliases.insert(alias.as_str()) {
                    return invalid(format!("duplicate name or alias {alias:?}"));
                }
                if !claimed_names.insert(alias.as_str()) {
                    return invalid(format!("name or alias {alias:?} is claimed twice"));
                }
            }

            for (channel, targets) in &entry.channels {
                validate_name(channel, "channel")?;
                if targets.is_empty() {
                    return invalid(format!("channel {channel:?} has no targets"));
                }
                for target in targets {
                    validate_name(target, "target")?;
                }
            }
        }
        Ok(())
    }
}

impl ToolManifest {
    pub fn validate(&self) -> Result<(), Error> {
        validate_schema(self.schema_version)?;
        validate_id(&self.id)?;
        validate_name(&self.name, "tool name")?;
        if self.ttl_secs == 0 {
            return invalid("ToolManifest TTL must be non-zero");
        }
        if self.channels.is_empty() || self.versions.is_empty() {
            return invalid("ToolManifest channels and versions must be non-empty");
        }

        for (channel, version) in &self.channels {
            validate_name(channel, "channel")?;
            validate_name(version, "version")?;
            if !self.versions.contains_key(version) {
                return invalid(format!(
                    "channel {channel:?} refers to unknown version {version:?}"
                ));
            }
        }

        for (version, tool_version) in &self.versions {
            validate_name(version, "version")?;
            if tool_version.targets.is_empty() {
                return invalid(format!("version {version:?} has no targets"));
            }
            for (target, artifact) in &tool_version.targets {
                validate_name(target, "target")?;
                if let Ok(target) = target.parse() {
                    artifact.validate(target)?;
                } else {
                    artifact.validate_common()?;
                }
            }
        }

        Ok(())
    }

    pub fn validate_index_identity(
        &self,
        canonical_name: &str,
        entry: &StoreIndexEntry,
    ) -> Result<(), Error> {
        if self.id != entry.id || self.name != canonical_name {
            return invalid("Index entry and ToolManifest identity mismatch");
        }
        Ok(())
    }
}

impl Artifact {
    pub fn validate(&self, target: Target) -> Result<(), Error> {
        self.validate_common()?;
        if matches!(self.kind, ArtifactKind::SingleBinary)
            && self.fully_static
            && !target.is_linux()
        {
            return invalid("fully_static is only valid for Linux single binaries");
        }
        Ok(())
    }

    fn validate_common(&self) -> Result<(), Error> {
        if self.compressed_size == 0 || self.unpacked_size == 0 {
            return invalid("artifact sizes must be non-zero");
        }
        if let ArtifactKind::Package { entrypoint } = &self.kind {
            validate_relative_path(entrypoint, "package entrypoint")?;
            if self.fully_static {
                return invalid("packages cannot be marked fully_static");
            }
        }
        Ok(())
    }
}

fn validate_schema(schema: u32) -> Result<(), Error> {
    if schema == SCHEMA_VERSION {
        Ok(())
    } else {
        invalid(format!("unsupported schema version {schema}"))
    }
}

fn validate_id(id: &str) -> Result<(), Error> {
    validate_relative_path(id, "tool ID")?;
    for segment in id.split('/') {
        validate_name(segment, "tool ID segment")?;
    }
    Ok(())
}

fn validate_name(value: &str, label: &str) -> Result<(), Error> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return invalid(format!("invalid {label} {value:?}"));
    }
    Ok(())
}

pub fn validate_relative_path(value: &str, label: &str) -> Result<(), Error> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return invalid(format!("invalid {label} {value:?}"));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(message.into()))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedDocument {
    schema_version: u32,
    payload: Vec<u8>,
    key_id: KeyId,
    signature: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct TrustedKeys(BTreeMap<KeyId, VerifyingKey>);

impl TrustedKeys {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: VerifyingKey) -> KeyId {
        let id = KeyId::for_key(&key);
        self.0.insert(id, key);
        id
    }

    pub fn insert_named(&mut self, id: KeyId, key: VerifyingKey) -> Result<(), Error> {
        let actual = KeyId::for_key(&key);
        if actual != id {
            return invalid(format!(
                "public key digest {actual} does not match key ID {id}"
            ));
        }
        self.0.insert(id, key);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: &KeyId) -> Option<&VerifyingKey> {
        self.0.get(id)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = &KeyId> {
        self.0.keys()
    }
}

pub fn sign_document<T: Serialize>(value: &T, key: &SigningKey) -> Result<Vec<u8>, Error> {
    let mut payload = Vec::new();
    ciborium::ser::into_writer(value, &mut payload)
        .map_err(|error| Error::Cbor(error.to_string()))?;

    let signature = key
        .sign_prehashed(Sha512::new_with_prefix(&payload), Some(METADATA_CONTEXT))
        .map_err(|_| Error::Signature)?;
    let document = SignedDocument {
        schema_version: SCHEMA_VERSION,
        payload,
        key_id: KeyId::for_key(&key.verifying_key()),
        signature: signature.to_bytes().to_vec(),
    };

    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&document, &mut bytes)
        .map_err(|error| Error::Cbor(error.to_string()))?;
    Ok(bytes)
}

pub fn verify_document<T: DeserializeOwned>(bytes: &[u8], keys: &TrustedKeys) -> Result<T, Error> {
    let document: SignedDocument =
        ciborium::de::from_reader(bytes).map_err(|error| Error::Cbor(error.to_string()))?;
    validate_schema(document.schema_version)?;

    let key = keys
        .get(&document.key_id)
        .ok_or(Error::UnknownKey(document.key_id))?;
    let signature = Signature::from_slice(&document.signature).map_err(|_| Error::Signature)?;
    key.verify_prehashed(
        Sha512::new_with_prefix(&document.payload),
        Some(METADATA_CONTEXT),
        &signature,
    )
    .map_err(|_| Error::Signature)?;

    ciborium::de::from_reader(document.payload.as_slice())
        .map_err(|error| Error::Cbor(error.to_string()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WrittenArtifact {
    pub object_sha256: Digest,
    pub compressed_size: u64,
    pub payload_size: u64,
    pub key_id: KeyId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedArtifact {
    pub payload_size: u64,
    pub key_id: KeyId,
}

pub fn sign_artifact<R, W>(
    input: &mut R,
    output: &mut W,
    key: &SigningKey,
) -> Result<WrittenArtifact, Error>
where
    R: Read,
    W: Write + Seek,
{
    output.seek(SeekFrom::Start(0))?;

    let mut object_hash = Sha256::new();
    let mut signature_hash = Sha512::new();
    let mut payload_size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        object_hash.update(&buffer[..read]);
        signature_hash.update(&buffer[..read]);
        payload_size = payload_size
            .checked_add(read as u64)
            .ok_or_else(|| Error::Invalid("artifact size overflow".into()))?;
    }

    if payload_size == 0 {
        return invalid("artifact payload must be non-empty");
    }

    let key_id = KeyId::for_key(&key.verifying_key());
    let signature = key
        .sign_prehashed(signature_hash, Some(ARTIFACT_CONTEXT))
        .map_err(|_| Error::Signature)?;
    let mut trailer = [0_u8; ARTIFACT_TRAILER_SIZE];
    trailer[..4].copy_from_slice(&ARTIFACT_TRAILER_MAGIC.to_le_bytes());
    trailer[4..8].copy_from_slice(&(ARTIFACT_TRAILER_PAYLOAD_SIZE as u32).to_le_bytes());
    trailer[8..12].copy_from_slice(&ARTIFACT_SCHEMA_VERSION.to_le_bytes());
    trailer[12..44].copy_from_slice(&key_id.0);
    trailer[44..].copy_from_slice(&signature.to_bytes());
    output.write_all(&trailer)?;
    output.flush()?;

    object_hash.update(trailer);

    let compressed_size = payload_size + ARTIFACT_TRAILER_SIZE as u64;
    Ok(WrittenArtifact {
        object_sha256: Digest(object_hash.finalize().into()),
        compressed_size,
        payload_size,
        key_id,
    })
}

pub fn verify_artifact<R>(
    reader: &mut R,
    keys: &TrustedKeys,
    expected_digest: Digest,
    expected_size: u64,
) -> Result<VerifiedArtifact, Error>
where
    R: Read + Seek,
{
    let actual_size = reader.seek(SeekFrom::End(0))?;
    if actual_size != expected_size {
        return Err(Error::SizeMismatch {
            expected: expected_size,
            actual: actual_size,
        });
    }
    if actual_size <= ARTIFACT_TRAILER_SIZE as u64 {
        return invalid("artifact is too short for signature trailer");
    }

    reader.seek(SeekFrom::Start(0))?;
    let payload_size = actual_size - ARTIFACT_TRAILER_SIZE as u64;
    let mut object_hash = Sha256::new();
    let mut signature_hash = Sha512::new();
    let mut remaining_payload = payload_size;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining_payload != 0 {
        let wanted = usize::try_from(remaining_payload.min(buffer.len() as u64))
            .expect("bounded read size fits usize");
        let read = reader.read(&mut buffer[..wanted])?;
        if read == 0 {
            return invalid("truncated artifact payload");
        }
        object_hash.update(&buffer[..read]);
        signature_hash.update(&buffer[..read]);
        remaining_payload -= read as u64;
    }

    let mut trailer = [0_u8; ARTIFACT_TRAILER_SIZE];
    reader.read_exact(&mut trailer)?;
    object_hash.update(trailer);
    let actual_digest = Digest(object_hash.finalize().into());
    if actual_digest != expected_digest {
        return Err(Error::DigestMismatch {
            expected: expected_digest,
            actual: actual_digest,
        });
    }

    let magic = u32::from_le_bytes(trailer[..4].try_into().expect("four-byte field"));
    let frame_size = u32::from_le_bytes(trailer[4..8].try_into().expect("four-byte field"));
    let schema = u32::from_le_bytes(trailer[8..12].try_into().expect("four-byte field"));
    if magic != ARTIFACT_TRAILER_MAGIC || frame_size as usize != ARTIFACT_TRAILER_PAYLOAD_SIZE {
        return invalid("invalid artifact signature frame");
    }
    if schema != ARTIFACT_SCHEMA_VERSION {
        return invalid(format!("unsupported artifact schema version {schema}"));
    }

    let key_id = KeyId(trailer[12..44].try_into().expect("32-byte field"));
    let signature = Signature::from_slice(&trailer[44..]).map_err(|_| Error::Signature)?;
    let key = keys.get(&key_id).ok_or(Error::UnknownKey(key_id))?;
    key.verify_prehashed(signature_hash, Some(ARTIFACT_CONTEXT), &signature)
        .map_err(|_| Error::Signature)?;

    reader.seek(SeekFrom::Start(0))?;
    Ok(VerifiedArtifact {
        payload_size,
        key_id,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalTreeEntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Debug)]
pub struct CanonicalTreeEntry {
    relative_path: String,
    source: PathBuf,
    kind: CanonicalTreeEntryKind,
    mode: u32,
    size: u64,
    link_target: Option<String>,
}

impl CanonicalTreeEntry {
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn kind(&self) -> CanonicalTreeEntryKind {
        self.kind
    }

    pub fn mode(&self) -> u32 {
        self.mode
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn link_target(&self) -> Option<&str> {
        self.link_target.as_deref()
    }
}

#[derive(Debug)]
pub struct CanonicalTree {
    entries: Vec<CanonicalTreeEntry>,
}

impl CanonicalTree {
    pub fn collect(root: &Path) -> Result<Self, Error> {
        let mut entries = Vec::new();
        collect_tree(root, root, &mut entries)?;

        entries.sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(Self { entries })
    }

    pub fn digest(&self) -> Result<Digest, Error> {
        let mut hash = Sha256::new();
        for entry in &self.entries {
            let path_bytes = entry.relative_path.as_bytes();
            hash.update((path_bytes.len() as u32).to_le_bytes());
            hash.update(path_bytes);

            match entry.kind {
                CanonicalTreeEntryKind::Directory => {
                    hash.update(*b"d");
                    hash.update(entry.mode.to_le_bytes());
                    hash.update(0_u64.to_le_bytes());
                }
                CanonicalTreeEntryKind::File => {
                    hash.update(*b"f");
                    hash.update(entry.mode.to_le_bytes());
                    hash.update(entry.size.to_le_bytes());
                    let mut file = fs::File::open(&entry.source)?;
                    io::copy(&mut file, &mut HashWriter(&mut hash))?;
                }
                CanonicalTreeEntryKind::Symlink => {
                    let target = entry
                        .link_target
                        .as_deref()
                        .expect("symlink entries have link targets");
                    hash.update(*b"l");
                    hash.update(entry.mode.to_le_bytes());
                    hash.update((target.len() as u64).to_le_bytes());
                    hash.update(target.as_bytes());
                }
            }
        }
        Ok(Digest(hash.finalize().into()))
    }

    pub fn entries(&self) -> &[CanonicalTreeEntry] {
        &self.entries
    }
}

pub fn canonical_tree_digest(root: &Path) -> Result<Digest, Error> {
    CanonicalTree::collect(root)?.digest()
}

fn collect_tree(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<CanonicalTreeEntry>,
) -> Result<(), Error> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let source = entry.path();
        let relative_path = source
            .strip_prefix(root)
            .map_err(|_| Error::Invalid("tree path escaped root".into()))?
            .to_str()
            .ok_or_else(|| Error::Invalid("package path is not UTF-8".into()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_relative_path(&relative_path, "package path")?;

        let metadata = fs::symlink_metadata(&source)?;
        let mode = canonical_mode(&metadata);
        let (kind, size, link_target) = if metadata.is_dir() {
            (CanonicalTreeEntryKind::Directory, 0, None)
        } else if metadata.is_file() {
            (CanonicalTreeEntryKind::File, metadata.len(), None)
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&source)?;
            validate_safe_link(root, &source, &target)?;
            let target = target
                .to_str()
                .ok_or_else(|| Error::Invalid("non-UTF-8 symlink target".into()))?
                .to_owned();
            (CanonicalTreeEntryKind::Symlink, 0, Some(target))
        } else {
            return invalid(format!("unsupported filesystem entry {}", source.display()));
        };

        if kind == CanonicalTreeEntryKind::Directory {
            collect_tree(root, &source, entries)?;
        }

        entries.push(CanonicalTreeEntry {
            relative_path,
            source,
            kind,
            mode,
            size,
            link_target,
        });
    }
    Ok(())
}

fn canonical_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

fn validate_safe_link(root: &Path, source: &Path, target: &Path) -> Result<(), Error> {
    if target.is_absolute() {
        return invalid(format!("absolute symlink target in {}", source.display()));
    }

    let parent = source
        .parent()
        .ok_or_else(|| Error::Invalid("symlink has no parent".into()))?;
    let joined = parent.join(target);

    let mut depth = 0_usize;
    for component in joined
        .strip_prefix(root)
        .map_err(|_| Error::Invalid("symlink escaped package root".into()))?
        .components()
    {
        match component {
            Component::Normal(_) => depth += 1,
            Component::ParentDir if depth != 0 => depth -= 1,
            Component::ParentDir => return invalid("symlink escaped package root"),
            Component::CurDir => {}
            _ => return invalid("invalid symlink target"),
        }
    }
    Ok(())
}

struct HashWriter<'a, D>(&'a mut D);

impl<D: sha2::digest::Update> Write for HashWriter<'_, D> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (SigningKey, TrustedKeys) {
        let signing = SigningKey::from_bytes(&[7_u8; 32]);
        let mut trusted = TrustedKeys::new();
        trusted.insert(signing.verifying_key());
        (signing, trusted)
    }

    #[test]
    fn signed_index_round_trip_rejects_mutation() {
        let (signing, trusted) = keys();
        let index = StoreIndex {
            schema_version: SCHEMA_VERSION,
            generation: 1,
            ttl_secs: 60,
            tools: BTreeMap::from([(
                "tool".into(),
                StoreIndexEntry {
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

        let bytes = sign_document(&index, &signing).expect("sign Index");
        let decoded: StoreIndex = verify_document(&bytes, &trusted).expect("verify Index");
        assert_eq!(decoded, index);
        decoded
            .validate()
            .expect("ignore unknown non-local Index target");

        let mut modified = bytes;
        let middle = modified.len() / 2;
        modified[middle] ^= 1;
        assert!(verify_document::<StoreIndex>(&modified, &trusted).is_err());

        let legacy_index = StoreIndex {
            schema_version: 1,
            ..index
        };
        let legacy_payload = sign_document(&legacy_index, &signing).expect("sign legacy payload");
        let decoded: StoreIndex =
            verify_document(&legacy_payload, &trusted).expect("verify signed envelope");
        assert!(
            decoded.validate().is_err(),
            "v1 Index payload is unsupported"
        );
        let mut legacy_envelope: SignedDocument =
            ciborium::de::from_reader(legacy_payload.as_slice()).expect("decode signed envelope");
        legacy_envelope.schema_version = 1;
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&legacy_envelope, &mut bytes).expect("encode legacy envelope");
        assert!(matches!(
            verify_document::<StoreIndex>(&bytes, &trusted),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn metadata_types_round_trip_and_reject_untrusted_or_unknown_fields() {
        let (signing, trusted) = keys();
        let artifact = Artifact {
            kind: ArtifactKind::Package {
                entrypoint: "bin/tool".into(),
            },
            object_sha256: Digest([1; 32]),
            compressed_size: 123,
            unpacked_size: 456,
            unpacked_sha256: Digest([2; 32]),
            fully_static: false,
        };

        let manifest = ToolManifest {
            schema_version: SCHEMA_VERSION,
            id: "cli/tool".into(),
            name: "tool".into(),
            ttl_secs: 60,
            channels: BTreeMap::from([("stable".into(), "1.0.0".into())]),
            versions: BTreeMap::from([(
                "1.0.0".into(),
                ToolVersion {
                    targets: BTreeMap::from([(Target::X86_64Linux.to_string(), artifact.clone())]),
                },
            )]),
        };

        for bytes in [
            sign_document(&manifest, &signing).expect("sign manifest"),
            sign_document(&artifact, &signing).expect("sign artifact metadata"),
        ] {
            assert!(verify_document::<ciborium::Value>(&bytes, &trusted).is_ok());
            assert!(matches!(
                verify_document::<ciborium::Value>(&bytes, &TrustedKeys::new()),
                Err(Error::UnknownKey(_))
            ));
        }
        assert_eq!(
            verify_document::<ToolManifest>(
                &sign_document(&manifest, &signing).expect("sign manifest"),
                &trusted,
            )
            .expect("verify manifest"),
            manifest
        );

        let mut forward_compatible = manifest.clone();
        forward_compatible
            .versions
            .get_mut("1.0.0")
            .expect("fixture version")
            .targets
            .insert("retired-target".into(), artifact.clone());
        let decoded: ToolManifest = verify_document(
            &sign_document(&forward_compatible, &signing)
                .expect("sign manifest with retired target"),
            &trusted,
        )
        .expect("decode manifest with retired target");
        decoded
            .validate()
            .expect("ignore unknown non-local target contract");

        assert_eq!(
            verify_document::<Artifact>(
                &sign_document(&artifact, &signing).expect("sign artifact metadata"),
                &trusted,
            )
            .expect("verify artifact metadata"),
            artifact
        );

        #[derive(Serialize)]
        struct IndexWithExtra {
            schema_version: u32,
            generation: u64,
            ttl_secs: u64,
            tools: BTreeMap<String, StoreIndexEntry>,
            unexpected: bool,
        }
        let extra = IndexWithExtra {
            schema_version: SCHEMA_VERSION,
            generation: 1,
            ttl_secs: 60,
            tools: BTreeMap::new(),
            unexpected: true,
        };
        let bytes = sign_document(&extra, &signing).expect("sign non-canonical Index");
        assert!(matches!(
            verify_document::<StoreIndex>(&bytes, &trusted),
            Err(Error::Cbor(_))
        ));

        let unsafe_package = Artifact {
            kind: ArtifactKind::Package {
                entrypoint: "../escape".into(),
            },
            ..artifact
        };
        assert!(unsafe_package.validate(Target::X86_64Linux).is_err());
    }

    fn signed_object(signing: &SigningKey) -> (Vec<u8>, WrittenArtifact) {
        let compressed = zstd::stream::encode_all(b"payload".as_slice(), 3).expect("compress");
        let mut object = io::Cursor::new(Vec::new());

        let written =
            sign_artifact(&mut compressed.as_slice(), &mut object, signing).expect("sign artifact");
        (object.into_inner(), written)
    }

    fn verify_exact(object: Vec<u8>, trusted: &TrustedKeys) -> Result<VerifiedArtifact, Error> {
        let digest = Digest::of(&object);
        let size = object.len() as u64;
        verify_artifact(&mut io::Cursor::new(object), trusted, digest, size)
    }

    #[test]
    fn artifact_verification_rejects_every_trailer_and_content_failure() {
        let (signing, trusted) = keys();
        let (object, written) = signed_object(&signing);

        assert!(matches!(
            verify_artifact(
                &mut io::Cursor::new(object.clone()),
                &trusted,
                Digest([0; 32]),
                written.compressed_size,
            ),
            Err(Error::DigestMismatch { .. })
        ));
        assert!(matches!(
            verify_artifact(
                &mut io::Cursor::new(object.clone()),
                &trusted,
                written.object_sha256,
                written.compressed_size + 1,
            ),
            Err(Error::SizeMismatch { .. })
        ));
        assert!(matches!(
            verify_artifact(
                &mut io::Cursor::new(object.clone()),
                &TrustedKeys::new(),
                written.object_sha256,
                written.compressed_size,
            ),
            Err(Error::UnknownKey(_))
        ));
        assert!(matches!(
            verify_exact(vec![0; ARTIFACT_TRAILER_SIZE], &trusted),
            Err(Error::Invalid(_))
        ));

        let trailer = object.len() - ARTIFACT_TRAILER_SIZE;
        let mut wrong_magic = object.clone();
        wrong_magic[trailer] ^= 1;
        assert!(matches!(
            verify_exact(wrong_magic, &trusted),
            Err(Error::Invalid(_))
        ));

        let mut wrong_version = object.clone();
        wrong_version[trailer + 8..trailer + 12].copy_from_slice(&2_u32.to_le_bytes());
        assert!(matches!(
            verify_exact(wrong_version, &trusted),
            Err(Error::Invalid(_))
        ));

        let mut bad_signature = object;
        let last = bad_signature.len() - 1;
        bad_signature[last] ^= 1;
        assert!(matches!(
            verify_exact(bad_signature, &trusted),
            Err(Error::Signature)
        ));

        let compressed = zstd::stream::encode_all(b"context".as_slice(), 3).expect("compress");
        let signature = signing
            .sign_prehashed(Sha512::new_with_prefix(&compressed), Some(METADATA_CONTEXT))
            .expect("sign with wrong context");
        let mut wrong_context = compressed;
        wrong_context.extend_from_slice(&ARTIFACT_TRAILER_MAGIC.to_le_bytes());
        wrong_context.extend_from_slice(&(ARTIFACT_TRAILER_PAYLOAD_SIZE as u32).to_le_bytes());
        wrong_context.extend_from_slice(&ARTIFACT_SCHEMA_VERSION.to_le_bytes());
        wrong_context.extend_from_slice(&KeyId::for_key(&signing.verifying_key()).0);
        wrong_context.extend_from_slice(&signature.to_bytes());
        assert!(matches!(
            verify_exact(wrong_context, &trusted),
            Err(Error::Signature)
        ));
    }

    #[test]
    fn artifact_trailer_is_signed_and_zstd_compatible() {
        let (signing, trusted) = keys();
        let executable = b"#!/bin/sh\nprintf ok\n";
        let compressed = zstd::stream::encode_all(executable.as_slice(), 3).expect("compress");
        let mut object = io::Cursor::new(Vec::new());
        let written = sign_artifact(&mut compressed.as_slice(), &mut object, &signing)
            .expect("sign artifact");
        verify_artifact(
            &mut object,
            &trusted,
            written.object_sha256,
            written.compressed_size,
        )
        .expect("verify artifact");

        object.set_position(0);
        let decoded = zstd::stream::decode_all(&mut object).expect("decode ordinary zstd stream");
        assert_eq!(decoded, executable);

        let end = object.get_mut().len() - 1;
        object.get_mut()[end] ^= 1;
        assert!(
            verify_artifact(
                &mut object,
                &trusted,
                written.object_sha256,
                written.compressed_size
            )
            .is_err()
        );
    }
}
