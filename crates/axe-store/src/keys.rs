use std::fs;
use std::io::{self, Write};
use std::path::Path;

use axe_artifact::{KeyId, TrustedKeys, decode_hex_32};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::RngExt;
use rcgen::{
    CertificateParams, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose, PKCS_ED25519,
    date_time_ymd,
};

pub fn generate(root: &Path) -> Result<(), String> {
    let signing_path = root.join("signing.key");
    let trusted_dir = root.join("trusted");
    if trusted_dir
        .read_dir()
        .ok()
        .is_some_and(|mut entries| entries.next().is_some())
    {
        return Err(format!(
            "refusing to overwrite keys in {}",
            trusted_dir.display()
        ));
    }

    fs::create_dir_all(&trusted_dir)
        .map_err(|error| format!("create {}: {error}", trusted_dir.display()))?;

    let seed = rand::rng().random::<[u8; 32]>();
    let signing = SigningKey::from_bytes(&seed);
    let key_id = KeyId::for_key(&signing.verifying_key());
    let public_path = trusted_dir.join(format!("{key_id}.pub"));

    write_private_atomic(&signing_path, format_hex(&seed).as_bytes())?;
    if let Err(error) = write_atomic(
        &public_path,
        format_hex(signing.verifying_key().as_bytes()).as_bytes(),
    ) {
        let _ = fs::remove_file(&signing_path);
        return Err(error);
    }
    Ok(())
}

pub fn generate_relay_token(path: &Path) -> Result<(), String> {
    let token = rand::rng().random::<[u8; 32]>();
    write_private_atomic(path, format_hex(&token).as_bytes())
}

pub fn generate_relay_identities(root: &Path) -> Result<(), String> {
    let server_certificate_path = root.join("quic_server_cert.pem");
    let server_private_key_path = root.join("quic_server_key.pem");
    let client_certificate_path = root.join("quic_client_cert.pem");
    let client_private_key_path = root.join("quic_client_key.pem");
    let paths = [
        &server_certificate_path,
        &server_private_key_path,
        &client_certificate_path,
        &client_private_key_path,
    ];
    if paths.iter().any(|path| path.exists()) {
        return Err(format!(
            "refusing to overwrite relay QUIC identities in {}",
            root.display()
        ));
    }

    fs::create_dir_all(root).map_err(|error| format!("create {}: {error}", root.display()))?;
    let (server_certificate, server_private_key) =
        generate_quic_identity("axe-relay", ExtendedKeyUsagePurpose::ServerAuth)?;
    let (client_certificate, client_private_key) =
        generate_quic_identity("axe-sshd", ExtendedKeyUsagePurpose::ClientAuth)?;
    let artifacts = [
        (server_private_key_path, server_private_key, true),
        (server_certificate_path, server_certificate, false),
        (client_private_key_path, client_private_key, true),
        (client_certificate_path, client_certificate, false),
    ];

    let mut written = Vec::with_capacity(artifacts.len());
    for (path, contents, private) in artifacts {
        let result = if private {
            write_private_atomic(&path, contents.as_bytes())
        } else {
            write_atomic(&path, contents.as_bytes())
        };
        if let Err(error) = result {
            for written_path in written {
                let _ = fs::remove_file(written_path);
            }
            return Err(error);
        }
        written.push(path);
    }
    Ok(())
}

fn generate_quic_identity(
    subject: &str,
    usage: ExtendedKeyUsagePurpose,
) -> Result<(String, String), String> {
    let mut parameters = CertificateParams::new(vec![subject.to_owned()])
        .map_err(|error| format!("create {subject} QUIC certificate parameters: {error}"))?;
    parameters.not_before = date_time_ymd(2020, 1, 1);
    parameters.not_after = date_time_ymd(2120, 1, 1);
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![usage];

    let private_key = KeyPair::generate_for(&PKCS_ED25519)
        .map_err(|error| format!("generate {subject} QUIC private key: {error}"))?;
    let certificate = parameters
        .self_signed(&private_key)
        .map_err(|error| format!("sign {subject} QUIC certificate: {error}"))?;

    Ok((certificate.pem(), private_key.serialize_pem()))
}

pub fn load_signing_key(workspace: &Path) -> Result<SigningKey, String> {
    let encoded = match std::env::var("AXE_STORE_SIGNING_KEY") {
        Ok(value) if !value.is_empty() => value,
        _ => fs::read_to_string(workspace.join("keys/store/signing.key"))
            .map_err(|error| format!("read keys/store/signing.key: {error}"))?,
    };
    let bytes =
        decode_hex_32(encoded.trim(), "store signing key").map_err(|error| error.to_string())?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn load_trusted_keys(directory: &Path) -> Result<TrustedKeys, String> {
    let mut paths = fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {}: {error}", directory.display()))?;
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "pub"));
    paths.sort_unstable();

    if paths.is_empty() {
        return Err(format!(
            "{} has no trusted public keys",
            directory.display()
        ));
    }

    let mut keys = TrustedKeys::new();
    for path in paths {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| format!("invalid trusted key filename {}", path.display()))?;
        let id = stem.parse::<KeyId>().map_err(|error| error.to_string())?;
        let encoded = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let bytes = decode_hex_32(encoded.trim(), "trusted public key")
            .map_err(|error| error.to_string())?;
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        keys.insert_named(id, key)
            .map_err(|error| error.to_string())?;
    }

    Ok(keys)
}

pub fn format_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic_with_mode(path, bytes, Some(0o600))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic_with_mode(path, bytes, None)
}

fn write_atomic_with_mode(path: &Path, bytes: &[u8], unix_mode: Option<u32>) -> Result<(), String> {
    crate::fs_atomic::write(
        path,
        crate::fs_atomic::InstallMode::NoReplace,
        unix_mode,
        |file| {
            file.write_all(bytes)?;
            file.write_all(b"\n")
        },
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            format!("refusing to overwrite {}", path.display())
        } else {
            format!("write {}: {error}", path.display())
        }
    })
}
