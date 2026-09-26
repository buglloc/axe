use std::collections::HashSet;
use std::sync::LazyLock;

use russh::keys::{HashAlg, ssh_key};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct SshdFile {
    principals: Vec<String>,
}

pub struct SshdConfig {
    principals: Vec<String>,
    ca_fingerprints: Vec<ssh_key::Fingerprint>,
}

static CONFIG: LazyLock<Result<SshdConfig, String>> = LazyLock::new(|| {
    let file: SshdFile = serde_json::from_str(crate::embedded::SSHD_CONFIG_JSON)
        .map_err(|error| format!("invalid config/sshd.json: {error}"))?;
    if file.principals.is_empty() {
        return Err("config/sshd.json principals must not be empty".into());
    }

    let mut unique = HashSet::with_capacity(file.principals.len());
    for principal in &file.principals {
        if principal.is_empty() {
            return Err("config/sshd.json principals must not contain empty names".into());
        }
        if !unique.insert(principal) {
            return Err(format!(
                "config/sshd.json contains duplicate principal '{principal}'"
            ));
        }
    }

    let ca_fingerprints = crate::embedded::SSH_USER_CA_KEYS
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, encoded)| {
            ssh_key::PublicKey::from_openssh(encoded)
                .map(|key| key.fingerprint(HashAlg::Sha256))
                .map_err(|error| format!("invalid keys/ssh/user_ca_keys line {index}: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SshdConfig {
        principals: file.principals,
        ca_fingerprints,
    })
});

pub fn load() -> Result<&'static SshdConfig, String> {
    CONFIG.as_ref().map_err(Clone::clone)
}

pub fn principals() -> &'static [String] {
    &load()
        .expect("sshd config was validated during registry construction")
        .principals
}

pub fn ca_fingerprints() -> &'static [ssh_key::Fingerprint] {
    &load()
        .expect("sshd config was validated during registry construction")
        .ca_fingerprints
}
