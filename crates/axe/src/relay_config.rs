use std::sync::LazyLock;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    enabled_by_default: bool,
    tcp_endpoint: Option<String>,
    quic_endpoint: Option<String>,
}

static CONFIG: LazyLock<Result<RelayConfig, String>> = LazyLock::new(|| {
    let config: RelayConfig = serde_json::from_str(crate::embedded::RELAY_CONFIG_JSON)
        .map_err(|error| format!("invalid config/relay.json: {error}"))?;
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
    Ok(config)
});

pub fn load() -> Result<&'static RelayConfig, String> {
    CONFIG.as_ref().map_err(Clone::clone)
}

pub fn enabled_by_default() -> bool {
    load()
        .expect("relay config was validated during registry construction")
        .enabled_by_default
}

pub fn tcp_endpoint() -> Option<&'static str> {
    load()
        .expect("relay config was validated during registry construction")
        .tcp_endpoint
        .as_deref()
}

pub fn quic_endpoint() -> Option<&'static str> {
    load()
        .expect("relay config was validated during registry construction")
        .quic_endpoint
        .as_deref()
}

pub fn token() -> &'static str {
    crate::embedded::RELAY_TOKEN
}
