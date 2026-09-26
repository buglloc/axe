use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

use axe_artifact::{KeyId, StoreIndex, Target, TrustedKeys, decode_hex_32, verify_document};
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use ssh_key::PrivateKey;

const EDITION_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edition {
    schema_version: u32,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreConfig {
    consumer: ConsumerConfig,
    storage: StorageConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumerConfig {
    addresses: Vec<IpAddr>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageConfig {
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    release_prefix: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SshdConfig {
    principals: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayConfig {
    enabled_by_default: bool,
    tcp_endpoint: Option<String>,
    quic_endpoint: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapPackage {
    id: String,
    name: String,
    aliases: Vec<String>,
    synopsis: Option<String>,
    channels: BTreeMap<String, String>,
    targets: BTreeSet<Target>,
    artifact: serde_json::Value,
}

fn main() {
    if let Err(error) = run() {
        panic!("{error}\nrun 'just generate-dev-keys' or provide deployment keys/");
    }
}

fn run() -> Result<(), String> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "cannot locate workspace root".to_string())?;

    println!("cargo:rerun-if-env-changed=AXE_EDITION_ROOT");
    let edition_root = env::var_os("AXE_EDITION_ROOT")
        .filter(|root| !root.is_empty())
        .map_or_else(|| workspace.to_path_buf(), PathBuf::from);
    let edition_json = read_text(&edition_root, "edition.json")?;
    let edition: Edition = serde_json::from_str(&edition_json)
        .map_err(|error| format!("invalid edition.json: {error}"))?;
    validate_edition(&edition)?;

    let build_commit = build_commit(workspace)?;
    println!("cargo:rustc-env=AXE_BUILD_COMMIT={build_commit}");
    let build_target =
        env::var("TARGET").map_err(|error| format!("TARGET is unavailable: {error}"))?;
    println!("cargo:rustc-env=AXE_BUILD_TARGET={build_target}");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        let build_id_argument = if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") {
            "--build-id=sha1"
        } else {
            "-Wl,--build-id=sha1"
        };
        println!("cargo:rustc-link-arg={build_id_argument}");
    }

    println!(
        "cargo:rerun-if-changed={}",
        workspace.join("config/aliases.json").display()
    );
    for path in [
        "edition.json",
        "config/store.json",
        "config/sshd.json",
        "config/relay.json",
        "keys/ssh/host_ed25519",
        "keys/ssh/user_ca_keys",
        "keys/relay/token",
        "keys/relay/quic_server_cert.pem",
        "keys/relay/quic_client_cert.pem",
        "keys/relay/quic_client_key.pem",
        "keys/store/trusted",
        "store/bootstrap.json",
        "store/bootstrap-index.cbor.zst",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            edition_root.join(path).display()
        );
    }

    let store_json = read_text(&edition_root, "config/store.json")?;
    let store: StoreConfig = serde_json::from_str(&store_json)
        .map_err(|error| format!("invalid config/store.json: {error}"))?;
    validate_store(&store)?;

    let sshd_json = read_text(&edition_root, "config/sshd.json")?;
    let sshd: SshdConfig = serde_json::from_str(&sshd_json)
        .map_err(|error| format!("invalid config/sshd.json: {error}"))?;
    if sshd.principals.is_empty() || sshd.principals.iter().any(String::is_empty) {
        return Err("config/sshd.json principals must be non-empty".into());
    }

    let relay_json = read_text(&edition_root, "config/relay.json")?;
    let relay: RelayConfig = serde_json::from_str(&relay_json)
        .map_err(|error| format!("invalid config/relay.json: {error}"))?;
    validate_relay(&relay)?;

    let host_key = fs::read(edition_root.join("keys/ssh/host_ed25519")).map_err(|error| {
        format!(
            "read {}: {error}",
            edition_root.join("keys/ssh/host_ed25519").display()
        )
    })?;
    PrivateKey::from_openssh(&host_key)
        .map_err(|error| format!("invalid keys/ssh/host_ed25519: {error}"))?;
    let user_ca_keys = read_text(&edition_root, "keys/ssh/user_ca_keys")?;
    let ca_lines = user_ca_keys
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if ca_lines.is_empty()
        || ca_lines
            .iter()
            .any(|line| line.split_whitespace().count() < 2)
    {
        return Err("keys/ssh/user_ca_keys must contain OpenSSH public keys".into());
    }

    let relay_token = if relay.tcp_endpoint.is_some() {
        read_optional_text(&edition_root, "keys/relay/token")?.unwrap_or_default()
    } else {
        String::new()
    };
    if !relay_token.trim().is_empty() && relay_token.trim().len() < 32 {
        return Err("keys/relay/token must contain at least 32 bytes".into());
    }
    if relay.enabled_by_default && relay_token.trim().len() < 32 {
        return Err(
            "enabled relay default requires keys/relay/token with at least 32 bytes".into(),
        );
    }

    let (
        relay_quic_server_certificate,
        relay_quic_client_certificate,
        relay_quic_client_private_key,
    ) = if relay.quic_endpoint.is_some() {
        let server = read_optional_certificate(&edition_root, "keys/relay/quic_server_cert.pem")?;
        let client = read_optional_certificate(&edition_root, "keys/relay/quic_client_cert.pem")?;
        let key = read_optional_pkcs8_private_key(&edition_root, "keys/relay/quic_client_key.pem")?;
        if server.is_empty() || client.is_empty() || key.is_empty() {
            return Err(
                "configured QUIC relay endpoint requires a complete embedded identity".into(),
            );
        }
        (server, client, key)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    let trusted = load_trusted(&edition_root)?;
    let index_zstd = read_bootstrap_snapshot(&edition_root, &store, &trusted)?;

    let mut generated = String::new();
    writeln!(generated, "pub const EDITION_ID: &str = {:?};", edition.id).unwrap();
    writeln!(
        generated,
        "pub const STORE_CONFIG_JSON: &str = {store_json:?};"
    )
    .unwrap();
    writeln!(
        generated,
        "pub const SSHD_CONFIG_JSON: &str = {sshd_json:?};"
    )
    .unwrap();
    writeln!(
        generated,
        "pub const RELAY_CONFIG_JSON: &str = {relay_json:?};"
    )
    .unwrap();
    writeln!(generated, "pub const SSH_HOST_KEY: &[u8] = &{host_key:?};").unwrap();
    writeln!(
        generated,
        "pub const SSH_USER_CA_KEYS: &str = {user_ca_keys:?};"
    )
    .unwrap();
    writeln!(
        generated,
        "pub const RELAY_TOKEN: &str = {:?};",
        relay_token.trim()
    )
    .unwrap();
    writeln!(
        generated,
        "pub const RELAY_QUIC_SERVER_CERT_DER: &[u8] = &{relay_quic_server_certificate:?};"
    )
    .unwrap();
    writeln!(
        generated,
        "pub const RELAY_QUIC_CLIENT_CERT_DER: &[u8] = &{relay_quic_client_certificate:?};"
    )
    .unwrap();
    writeln!(
        generated,
        "pub const RELAY_QUIC_CLIENT_KEY_DER: &[u8] = &{relay_quic_client_private_key:?};"
    )
    .unwrap();
    generated.push_str("pub const STORE_TRUSTED_KEYS: &[(&str, &str)] = &[\n");
    for (id, key) in trusted {
        writeln!(generated, "    ({:?}, {:?}),", id.to_string(), hex(&key)).unwrap();
    }
    generated.push_str("];\n");
    writeln!(
        generated,
        "pub const BOOTSTRAP_INDEX_ZSTD: &[u8] = &{index_zstd:?};"
    )
    .unwrap();

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("embedded.rs");
    fs::write(&output, generated).map_err(|error| format!("write {}: {error}", output.display()))
}

fn build_commit(workspace: &Path) -> Result<String, String> {
    println!("cargo:rerun-if-env-changed=AXE_BUILD_COMMIT");

    if let Ok(commit) = env::var("AXE_BUILD_COMMIT") {
        return short_commit(commit.trim());
    }

    if let Some(head) = git_output(
        workspace,
        &["rev-parse", "--path-format=absolute", "--git-path", "HEAD"],
    ) {
        println!("cargo:rerun-if-changed={head}");
        if let Ok(contents) = fs::read_to_string(&head)
            && let Some(reference) = contents.trim().strip_prefix("ref: ")
            && let Some(reference_path) = git_output(
                workspace,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    reference,
                ],
            )
        {
            println!("cargo:rerun-if-changed={reference_path}");
        }
    }
    if let Some(packed_refs) = git_output(
        workspace,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "packed-refs",
        ],
    ) {
        println!("cargo:rerun-if-changed={packed_refs}");
    }

    match git_output(workspace, &["rev-parse", "--short=12", "HEAD"]) {
        Some(commit) => short_commit(&commit),
        None => Ok("unknown".to_owned()),
    }
}

fn short_commit(commit: &str) -> Result<String, String> {
    if !(7..=64).contains(&commit.len()) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("AXE_BUILD_COMMIT must be a 7-64 character hexadecimal commit ID".into());
    }

    Ok(commit[..commit.len().min(12)].to_ascii_lowercase())
}

fn git_output(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    String::from_utf8(output.stdout)
        .ok()
        .map(|output| output.trim().to_owned())
        .filter(|output| !output.is_empty())
}

fn validate_edition(edition: &Edition) -> Result<(), String> {
    if edition.schema_version != EDITION_SCHEMA_VERSION {
        return Err(format!(
            "edition.json schema_version {} is unsupported; expected {EDITION_SCHEMA_VERSION}",
            edition.schema_version
        ));
    }
    if edition.id.is_empty()
        || !edition
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("edition.json id must match [a-z0-9-]+".into());
    }
    Ok(())
}

fn validate_relay(config: &RelayConfig) -> Result<(), String> {
    for (name, endpoint) in [
        ("tcp_endpoint", config.tcp_endpoint.as_deref()),
        ("quic_endpoint", config.quic_endpoint.as_deref()),
    ] {
        if endpoint.is_some_and(str::is_empty) {
            return Err(format!(
                "config/relay.json {name} must be null or a non-empty string"
            ));
        }
    }
    if config.enabled_by_default && config.tcp_endpoint.is_none() {
        return Err(
            "config/relay.json enabled_by_default requires tcp_endpoint for the default transport"
                .into(),
        );
    }
    Ok(())
}

fn validate_store(config: &StoreConfig) -> Result<(), String> {
    let consumer = &config.consumer;
    let storage = &config.storage;
    if consumer.addresses.is_empty()
        || consumer.max_object_bytes == 0
        || consumer.max_metadata_bytes == 0
        || consumer.index_ttl_secs == 0
        || consumer.manifest_ttl_secs == 0
        || consumer.metadata_timeout_secs == 0
        || consumer.object_timeout_secs == 0
        || consumer.transient_retry_secs == 0
        || consumer.free_space_reserve_bytes == 0
        || consumer.channel.is_empty()
        || !storage.endpoint.starts_with("https://")
        || storage.region.is_empty()
        || storage.bucket.is_empty()
        || storage.prefix.is_empty()
        || storage.release_prefix.is_empty()
    {
        return Err("config/store.json contains an empty or invalid required value".into());
    }
    let _ = (&consumer.persistent_roots, &consumer.tmpfs_roots);
    Ok(())
}

fn read_bootstrap_snapshot(
    edition_root: &Path,
    config: &StoreConfig,
    trusted: &[(KeyId, [u8; 32])],
) -> Result<Vec<u8>, String> {
    let path = edition_root.join("store/bootstrap-index.cbor.zst");
    let compressed = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let limit = config.consumer.max_metadata_bytes;
    if compressed.len() as u64 > limit {
        return Err(format!("{} exceeds metadata size limit", path.display()));
    }
    let mut decoder = zstd::stream::read::Decoder::new(compressed.as_slice())
        .map_err(|error| format!("decompress {}: {error}", path.display()))?;
    let mut signed = Vec::new();
    decoder
        .by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut signed)
        .map_err(|error| format!("decompress {}: {error}", path.display()))?;
    if signed.len() as u64 > limit {
        return Err(format!(
            "{} expands beyond metadata size limit",
            path.display()
        ));
    }
    let mut keys = TrustedKeys::new();
    for &(id, key) in trusted {
        let key = VerifyingKey::from_bytes(&key)
            .map_err(|error| format!("invalid trusted key {id}: {error}"))?;
        keys.insert_named(id, key)
            .map_err(|error| error.to_string())?;
    }
    let index: StoreIndex = verify_document(&signed, &keys)
        .map_err(|error| format!("verify {}: {error}", path.display()))?;
    index
        .validate()
        .map_err(|error| format!("validate {}: {error}", path.display()))?;
    validate_bootstrap_inventory(edition_root, &index)?;
    Ok(compressed)
}

fn validate_bootstrap_inventory(edition_root: &Path, index: &StoreIndex) -> Result<(), String> {
    let path = edition_root.join("store/bootstrap.json");
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let packages: BTreeMap<String, BootstrapPackage> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if packages.len() != index.tools.len() {
        return Err(format!(
            "signed bootstrap Index does not match {} tool count",
            path.display()
        ));
    }
    for (attribute, package) in packages {
        if package.artifact.is_null() {
            return Err(format!("Nix package {attribute} has no artifact contract"));
        }
        let entry = index.tools.get(&package.name).ok_or_else(|| {
            format!(
                "signed bootstrap Index has no tool {:?} from Nix package {attribute}",
                package.name
            )
        })?;
        let targets = package
            .targets
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();
        let channels: BTreeMap<_, _> = package
            .channels
            .keys()
            .map(|channel| (channel.clone(), targets.clone()))
            .collect();
        if entry.id != package.id
            || entry.aliases != package.aliases
            || entry.synopsis != package.synopsis
            || entry.channels != channels
        {
            return Err(format!(
                "signed bootstrap Index metadata for {:?} differs from Nix package {attribute}",
                package.name
            ));
        }
    }
    Ok(())
}

fn load_trusted(workspace: &Path) -> Result<Vec<(KeyId, [u8; 32])>, String> {
    let directory = workspace.join("keys/store/trusted");
    let mut paths = fs::read_dir(&directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {}: {error}", directory.display()))?;
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "pub"));
    paths.sort_unstable();
    if paths.is_empty() {
        return Err("keys/store/trusted contains no public keys".into());
    }
    paths
        .into_iter()
        .map(|path| {
            let id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("invalid trusted key filename {}", path.display()))?
                .parse::<KeyId>()
                .map_err(|error| error.to_string())?;
            let key = decode_hex_32(
                fs::read_to_string(&path)
                    .map_err(|error| format!("read {}: {error}", path.display()))?
                    .trim(),
                "trusted public key",
            )
            .map_err(|error| error.to_string())?;
            VerifyingKey::from_bytes(&key)
                .map_err(|error| format!("invalid {}: {error}", path.display()))?;
            if KeyId::for_key(&VerifyingKey::from_bytes(&key).map_err(|error| error.to_string())?)
                != id
            {
                return Err(format!(
                    "trusted key filename does not match {}",
                    path.display()
                ));
            }
            Ok((id, key))
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}

fn read_optional_certificate(root: &Path, relative: &str) -> Result<Vec<u8>, String> {
    let Some(pem) = read_optional_text(root, relative)? else {
        return Ok(Vec::new());
    };
    parse_certificate(&pem, relative)
}

fn parse_certificate(pem: &str, relative: &str) -> Result<Vec<u8>, String> {
    let mut reader = pem.as_bytes();
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("invalid {relative}: {error}"))?;
    let [certificate] = certificates.as_slice() else {
        return Err(format!("{relative} must contain exactly one certificate"));
    };
    Ok(certificate.as_ref().to_vec())
}

fn read_optional_pkcs8_private_key(root: &Path, relative: &str) -> Result<Vec<u8>, String> {
    let Some(pem) = read_optional_text(root, relative)? else {
        return Ok(Vec::new());
    };
    parse_pkcs8_private_key(&pem, relative)
}

fn parse_pkcs8_private_key(pem: &str, relative: &str) -> Result<Vec<u8>, String> {
    let mut reader = pem.as_bytes();
    let private_keys = rustls_pemfile::pkcs8_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("invalid {relative}: {error}"))?;
    let [private_key] = private_keys.as_slice() else {
        return Err(format!(
            "{relative} must contain exactly one PKCS#8 private key"
        ));
    };
    Ok(private_key.secret_pkcs8_der().to_vec())
}

fn read_text(root: &Path, relative: &str) -> Result<String, String> {
    let path = root.join(relative);
    fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn read_optional_text(root: &Path, relative: &str) -> Result<Option<String>, String> {
    let path = root.join(relative);
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}
