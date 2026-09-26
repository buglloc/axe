use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use axe_artifact::{
    ArtifactKind, Digest, canonical_tree_digest, validate_relative_path, verify_artifact,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    Client, DownloadEvent, FailureClass, ProgressReporter, ResolvedTool, StoreError, StoreStage,
    classify_http, ensure_writable_root, storage_error,
};

const MINIMUM_RESERVE: u64 = 256 * 1024 * 1024;
const VERIFIED_STATE_SCHEMA: u8 = 2;
const VERIFIED_STATE_FILE: &str = "verified.json";

pub struct PreparedTool {
    executable: Executable,
}

enum Executable {
    Path(PathBuf),
    #[cfg(target_os = "linux")]
    Memory(File),
    #[cfg(not(target_os = "linux"))]
    Temporary(PathBuf),
}

impl PreparedTool {
    pub fn run(&self, name: &str, args: &[OsString]) -> Result<ExitStatus, StoreError> {
        let ca_bundle = (name == "curl")
            .then(CaBundle::create)
            .transpose()
            .map_err(|error| StoreError::transient(StoreStage::Execute, error.to_string()))?;
        let result = match &self.executable {
            Executable::Path(path) => run_path(path, name, args, ca_bundle.as_ref()),
            #[cfg(target_os = "linux")]
            Executable::Memory(file) => run_memfd(file, name, args, ca_bundle.as_ref()),
            #[cfg(not(target_os = "linux"))]
            Executable::Temporary(path) => run_path(path, name, args, ca_bundle.as_ref()),
        };
        result.map_err(|error| StoreError::transient(StoreStage::Execute, error.to_string()))
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn is_memory_backed(&self) -> bool {
        matches!(self.executable, Executable::Memory(_))
    }
}

impl Drop for PreparedTool {
    fn drop(&mut self) {
        #[cfg(not(target_os = "linux"))]
        if let Executable::Temporary(path) = &self.executable {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) fn prepare(
    client: &Client,
    resolved: &ResolvedTool,
    progress: &mut dyn ProgressReporter,
) -> Result<PreparedTool, StoreError> {
    let (mut object, object_corruption) = find_cached_object(client, resolved)?;
    if object.is_none() {
        match download_object(client, resolved, progress) {
            Ok(downloaded) => object = Some(downloaded),
            Err(error) => return Err(object_corruption.unwrap_or(error)),
        }
    }
    let mut object = object.expect("cache or download produced object");

    match &resolved.artifact.kind {
        ArtifactKind::SingleBinary => {
            prepare_single(client, resolved, &mut object.file, object.payload_size)
        }
        ArtifactKind::Package { entrypoint } => prepare_package(
            client,
            resolved,
            entrypoint,
            &mut object.file,
            object.payload_size,
        ),
    }
}

struct ObjectFile {
    file: File,
    payload_size: u64,
}

fn find_cached_object(
    client: &Client,
    resolved: &ResolvedTool,
) -> Result<(Option<ObjectFile>, Option<StoreError>), StoreError> {
    let relative = object_relative(resolved.artifact.object_sha256);
    let mut corruption = None;
    for root in client.storage_roots() {
        let cache = root.join(&relative);
        match load_cached_object(client, resolved, &cache) {
            Ok(Some(object)) => return Ok((Some(object), corruption)),
            Ok(None) => {}
            Err(error) if error.class == FailureClass::Integrity => {
                let _ = remove_cache_entry(&cache);
                corruption.get_or_insert(error);
            }
            Err(_) => {}
        }
    }
    Ok((None, corruption))
}

fn load_cached_object(
    client: &Client,
    resolved: &ResolvedTool,
    cache: &Path,
) -> Result<Option<ObjectFile>, StoreError> {
    if !cache.exists() {
        return Ok(None);
    }
    ensure_private_cache_directory(cache)?;
    let path = cache.join("object");
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage_error(&path, error)),
    };
    if let Some(state) = read_matching_state(cache, &path, &client.metadata_namespace)
        && let Some(payload_size) = state.payload_size
    {
        return Ok(Some(ObjectFile { file, payload_size }));
    }

    let verified = verify_artifact(
        &mut file,
        &client.config.trusted_keys,
        resolved.artifact.object_sha256,
        resolved.artifact.compressed_size,
    )
    .map_err(|error| {
        StoreError::integrity(
            StoreStage::Object,
            format!("corrupt cached object {}: {error}", path.display()),
        )
    })?;
    write_verified_state(
        cache,
        &path,
        Some(verified.payload_size),
        &client.metadata_namespace,
    )?;
    Ok(Some(ObjectFile {
        file,
        payload_size: verified.payload_size,
    }))
}

fn download_object(
    client: &Client,
    resolved: &ResolvedTool,
    progress: &mut dyn ProgressReporter,
) -> Result<ObjectFile, StoreError> {
    let required = resolved
        .artifact
        .compressed_size
        .checked_add(client.config.free_space_reserve_bytes.max(MINIMUM_RESERVE))
        .ok_or_else(|| StoreError::configuration(StoreStage::Storage, "required space overflow"))?;
    let relative = object_relative(resolved.artifact.object_sha256);
    let mut storage_errors = Vec::new();
    for root in client.storage_roots() {
        if let Err(error) = ensure_writable_root(&root, required) {
            storage_errors.push(error);
            continue;
        }
        let destination = root.join(&relative);
        let parent = destination.parent().expect("object path has parent");
        if let Err(error) = fs::create_dir_all(parent) {
            storage_errors.push(storage_error(parent, error));
            continue;
        }
        let lock_path = root
            .join("locks")
            .join(format!("object-{}.lock", resolved.artifact.object_sha256));
        let _guard = match acquire_lock(&lock_path) {
            Ok(guard) => guard,
            Err(error) => {
                storage_errors.push(error);
                continue;
            }
        };
        match load_cached_object(client, resolved, &destination) {
            Ok(Some(object)) => return Ok(object),
            Ok(None) => {}
            Err(error) if error.class == FailureClass::Integrity => {
                if let Err(remove_error) = remove_cache_entry(&destination) {
                    storage_errors.push(storage_error(&destination, remove_error));
                    continue;
                }
            }
            Err(error) => {
                storage_errors.push(error);
                continue;
            }
        }

        let temporary = parent.join(format!(".{}.download", resolved.artifact.object_sha256));
        if let Err(error) = remove_cache_entry(&temporary) {
            storage_errors.push(storage_error(&temporary, error));
            continue;
        }
        if let Err(error) = ensure_private_cache_directory(&temporary) {
            storage_errors.push(error);
            continue;
        }
        let mut cleanup = CleanupPath::new(temporary.clone());
        let temporary_object = temporary.join("object");
        let mut file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temporary_object)
        {
            Ok(file) => file,
            Err(error) => {
                storage_errors.push(storage_error(&temporary_object, error));
                continue;
            }
        };
        let result: Result<ObjectFile, StoreError> = (|| {
            download_into(client, resolved, &mut file, progress)?;
            file.sync_all()
                .map_err(|error| storage_error(&temporary_object, error))?;
            let verified = verify_artifact(
                &mut file,
                &client.config.trusted_keys,
                resolved.artifact.object_sha256,
                resolved.artifact.compressed_size,
            )
            .map_err(|error| StoreError::integrity(StoreStage::Object, error.to_string()))?;
            write_verified_state(
                &temporary,
                &temporary_object,
                Some(verified.payload_size),
                &client.metadata_namespace,
            )?;
            sync_directory(&temporary)?;
            remove_cache_entry(&destination).map_err(|error| storage_error(&destination, error))?;
            fs::rename(&temporary, &destination)
                .map_err(|error| storage_error(&destination, error))?;
            sync_directory(parent)?;
            cleanup.disarm();
            file.seek(SeekFrom::Start(0))
                .map_err(|error| storage_error(&destination, error))?;
            Ok(ObjectFile {
                file,
                payload_size: verified.payload_size,
            })
        })();
        match result {
            Ok(object) => return Ok(object),
            Err(error)
                if error.class == FailureClass::Transient && error.stage == StoreStage::Storage =>
            {
                storage_errors.push(error);
            }
            Err(error) => return Err(error),
        }
    }

    #[cfg(target_os = "linux")]
    drop(storage_errors);
    #[cfg(target_os = "linux")]
    {
        let mut file = create_memfd("axe-store-object", false)
            .map_err(|error| StoreError::transient(StoreStage::Storage, error.to_string()))?;
        download_into(client, resolved, &mut file, progress)?;
        let verified = verify_artifact(
            &mut file,
            &client.config.trusted_keys,
            resolved.artifact.object_sha256,
            resolved.artifact.compressed_size,
        )
        .map_err(|error| StoreError::integrity(StoreStage::Object, error.to_string()))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| StoreError::transient(StoreStage::Storage, error.to_string()))?;
        Ok(ObjectFile {
            file,
            payload_size: verified.payload_size,
        })
    }

    #[cfg(not(target_os = "linux"))]
    Err(storage_errors.pop().unwrap_or_else(|| {
        StoreError::transient(StoreStage::Storage, "no writable object storage backend")
    }))
}

fn download_into(
    client: &Client,
    resolved: &ResolvedTool,
    destination: &mut File,
    progress: &mut dyn ProgressReporter,
) -> Result<(), StoreError> {
    client.before_network(StoreStage::Object)?;

    let total = resolved.artifact.compressed_size;
    progress.report(DownloadEvent::Started {
        name: &resolved.name,
        version: &resolved.version,
        target: resolved.target,
        total,
    });
    let result = (|| {
        let url = format!(
            "{}{}",
            client.config.public_base_url,
            resolved.artifact.object_sha256.object_path()
        );
        let mut response = client
            .object_agent
            .get(&url)
            .header("User-Agent", concat!("axe/", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(|error| classify_http(StoreStage::Object, error))?;
        let status = response.status().as_u16();
        if status == 404 || status == 410 {
            return Err(StoreError::integrity(
                StoreStage::Object,
                format!("HTTP {status} for signed object path"),
            ));
        }
        if status == 408 || status == 429 || status >= 500 {
            return Err(StoreError::transient(
                StoreStage::Object,
                format!("HTTP {status}"),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(StoreError::configuration(
                StoreStage::Object,
                format!("HTTP {status}"),
            ));
        }
        if total > client.config.max_object_bytes
            || response
                .body()
                .content_length()
                .is_some_and(|length| length != total || length > client.config.max_object_bytes)
        {
            return Err(StoreError::integrity(
                StoreStage::Object,
                "object Content-Length violates signed size or configured limit",
            ));
        }

        destination
            .seek(SeekFrom::Start(0))
            .map_err(|error| storage_error(Path::new("object temporary file"), error))?;
        let mut reader = response.body_mut().as_reader();
        let mut downloaded = 0_u64;
        let mut last_report = Instant::now();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|error| {
                StoreError::transient(StoreStage::Object, format!("read object response: {error}"))
            })?;
            if read == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(read as u64)
                .ok_or_else(|| StoreError::integrity(StoreStage::Object, "object size overflow"))?;
            if downloaded > total || downloaded > client.config.max_object_bytes {
                return Err(StoreError::integrity(
                    StoreStage::Object,
                    "object exceeds signed size",
                ));
            }
            destination
                .write_all(&buffer[..read])
                .map_err(|error| storage_error(Path::new("object temporary file"), error))?;
            if last_report.elapsed() >= Duration::from_millis(100) || downloaded == total {
                progress.report(DownloadEvent::Advanced { downloaded, total });
                last_report = Instant::now();
            }
        }
        if downloaded != total {
            return Err(StoreError::transient(
                StoreStage::Object,
                format!("object response ended after {downloaded} of {total} bytes"),
            ));
        }
        Ok(downloaded)
    })();
    client.record_network_result(&result);

    match result {
        Ok(downloaded) => {
            progress.report(DownloadEvent::Finished { downloaded });
            Ok(())
        }
        Err(error) => {
            progress.report(DownloadEvent::Failed);
            Err(error)
        }
    }
}

fn prepare_single(
    client: &Client,
    resolved: &ResolvedTool,
    object: &mut File,
    payload_size: u64,
) -> Result<PreparedTool, StoreError> {
    let reserve = resolved
        .artifact
        .unpacked_size
        .checked_add(client.config.free_space_reserve_bytes.max(MINIMUM_RESERVE))
        .ok_or_else(|| StoreError::configuration(StoreStage::Storage, "required space overflow"))?;
    let relative = unpacked_relative(resolved.artifact.unpacked_sha256);
    let mut corruption = None;
    let mut storage_errors = Vec::new();
    for root in client.storage_roots() {
        if let Err(error) = ensure_writable_root(&root, reserve) {
            storage_errors.push(error);
            continue;
        }
        if axe_paths::path_is_noexec(&root) {
            storage_errors.push(StoreError::transient(
                StoreStage::Storage,
                format!("{} is mounted noexec", root.display()),
            ));
            continue;
        }
        let cache = root.join(&relative);
        let destination = cache.join("payload");
        let parent = cache.parent().expect("unpacked path has parent");
        if let Err(error) = fs::create_dir_all(parent) {
            storage_errors.push(storage_error(parent, error));
            continue;
        }
        let lock_path = root.join("locks").join(format!(
            "unpacked-{}.lock",
            resolved.artifact.unpacked_sha256
        ));
        let _guard = match acquire_lock(&lock_path) {
            Ok(guard) => guard,
            Err(error) => {
                storage_errors.push(error);
                continue;
            }
        };

        if cache.exists() {
            match ensure_private_cache_directory(&cache) {
                Ok(()) => {
                    if read_matching_state(&cache, &destination, &client.metadata_namespace)
                        .is_some()
                        && is_executable(&destination)
                    {
                        return Ok(PreparedTool {
                            executable: Executable::Path(destination),
                        });
                    }
                    match verify_unpacked_file(
                        &destination,
                        resolved.artifact.unpacked_sha256,
                        resolved.artifact.unpacked_size,
                    ) {
                        Ok(true) => {
                            write_verified_state(
                                &cache,
                                &destination,
                                None,
                                &client.metadata_namespace,
                            )?;
                            return Ok(PreparedTool {
                                executable: Executable::Path(destination),
                            });
                        }
                        Ok(false) => {}
                        Err(error) if error.class == FailureClass::Integrity => {
                            corruption.get_or_insert(error);
                        }
                        Err(error) => {
                            storage_errors.push(error);
                            continue;
                        }
                    }
                }
                Err(error) if error.class == FailureClass::Integrity => {
                    corruption.get_or_insert(error);
                }
                Err(error) => {
                    storage_errors.push(error);
                    continue;
                }
            }
            if let Err(error) = remove_cache_entry(&cache) {
                storage_errors.push(storage_error(&cache, error));
                continue;
            }
        }

        let temporary = parent.join(format!(".{}.prepare", resolved.artifact.unpacked_sha256));
        if let Err(error) = remove_cache_entry(&temporary) {
            storage_errors.push(storage_error(&temporary, error));
            continue;
        }
        if let Err(error) = ensure_private_cache_directory(&temporary) {
            storage_errors.push(error);
            continue;
        }
        let mut cleanup = CleanupPath::new(temporary.clone());
        let temporary_payload = temporary.join("payload");
        let mut output = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_payload)
        {
            Ok(file) => file,
            Err(error) => {
                storage_errors.push(storage_error(&temporary_payload, error));
                continue;
            }
        };
        let result: Result<PreparedTool, StoreError> = (|| {
            decode_single(object, payload_size, &mut output, resolved)?;
            make_executable(&temporary_payload)
                .map_err(|error| storage_error(&temporary_payload, error))?;
            output
                .sync_all()
                .map_err(|error| storage_error(&temporary_payload, error))?;
            write_verified_state(
                &temporary,
                &temporary_payload,
                None,
                &client.metadata_namespace,
            )?;
            sync_directory(&temporary)?;
            fs::rename(&temporary, &cache).map_err(|error| storage_error(&cache, error))?;
            sync_directory(parent)?;
            cleanup.disarm();
            Ok(PreparedTool {
                executable: Executable::Path(destination),
            })
        })();
        match result {
            Ok(prepared) => return Ok(prepared),
            Err(error) if error.class == FailureClass::Integrity => return Err(error),
            Err(error) => storage_errors.push(error),
        }
    }

    drop(storage_errors);
    #[cfg(target_os = "linux")]
    {
        let mut output = create_memfd(&format!("axe-{}", resolved.name), true)
            .map_err(|error| StoreError::transient(StoreStage::Storage, error.to_string()))?;
        if let Err(error) = decode_single(object, payload_size, &mut output, resolved) {
            return Err(corruption.unwrap_or(error));
        }
        seal_memfd(&output)
            .map_err(|error| StoreError::transient(StoreStage::Storage, error.to_string()))?;
        Ok(PreparedTool {
            executable: Executable::Memory(output),
        })
    }

    #[cfg(not(target_os = "linux"))]
    {
        let path =
            std::env::temp_dir().join(format!("axe-{}-{}", resolved.name, std::process::id()));
        let mut output = File::create(&path).map_err(|error| storage_error(&path, error))?;
        decode_single(object, payload_size, &mut output, resolved)?;
        make_executable(&path).map_err(|error| storage_error(&path, error))?;
        Ok(PreparedTool {
            executable: Executable::Temporary(path),
        })
    }
}

fn decode_single(
    object: &mut File,
    payload_size: u64,
    output: &mut File,
    resolved: &ResolvedTool,
) -> Result<(), StoreError> {
    object
        .seek(SeekFrom::Start(0))
        .map_err(|error| StoreError::transient(StoreStage::Storage, error.to_string()))?;
    output
        .seek(SeekFrom::Start(0))
        .map_err(|error| StoreError::transient(StoreStage::Storage, error.to_string()))?;
    let limited = object.take(payload_size);
    let mut decoder = zstd::Decoder::new(limited)
        .map_err(|error| classify_decode_io(error, "open Zstandard payload"))?;
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = decoder
            .read(&mut buffer)
            .map_err(|error| classify_decode_io(error, "decode executable"))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| StoreError::integrity(StoreStage::Decode, "decoded size overflow"))?;
        if size > resolved.artifact.unpacked_size {
            return Err(StoreError::integrity(
                StoreStage::Decode,
                "decoded executable exceeds signed size",
            ));
        }
        output
            .write_all(&buffer[..read])
            .map_err(|error| StoreError::transient(StoreStage::Storage, error.to_string()))?;
        hash.update(&buffer[..read]);
    }
    let digest = Digest(hash.finalize().into());
    if size != resolved.artifact.unpacked_size || digest != resolved.artifact.unpacked_sha256 {
        return Err(StoreError::integrity(
            StoreStage::Decode,
            format!("decoded executable digest or size mismatch: {digest}, {size} bytes"),
        ));
    }
    Ok(())
}

fn prepare_package(
    client: &Client,
    resolved: &ResolvedTool,
    entrypoint: &str,
    object: &mut File,
    payload_size: u64,
) -> Result<PreparedTool, StoreError> {
    validate_relative_path(entrypoint, "package entrypoint")
        .map_err(|error| StoreError::integrity(StoreStage::Decode, error.to_string()))?;
    let reserve = resolved
        .artifact
        .unpacked_size
        .checked_add(client.config.free_space_reserve_bytes.max(MINIMUM_RESERVE))
        .ok_or_else(|| StoreError::configuration(StoreStage::Storage, "required space overflow"))?;
    let relative = unpacked_relative(resolved.artifact.unpacked_sha256);
    let mut corruption = None;
    let mut last_storage = None;
    for root in client.storage_roots() {
        if let Err(error) = ensure_writable_root(&root, reserve) {
            last_storage = Some(error);
            continue;
        }
        if axe_paths::path_is_noexec(&root) {
            last_storage = Some(StoreError::transient(
                StoreStage::Storage,
                format!("{} is mounted noexec", root.display()),
            ));
            continue;
        }
        let cache = root.join(&relative);
        let destination = cache.join("tree");
        let executable = destination.join(entrypoint);
        let parent = cache.parent().expect("unpacked path has parent");
        if let Err(error) = fs::create_dir_all(parent) {
            last_storage = Some(storage_error(parent, error));
            continue;
        }
        let lock_path = root.join("locks").join(format!(
            "unpacked-{}.lock",
            resolved.artifact.unpacked_sha256
        ));
        let _guard = match acquire_lock(&lock_path) {
            Ok(guard) => guard,
            Err(error) => {
                last_storage = Some(error);
                continue;
            }
        };

        if cache.exists() {
            match ensure_private_cache_directory(&cache) {
                Ok(()) => {
                    if read_matching_state(&cache, &destination, &client.metadata_namespace)
                        .is_some()
                        && is_executable(&executable)
                    {
                        return Ok(PreparedTool {
                            executable: Executable::Path(executable),
                        });
                    }
                    match canonical_tree_digest(&destination) {
                        Ok(digest)
                            if digest == resolved.artifact.unpacked_sha256
                                && is_executable(&executable) =>
                        {
                            write_verified_state(
                                &cache,
                                &destination,
                                None,
                                &client.metadata_namespace,
                            )?;
                            return Ok(PreparedTool {
                                executable: Executable::Path(executable),
                            });
                        }
                        Ok(_) | Err(_) => {
                            corruption.get_or_insert_with(|| {
                                StoreError::integrity(
                                    StoreStage::Decode,
                                    "cached package tree digest mismatch or invalid entrypoint",
                                )
                            });
                        }
                    }
                }
                Err(error) if error.class == FailureClass::Integrity => {
                    corruption.get_or_insert(error);
                }
                Err(error) => {
                    last_storage = Some(error);
                    continue;
                }
            }
            if let Err(error) = remove_cache_entry(&cache) {
                last_storage = Some(storage_error(&cache, error));
                continue;
            }
        }

        let temporary = parent.join(format!(".{}.prepare", resolved.artifact.unpacked_sha256));
        if let Err(error) = remove_cache_entry(&temporary) {
            last_storage = Some(storage_error(&temporary, error));
            continue;
        }
        if let Err(error) = ensure_private_cache_directory(&temporary) {
            last_storage = Some(error);
            continue;
        }
        let mut cleanup = CleanupPath::new(temporary.clone());
        let tar_path = temporary.join("package.tar");
        let tree = temporary.join("tree");
        let result: Result<PreparedTool, StoreError> = (|| {
            decode_and_extract_package(
                object,
                payload_size,
                &tar_path,
                &tree,
                resolved,
                entrypoint,
            )?;
            fs::remove_file(&tar_path).map_err(|error| storage_error(&tar_path, error))?;
            write_verified_state(&temporary, &tree, None, &client.metadata_namespace)?;
            sync_directory(&temporary)?;
            fs::rename(&temporary, &cache).map_err(|error| storage_error(&cache, error))?;
            sync_directory(parent)?;
            cleanup.disarm();
            Ok(PreparedTool {
                executable: Executable::Path(executable),
            })
        })();
        match result {
            Ok(prepared) => return Ok(prepared),
            Err(error) if error.class == FailureClass::Integrity => return Err(error),
            Err(error) => last_storage = Some(error),
        }
    }
    Err(corruption.or(last_storage).unwrap_or_else(|| {
        StoreError::transient(StoreStage::Storage, "no package filesystem backend")
    }))
}

fn decode_and_extract_package(
    object: &mut File,
    payload_size: u64,
    tar_path: &Path,
    tree: &Path,
    resolved: &ResolvedTool,
    entrypoint: &str,
) -> Result<(), StoreError> {
    object
        .seek(SeekFrom::Start(0))
        .map_err(|error| StoreError::transient(StoreStage::Storage, error.to_string()))?;
    let limited = object.take(payload_size);
    let mut decoder = zstd::Decoder::new(limited)
        .map_err(|error| classify_decode_io(error, "open package Zstandard payload"))?;
    let mut tar_file = File::create(tar_path).map_err(|error| storage_error(tar_path, error))?;
    let size = io::copy(&mut decoder, &mut tar_file)
        .map_err(|error| classify_decode_io(error, "decode package tar"))?;
    if size != resolved.artifact.unpacked_size {
        return Err(StoreError::integrity(
            StoreStage::Decode,
            format!(
                "decoded package size {size} does not match signed {}",
                resolved.artifact.unpacked_size
            ),
        ));
    }
    tar_file
        .sync_all()
        .map_err(|error| storage_error(tar_path, error))?;
    fs::create_dir(tree).map_err(|error| storage_error(tree, error))?;
    extract_safe_tar(tar_path, tree)?;
    let digest = canonical_tree_digest(tree)
        .map_err(|error| StoreError::integrity(StoreStage::Decode, error.to_string()))?;
    if digest != resolved.artifact.unpacked_sha256 {
        return Err(StoreError::integrity(
            StoreStage::Decode,
            "package tree digest mismatch",
        ));
    }
    let executable = tree.join(entrypoint);
    if !is_executable(&executable) {
        return Err(StoreError::integrity(
            StoreStage::Decode,
            "package entrypoint is missing or not executable",
        ));
    }
    sync_directory_tree(tree).map_err(|error| storage_error(tree, error))
}

fn extract_safe_tar(tar_path: &Path, destination: &Path) -> Result<(), StoreError> {
    let file = File::open(tar_path).map_err(|error| storage_error(tar_path, error))?;
    let mut archive = tar::Archive::new(file);

    archive
        .unpack(destination)
        .map_err(|error| classify_decode_io(error, "unpack package"))
}
#[derive(Debug, Deserialize, Serialize)]
struct VerifiedState {
    schema: u8,
    namespace: String,
    fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_size: Option<u64>,
}

fn read_matching_state(cache: &Path, payload: &Path, namespace: &str) -> Option<VerifiedState> {
    let bytes = fs::read(cache.join(VERIFIED_STATE_FILE)).ok()?;
    let state: VerifiedState = serde_json::from_slice(&bytes).ok()?;
    if state.schema != VERIFIED_STATE_SCHEMA || state.namespace != namespace {
        return None;
    }
    let fingerprint = metadata_fingerprint(payload).ok()?;
    (state.fingerprint == fingerprint).then_some(state)
}

fn write_verified_state(
    cache: &Path,
    payload: &Path,
    payload_size: Option<u64>,
    namespace: &str,
) -> Result<(), StoreError> {
    let state = VerifiedState {
        schema: VERIFIED_STATE_SCHEMA,
        namespace: namespace.to_owned(),
        fingerprint: metadata_fingerprint(payload)
            .map_err(|error| storage_error(payload, error))?,
        payload_size,
    };
    let bytes = serde_json::to_vec(&state).map_err(|error| {
        StoreError::transient(
            StoreStage::Storage,
            format!("serialize verified cache state: {error}"),
        )
    })?;
    let temporary = cache.join(".verified.tmp");
    let destination = cache.join(VERIFIED_STATE_FILE);
    remove_cache_entry(&temporary).map_err(|error| storage_error(&temporary, error))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| storage_error(&temporary, error))?;
    file.write_all(&bytes)
        .map_err(|error| storage_error(&temporary, error))?;
    file.sync_all()
        .map_err(|error| storage_error(&temporary, error))?;
    remove_cache_entry(&destination).map_err(|error| storage_error(&destination, error))?;
    fs::rename(&temporary, &destination).map_err(|error| storage_error(&destination, error))?;
    sync_directory(cache)
}

fn metadata_fingerprint(path: &Path) -> io::Result<String> {
    let mut hash = Sha256::new();
    fingerprint_entry(path, Path::new(""), &mut hash)?;
    Ok(Digest(hash.finalize().into()).to_string())
}

fn fingerprint_entry(path: &Path, relative: &Path, hash: &mut Sha256) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let relative_bytes = relative.as_os_str().as_encoded_bytes();
    hash.update(relative_bytes.len().to_le_bytes());
    hash.update(relative_bytes);
    let file_type = metadata.file_type();
    hash.update([if file_type.is_file() {
        b'f'
    } else if file_type.is_dir() {
        b'd'
    } else if file_type.is_symlink() {
        b'l'
    } else {
        b'?'
    }]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.mode().into(),
            metadata.nlink(),
            metadata.uid().into(),
            metadata.gid().into(),
            metadata.size(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ] {
            hash.update(value.to_le_bytes());
        }
    }
    #[cfg(not(unix))]
    {
        hash.update(metadata.len().to_le_bytes());
        hash.update([u8::from(metadata.permissions().readonly())]);
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(SystemTime::UNIX_EPOCH)
        {
            hash.update(duration.as_nanos().to_le_bytes());
        }
    }
    if file_type.is_symlink() {
        let target = fs::read_link(path)?;
        let target = target.as_os_str().as_encoded_bytes();
        hash.update(target.len().to_le_bytes());
        hash.update(target);
    } else if file_type.is_dir() {
        let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by(|left, right| {
            left.file_name()
                .as_encoded_bytes()
                .cmp(right.file_name().as_encoded_bytes())
        });
        for child in children {
            fingerprint_entry(&child.path(), &relative.join(child.file_name()), hash)?;
        }
    }
    Ok(())
}

fn ensure_private_cache_directory(path: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() {
                return Err(StoreError::integrity(
                    StoreStage::Storage,
                    format!("cache entry {} is not a directory", path.display()),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};

                // SAFETY: geteuid has no preconditions.
                if metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(StoreError::integrity(
                        StoreStage::Storage,
                        format!("cache directory {} has the wrong owner", path.display()),
                    ));
                }
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .map_err(|error| storage_error(path, error))?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(path)
                .map_err(|error| storage_error(path, error))?;
        }
        Err(error) => return Err(storage_error(path, error)),
    }
    Ok(())
}

pub(super) fn remove_cache_entry(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            prepare_directory_removal(path)?;
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn prepare_directory_removal(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Ok(());
    }
    let mode = metadata.permissions().mode();
    if mode & 0o700 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700))?;
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            prepare_directory_removal(&entry.path())?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_directory_removal(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| storage_error(path, error))
}

struct CleanupPath {
    path: Option<PathBuf>,
}

impl CleanupPath {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for CleanupPath {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = remove_cache_entry(path);
        }
    }
}

fn verify_unpacked_file(path: &Path, expected: Digest, size: u64) -> Result<bool, StoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(storage_error(path, error)),
    };
    if !metadata.is_file() {
        return Err(StoreError::integrity(
            StoreStage::Decode,
            "cached executable is not a file",
        ));
    }
    if metadata.len() != size {
        return Err(StoreError::integrity(
            StoreStage::Decode,
            "cached executable size mismatch",
        ));
    }
    let actual = Digest::from_file(path).map_err(|error| storage_error(path, error))?;
    if actual != expected {
        return Err(StoreError::integrity(
            StoreStage::Decode,
            "cached executable digest mismatch",
        ));
    }
    make_executable(path).map_err(|error| storage_error(path, error))?;
    Ok(true)
}

fn object_relative(digest: Digest) -> PathBuf {
    PathBuf::from(digest.object_path())
}

fn unpacked_relative(digest: Digest) -> PathBuf {
    let digest = digest.to_string();
    PathBuf::from("unpacked/sha256")
        .join(&digest[..2])
        .join(digest)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    path.is_file()
}

fn classify_decode_io(error: io::Error, context: &str) -> StoreError {
    let kind = error.kind();
    let source = format!("{context}: {}", format_error_chain(&error));
    match kind {
        io::ErrorKind::StorageFull
        | io::ErrorKind::QuotaExceeded
        | io::ErrorKind::PermissionDenied
        | io::ErrorKind::ReadOnlyFilesystem => StoreError::transient(StoreStage::Storage, source),
        _ => StoreError::integrity(StoreStage::Decode, source),
    }
}

fn format_error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut previous = message.clone();
    let mut source = error.source();
    while let Some(error) = source {
        let current = error.to_string();
        if current != previous {
            message.push_str(": ");
            message.push_str(&current);
        }
        previous = current;
        source = error.source();
    }
    message
}

fn acquire_lock(path: &Path) -> Result<LockGuard, StoreError> {
    let parent = path.parent().expect("lock path has parent");
    fs::create_dir_all(parent).map_err(|error| storage_error(parent, error))?;
    for _ in 0..100 {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                return Ok(LockGuard(path.to_owned()));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if lock_stale(path) {
                    let _ = fs::remove_file(path);
                } else {
                    thread::sleep(Duration::from_millis(100));
                }
            }
            Err(error) => return Err(storage_error(path, error)),
        }
    }
    Err(StoreError::transient(
        StoreStage::Storage,
        "timed out waiting for cache lock",
    ))
}

fn lock_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .and_then(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .map_err(io::Error::other)
        })
        .is_ok_and(|age| age > Duration::from_secs(120))
}

struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(target_os = "linux")]
fn create_memfd(name: &str, sealing: bool) -> io::Result<File> {
    use std::os::fd::FromRawFd;

    let name = CString::new(name).map_err(io::Error::other)?;
    let flags = if sealing {
        libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC
    } else {
        0
    };
    // SAFETY: name is NUL-terminated and memfd_create returns a new owned descriptor.
    let descriptor = unsafe { libc::memfd_create(name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: descriptor is newly owned and transferred exactly once to File.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(target_os = "linux")]
fn seal_memfd(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    // SAFETY: file owns a valid memfd and F_ADD_SEALS does not access Rust memory.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn run_path(
    path: &Path,
    name: &str,
    args: &[OsString],
    ca: Option<&CaBundle>,
) -> io::Result<ExitStatus> {
    let mut command = Command::new(path);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.arg0(name);
    }
    #[cfg(not(unix))]
    let _ = name;
    command.args(args);
    if let Some(ca) = ca {
        command.env("CURL_CA_BUNDLE", ca.path());
    }
    command.status()
}

#[cfg(target_os = "linux")]
fn run_memfd(
    file: &File,
    name: &str,
    args: &[OsString],
    ca: Option<&CaBundle>,
) -> io::Result<ExitStatus> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(CString::new(name.as_bytes()).map_err(io::Error::other)?);
    for argument in args {
        argv.push(CString::new(argument.as_os_str().as_bytes()).map_err(io::Error::other)?);
    }
    let mut environment = std::env::vars_os()
        .filter(|(key, _)| key != OsStr::new("CURL_CA_BUNDLE"))
        .map(|(key, value)| {
            let mut bytes = key.as_os_str().as_bytes().to_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_os_str().as_bytes());
            CString::new(bytes).map_err(io::Error::other)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(ca) = ca {
        let mut bytes = b"CURL_CA_BUNDLE=".to_vec();
        bytes.extend_from_slice(ca.path().as_os_str().as_bytes());
        environment.push(CString::new(bytes).map_err(io::Error::other)?);
    }
    let argv_pointers = argv
        .iter()
        .map(|value| value.as_ptr() as usize)
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let environment_pointers = environment
        .iter()
        .map(|value| value.as_ptr() as usize)
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let descriptor = file.as_raw_fd();
    let empty = CString::new("").expect("empty CString");

    let mut command = Command::new("/proc/self/exe");
    // SAFETY: all CStrings and pointer arrays are owned by the closure, remain valid through
    // execveat, are NUL-terminated, and both arrays end with a null pointer. The descriptor is
    // a sealed executable memfd retained by PreparedTool until the child has exited.
    unsafe {
        command.pre_exec(move || {
            let _keep_alive = (&argv, &environment);
            let result = libc::syscall(
                libc::SYS_execveat,
                descriptor,
                empty.as_ptr(),
                argv_pointers.as_ptr().cast::<libc::c_char>(),
                environment_pointers.as_ptr().cast::<libc::c_char>(),
                libc::AT_EMPTY_PATH,
            );
            debug_assert_eq!(result, -1);
            Err(io::Error::last_os_error())
        });
    }
    command.status()
}

fn sync_directory_tree(root: &Path) -> io::Result<()> {
    let mut directories = vec![root.to_owned()];
    let mut index = 0;
    while index < directories.len() {
        for entry in fs::read_dir(&directories[index])? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                directories.push(entry.path());
            } else if entry.file_type()?.is_file() {
                File::open(entry.path())?.sync_all()?;
            }
        }
        index += 1;
    }
    for directory in directories.into_iter().rev() {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

enum CaBundle {
    #[cfg(target_os = "linux")]
    Memory(File),
    #[cfg(not(target_os = "linux"))]
    Temporary(PathBuf),
}

impl CaBundle {
    fn create() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let mut file = create_memfd("axe-curl-ca", false)?;
            file.write_all(axe_tls_roots::pem_bundle())?;
            file.flush()?;
            Ok(Self::Memory(file))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let path = std::env::temp_dir().join(format!("axe-curl-ca-{}", std::process::id()));
            fs::write(&path, axe_tls_roots::pem_bundle())?;
            Ok(Self::Temporary(path))
        }
    }

    fn path(&self) -> PathBuf {
        match self {
            #[cfg(target_os = "linux")]
            Self::Memory(file) => {
                use std::os::fd::AsRawFd;
                PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
            }
            #[cfg(not(target_os = "linux"))]
            Self::Temporary(path) => path.clone(),
        }
    }
}

impl Drop for CaBundle {
    fn drop(&mut self) {
        #[cfg(not(target_os = "linux"))]
        {
            let Self::Temporary(path) = self;
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_for_signer(signing: &ed25519_dalek::SigningKey) -> Client {
        let mut trusted_keys = axe_artifact::TrustedKeys::new();
        trusted_keys.insert(signing.verifying_key());
        Client::new(
            crate::ClientConfig {
                public_base_url: "https://example.com/store/".into(),
                addresses: vec!["127.0.0.1".parse().expect("loopback address")],
                max_object_bytes: 1024 * 1024,
                max_metadata_bytes: 1024 * 1024,
                index_ttl_secs: 60,
                manifest_ttl_secs: 60,
                metadata_timeout: Duration::from_secs(1),
                object_timeout: Duration::from_secs(1),
                transient_retry: Duration::from_secs(30),
                free_space_reserve_bytes: 1,
                channel: "stable".into(),
                persistent_roots: Vec::new(),
                tmpfs_roots: Vec::new(),
                trusted_keys,
                embedded_index: Vec::new(),
            },
            crate::NetworkPolicy::Offline,
        )
        .expect("build client")
    }

    #[test]
    fn verified_state_is_invalidated_by_payload_metadata_change() {
        let root = std::env::temp_dir().join(format!(
            "axe-store-verified-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let cache = root.join("cache");
        ensure_private_cache_directory(&root).expect("create test root");
        ensure_private_cache_directory(&cache).expect("create private cache");
        let payload = cache.join("payload");
        fs::write(&payload, b"original").expect("write payload");
        make_executable(&payload).expect("make payload executable");
        write_verified_state(&cache, &payload, None, "edition-a").expect("write verified state");
        assert!(read_matching_state(&cache, &payload, "edition-a").is_some());

        fs::write(&payload, b"modified").expect("modify payload");
        assert!(read_matching_state(&cache, &payload, "edition-a").is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&cache)
                    .expect("cache metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(root).expect("remove test cache");
    }

    #[test]
    fn shared_object_verified_by_another_trust_set_requires_new_signature_check() {
        use axe_artifact::{Artifact, ArtifactKind, sign_artifact};
        use ed25519_dalek::SigningKey;

        let root = std::env::temp_dir().join(format!(
            "axe-store-shared-object-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let cache = root.join("objects/sha256/shared");
        fs::create_dir_all(cache.parent().expect("cache has parent"))
            .expect("create shared cache parent");
        ensure_private_cache_directory(&cache).expect("create shared cache");
        let signing = SigningKey::from_bytes(&[17; 32]);
        let other_signing = SigningKey::from_bytes(&[23; 32]);
        let mut signed = io::Cursor::new(Vec::new());
        let written = sign_artifact(&mut b"compressed payload".as_slice(), &mut signed, &signing)
            .expect("sign object fixture");
        let path = cache.join("object");
        fs::write(&path, signed.into_inner()).expect("cache signed object");

        let first = client_for_signer(&signing);
        let second = client_for_signer(&other_signing);
        let resolved = crate::ResolvedTool {
            name: "shared".into(),
            version: "1".into(),
            target: axe_artifact::Target::detect().expect("supported target"),
            artifact: Artifact {
                kind: ArtifactKind::SingleBinary,
                object_sha256: written.object_sha256,
                compressed_size: written.compressed_size,
                unpacked_size: 1,
                unpacked_sha256: Digest::of(b"payload"),
                fully_static: false,
            },
        };
        load_cached_object(&first, &resolved, &cache)
            .expect("first edition verifies object")
            .expect("cached object");
        assert_ne!(first.metadata_namespace, second.metadata_namespace);
        let error = match load_cached_object(&second, &resolved, &cache) {
            Ok(_) => panic!("second edition cannot reuse first edition's verified state"),
            Err(error) => error,
        };
        assert_eq!(
            (error.stage, error.class),
            (StoreStage::Object, FailureClass::Integrity)
        );
        assert!(
            path.exists(),
            "shared object remains available to the first edition"
        );
        fs::remove_dir_all(root).expect("remove shared cache fixture");
    }

    #[test]
    fn shared_unpacked_state_from_another_edition_is_rechecked_and_repaired() {
        use axe_artifact::{Artifact, ArtifactKind, sign_artifact};
        use ed25519_dalek::SigningKey;

        let root = std::env::temp_dir().join(format!(
            "axe-store-shared-unpacked-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let correct = b"#!/bin/sh\nexit 7\n";
        let digest = Digest::of(correct);
        let cache = root.join(unpacked_relative(digest));
        fs::create_dir_all(cache.parent().expect("unpacked cache parent"))
            .expect("create unpacked cache parent");
        ensure_private_cache_directory(&cache).expect("create shared unpacked cache");
        let destination = cache.join("payload");
        fs::write(&destination, vec![b'x'; correct.len()]).expect("write stale payload");
        make_executable(&destination).expect("make stale payload executable");

        let first_signing = SigningKey::from_bytes(&[17; 32]);
        let second_signing = SigningKey::from_bytes(&[23; 32]);
        let first = client_for_signer(&first_signing);
        let mut second = client_for_signer(&second_signing);
        second.storage_roots_override = Some(vec![root.clone()]);
        write_verified_state(&cache, &destination, None, &first.metadata_namespace)
            .expect("write other edition's verified state");

        let compressed = zstd::stream::encode_all(correct.as_slice(), 0).expect("compress payload");
        let mut signed = io::Cursor::new(Vec::new());
        let written = sign_artifact(&mut compressed.as_slice(), &mut signed, &second_signing)
            .expect("sign replacement object");
        let object_path = root.join("signed-object");
        fs::write(&object_path, signed.into_inner()).expect("write replacement object");
        let mut object = File::open(object_path).expect("open replacement object");
        let resolved = crate::ResolvedTool {
            name: "shared".into(),
            version: "1".into(),
            target: axe_artifact::Target::detect().expect("supported target"),
            artifact: Artifact {
                kind: ArtifactKind::SingleBinary,
                object_sha256: written.object_sha256,
                compressed_size: written.compressed_size,
                unpacked_size: correct.len() as u64,
                unpacked_sha256: digest,
                fully_static: false,
            },
        };
        let prepared = prepare_single(&second, &resolved, &mut object, written.payload_size)
            .expect("repair corrupt shared payload");
        assert!(matches!(&prepared.executable, Executable::Path(_)));
        assert_eq!(
            fs::read(&destination)
                .expect("read repaired payload")
                .as_slice(),
            correct.as_slice()
        );
        assert!(read_matching_state(&cache, &destination, &second.metadata_namespace).is_some());
        fs::remove_dir_all(root).expect("remove shared unpacked fixture");
    }

    #[cfg(unix)]
    #[test]
    fn extracts_and_removes_package_with_read_only_directories() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "axe-store-read-only-package-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("create package fixture root");
        let tar_path = root.join("package.tar");
        let file = File::create(&tar_path).expect("create package tar");
        let mut builder = tar::Builder::new(file);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o555);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "bin", io::empty())
            .expect("append read-only directory");

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_link_name("python3").expect("set symlink target");
        header.set_cksum();
        builder
            .append_data(&mut header, "bin/python", io::empty())
            .expect("append symlink");

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o555);
        header.set_size(6);
        header.set_cksum();
        builder
            .append_data(&mut header, "bin/python3", &b"python"[..])
            .expect("append executable");
        builder.finish().expect("finish package tar");

        let tree = root.join("tree");
        fs::create_dir(&tree).expect("create package tree");
        extract_safe_tar(&tar_path, &tree).expect("extract package");

        assert_eq!(
            fs::read_link(tree.join("bin/python")).expect("read extracted symlink"),
            Path::new("python3")
        );
        assert_eq!(
            fs::metadata(tree.join("bin"))
                .expect("read extracted directory")
                .permissions()
                .mode()
                & 0o777,
            0o555
        );

        remove_cache_entry(&tree).expect("remove read-only package tree");
        assert!(!tree.exists());
        fs::remove_dir_all(root).expect("remove package fixture root");
    }

    #[test]
    fn decode_storage_error_preserves_source_chain() {
        #[derive(Debug)]
        struct ExtractionError(io::Error);

        impl std::fmt::Display for ExtractionError {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("failed to unpack fixture")
            }
        }

        impl std::error::Error for ExtractionError {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let error = io::Error::new(
            io::ErrorKind::PermissionDenied,
            ExtractionError(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "filesystem denied write",
            )),
        );
        let classified = classify_decode_io(error, "unpack package");

        assert_eq!(classified.class, FailureClass::Transient);
        assert_eq!(classified.stage, StoreStage::Storage);
        assert_eq!(
            classified.source,
            "unpack package: failed to unpack fixture: filesystem denied write"
        );
    }
}
