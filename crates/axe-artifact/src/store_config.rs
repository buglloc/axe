use std::net::IpAddr;
use std::path::PathBuf;

use serde::Deserialize;

use crate::{Error, invalid};

/// `config/store.json`, shared by the AXE build, the Store client, and the publisher.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    pub consumer: ConsumerConfig,
    pub storage: StorageConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerConfig {
    pub addresses: Vec<IpAddr>,
    pub max_object_bytes: u64,
    pub max_metadata_bytes: u64,
    pub metadata_timeout_secs: u64,
    pub object_timeout_secs: u64,
    pub transient_retry_secs: u64,
    pub free_space_reserve_bytes: u64,
    pub channel: String,
    pub persistent_roots: Vec<PathBuf>,
    pub tmpfs_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub release_prefix: String,
}

impl StoreConfig {
    pub fn from_json(json: &[u8]) -> Result<Self, Error> {
        let config: Self = serde_json::from_slice(json)
            .map_err(|error| Error::Invalid(format!("invalid Store configuration: {error}")))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), Error> {
        let consumer = &self.consumer;
        if consumer.addresses.is_empty() {
            return invalid("Store consumer.addresses must not be empty");
        }
        for (name, value) in [
            ("max_object_bytes", consumer.max_object_bytes),
            ("max_metadata_bytes", consumer.max_metadata_bytes),
            ("metadata_timeout_secs", consumer.metadata_timeout_secs),
            ("object_timeout_secs", consumer.object_timeout_secs),
            ("transient_retry_secs", consumer.transient_retry_secs),
            (
                "free_space_reserve_bytes",
                consumer.free_space_reserve_bytes,
            ),
        ] {
            if value == 0 {
                return invalid(format!("Store consumer.{name} must be non-zero"));
            }
        }
        if consumer.channel.is_empty() {
            return invalid("Store consumer.channel must not be empty");
        }

        let storage = &self.storage;
        if !storage
            .endpoint
            .strip_prefix("https://")
            .and_then(|rest| rest.split('/').next())
            .is_some_and(|host| !host.is_empty())
        {
            return invalid("Store storage.endpoint must be an HTTPS URL with a host");
        }
        for (name, value) in [
            ("region", &storage.region),
            ("bucket", &storage.bucket),
            ("prefix", &storage.prefix),
            ("release_prefix", &storage.release_prefix),
        ] {
            if value.is_empty() {
                return invalid(format!("Store storage.{name} must not be empty"));
            }
        }
        Ok(())
    }
}

impl StorageConfig {
    /// Anonymous consumer URL of the Store prefix, with a trailing slash.
    #[must_use]
    pub fn public_base_url(&self) -> String {
        format!(
            "{}/{}/{}/",
            self.endpoint.trim_end_matches('/'),
            self.bucket.trim_matches('/'),
            self.prefix.trim_matches('/')
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_config_builds_consumer_url() {
        let storage = StorageConfig {
            endpoint: "https://storage.yandexcloud.net/".into(),
            region: "ru-central1".into(),
            bucket: "test-bucket".into(),
            prefix: "/tools/".into(),
            release_prefix: "/axe/".into(),
        };

        assert_eq!(
            storage.public_base_url(),
            "https://storage.yandexcloud.net/test-bucket/tools/"
        );
    }
}
