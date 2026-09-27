use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axe_relay::protocol::{
    QUIC_ALPN, invalid, pem_certificate, pem_private_key, quic_transport_config,
};
use quinn::crypto::rustls::QuicServerConfig;
use quinn::{Endpoint, Incoming, ServerConfig};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;

use super::Control;

pub(super) async fn server_endpoint(
    address: &str,
    private_key_path: &Path,
    server_certificate_path: &Path,
    client_certificate_path: &Path,
) -> io::Result<Endpoint> {
    let address = address.parse::<SocketAddr>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid QUIC control address {address:?}: {error}"),
        )
    })?;

    let private_key = pem_private_key(
        &read_pem(private_key_path, "QUIC server private key").await?,
        "QUIC server private key",
    )?;
    let server_certificate = pem_certificate(
        &read_pem(server_certificate_path, "QUIC server certificate").await?,
        "QUIC server certificate",
    )?;
    let client_certificate = pem_certificate(
        &read_pem(client_certificate_path, "QUIC client certificate").await?,
        "QUIC client certificate",
    )?;

    Endpoint::server(
        server_config(private_key, server_certificate, client_certificate)?,
        address,
    )
    .map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("bind QUIC control {address}: {error}"),
        )
    })
}

/// Completes the mutually authenticated handshake and accepts the
/// registration stream.
pub(super) async fn accept(incoming: Incoming) -> io::Result<Control> {
    let connection = incoming
        .await
        .map_err(|error| io::Error::other(format!("QUIC handshake failed: {error}")))?;
    let (send, recv) = connection
        .accept_bi()
        .await
        .map_err(|error| io::Error::other(format!("accept registration stream: {error}")))?;
    Ok(Control::Quic {
        connection,
        send,
        recv,
    })
}

async fn read_pem(path: &Path, name: &str) -> io::Result<Vec<u8>> {
    tokio::fs::read(path).await.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read {name} {}: {error}", path.display()),
        )
    })
}

fn server_config(
    private_key: PrivateKeyDer<'static>,
    server_certificate: CertificateDer<'static>,
    client_certificate: CertificateDer<'static>,
) -> io::Result<ServerConfig> {
    let mut client_roots = RootCertStore::empty();
    client_roots
        .add(client_certificate)
        .map_err(|error| invalid(format!("load relay QUIC client trust: {error}")))?;

    let client_verifier = WebPkiClientVerifier::builder(Arc::new(client_roots))
        .build()
        .map_err(|error| invalid(format!("build relay QUIC client verifier: {error}")))?;

    let mut tls = rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(vec![server_certificate], private_key)
        .map_err(|error| invalid(format!("load relay QUIC server identity: {error}")))?;
    tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];

    let crypto = QuicServerConfig::try_from(tls)
        .map_err(|error| invalid(format!("build relay QUIC server crypto: {error}")))?;
    let mut config = ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(quic_transport_config());
    Ok(config)
}
