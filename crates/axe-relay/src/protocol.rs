//! Wire contract shared by the relay server and its clients.
//!
//! A client opens one control channel: a raw TCP stream (yamux-multiplexed
//! after registration) or a mutually authenticated QUIC connection whose first
//! bidirectional stream carries registration. Both exchange one length-prefixed
//! CBOR [`RegistrationRequest`] and one [`RegistrationResponse`]. Every tunnel
//! the relay opens afterwards starts with a one-byte stream kind.

use std::borrow::Cow;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

pub const PROTOCOL_VERSION: u8 = 2;
pub const STREAM_KIND_TCP: u8 = 1;
/// Bounds the whole registration exchange on both sides of a control channel.
pub const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounds how long a relay waits for a client to accept one tunnel.
pub const TUNNEL_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
pub const QUIC_ALPN: &[u8] = b"axe-relay-quic/1";

const MAX_CONTROL_MESSAGE: usize = 4096;
const MAX_MUX_STREAMS: usize = 128;
const MAX_MUX_RECEIVE_WINDOW: usize = 32 * 1024 * 1024;
const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);
const QUIC_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const QUIC_STREAM_RECEIVE_WINDOW: u32 = 16 * 1024 * 1024;
const QUIC_CONNECTION_RECEIVE_WINDOW: u32 = 64 * 1024 * 1024;
const QUIC_SEND_WINDOW: u64 = 64 * 1024 * 1024;
const QUIC_MAX_CONCURRENT_STREAMS: u32 = 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum Transport {
    #[default]
    Tcp,
    Quic,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Quic => "quic",
        }
    }
}

impl std::fmt::Display for Transport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Registration sent by a client. TCP clients authenticate with `token`; QUIC
/// clients authenticate with their TLS identity and omit it.
#[derive(Deserialize, Serialize)]
pub struct RegistrationRequest<'a> {
    pub version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Cow<'a, str>>,
    pub client_id: Cow<'a, str>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RegistrationResponse<'a> {
    Accepted {
        version: u8,
        public_address: Cow<'a, str>,
    },
    Rejected {
        version: u8,
        message: Cow<'a, str>,
    },
}

impl<'a> RegistrationResponse<'a> {
    pub fn accepted(public_address: &'a str) -> Self {
        Self::Accepted {
            version: PROTOCOL_VERSION,
            public_address: Cow::Borrowed(public_address),
        }
    }

    pub fn rejected(message: &'a str) -> Self {
        Self::Rejected {
            version: PROTOCOL_VERSION,
            message: Cow::Borrowed(message),
        }
    }

    /// Returns the public address assigned by `relay`, or the rejection.
    pub fn into_public_address(self, relay: &str) -> io::Result<String> {
        let (version, result) = match self {
            Self::Accepted {
                version,
                public_address,
            } => (version, Ok(public_address.into_owned())),
            Self::Rejected { version, message } => (
                version,
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("registration at {relay} rejected: {message}"),
                )),
            ),
        };
        if version != PROTOCOL_VERSION {
            return Err(invalid(format!(
                "endpoint {relay} uses unsupported protocol version {version}"
            )));
        }
        result
    }
}

pub async fn write_cbor_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> io::Result<()> {
    let mut payload = Vec::with_capacity(256);
    ciborium::ser::into_writer(value, &mut payload)
        .map_err(|error| invalid(format!("encode control message: {error}")))?;
    if payload.len() > MAX_CONTROL_MESSAGE {
        return Err(invalid("control message too long"));
    }

    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(&payload).await
}

pub async fn read_cbor_frame<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> io::Result<T> {
    let length = reader.read_u32().await? as usize;
    if length > MAX_CONTROL_MESSAGE {
        return Err(invalid("control message too long"));
    }

    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    ciborium::de::from_reader(payload.as_slice())
        .map_err(|error| invalid(format!("decode control message: {error}")))
}

pub fn configure_control_socket(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    SockRef::from(stream).set_tcp_keepalive(
        &TcpKeepalive::new()
            .with_time(Duration::from_secs(15))
            .with_interval(Duration::from_secs(5)),
    )
}

pub fn mux_config() -> yamux::Config {
    let mut config = yamux::Config::default();
    config.set_max_num_streams(MAX_MUX_STREAMS);
    config.set_max_connection_receive_window(Some(MAX_MUX_RECEIVE_WINDOW));
    config
}

pub fn quic_transport_config() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config.keep_alive_interval(Some(QUIC_KEEP_ALIVE_INTERVAL));
    config.max_idle_timeout(Some(
        QUIC_IDLE_TIMEOUT
            .try_into()
            .expect("relay QUIC idle timeout is representable"),
    ));
    config.stream_receive_window(quinn::VarInt::from_u32(QUIC_STREAM_RECEIVE_WINDOW));
    config.receive_window(quinn::VarInt::from_u32(QUIC_CONNECTION_RECEIVE_WINDOW));
    config.send_window(QUIC_SEND_WINDOW);
    config.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    config.max_concurrent_bidi_streams(quinn::VarInt::from_u32(QUIC_MAX_CONCURRENT_STREAMS));
    Arc::new(config)
}

/// Parses a PEM document that must contain exactly one certificate.
pub fn pem_certificate(pem: &[u8], name: &str) -> io::Result<CertificateDer<'static>> {
    let certificates = rustls_pemfile::certs(&mut &*pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid(format!("invalid {name}: {error}")))?;
    let [certificate] = certificates.as_slice() else {
        return Err(invalid(format!(
            "{name} must contain exactly one certificate"
        )));
    };
    Ok(certificate.clone())
}

/// Parses a PEM document that must contain exactly one PKCS#8 private key.
pub fn pem_private_key(pem: &[u8], name: &str) -> io::Result<PrivateKeyDer<'static>> {
    let private_keys = rustls_pemfile::pkcs8_private_keys(&mut &*pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid(format!("invalid {name}: {error}")))?;
    let [private_key] = private_keys.as_slice() else {
        return Err(invalid(format!(
            "{name} must contain exactly one PKCS#8 private key"
        )));
    };
    Ok(private_key.clone_key().into())
}

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
