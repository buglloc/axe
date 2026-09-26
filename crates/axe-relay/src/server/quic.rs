use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hickory_resolver::TokioResolver;
use quinn::crypto::rustls::QuicServerConfig;
use quinn::{Connection, Endpoint, Incoming, RecvStream, SendStream, ServerConfig};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use super::{
    PROTOCOL_VERSION, PortAllocator, PublicEndpoint, RegistrationResponse, STREAM_KIND_TCP,
    advertised_address, bind_port, invalid, read_cbor_frame, resolve_peer, write_cbor_frame,
};
use crate::monitor::Monitor;

const ALPN: &[u8] = b"axe-relay-quic/1";
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_RECEIVE_WINDOW: u32 = 16 * 1024 * 1024;
const CONNECTION_RECEIVE_WINDOW: u32 = 64 * 1024 * 1024;
const SEND_WINDOW: u64 = 64 * 1024 * 1024;
const MAX_CONCURRENT_STREAMS: u32 = 1024;

#[derive(Deserialize)]
struct Registration {
    version: u8,
    client_id: String,
}

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

    let private_key = load_server_private_key(private_key_path).await?;
    let server_certificate =
        load_certificate(server_certificate_path, "QUIC server certificate").await?;
    let client_certificate =
        load_certificate(client_certificate_path, "QUIC client certificate").await?;

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

pub(super) async fn handle_control(
    incoming: Incoming,
    public: &PublicEndpoint,
    ports: &Mutex<PortAllocator>,
    registration_timeout: Duration,
    resolver: Option<&TokioResolver>,
    monitor: &Monitor,
) -> io::Result<()> {
    let connection = tokio::time::timeout(registration_timeout, incoming)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "QUIC handshake timed out"))?
        .map_err(|error| io::Error::other(format!("QUIC handshake failed: {error}")))?;

    let (mut response, mut request) =
        tokio::time::timeout(registration_timeout, connection.accept_bi())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "registration timed out"))?
            .map_err(|error| io::Error::other(format!("accept registration stream: {error}")))?;
    let registration = read_cbor_frame::<Registration>(&mut request).await?;

    if registration.version != PROTOCOL_VERSION {
        return reject_registration(&mut response, "unsupported protocol version").await;
    }
    if registration.client_id.is_empty() {
        return reject_registration(&mut response, "invalid registration").await;
    }

    serve_registration(
        connection,
        response,
        &registration,
        public,
        ports,
        resolver,
        monitor,
    )
    .await
}

async fn reject_registration(response: &mut SendStream, message: &str) -> io::Result<()> {
    let rejection = RegistrationResponse::Rejected {
        version: PROTOCOL_VERSION,
        message,
    };
    let _ = write_cbor_frame(response, &rejection).await;
    let _ = response.finish();
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        message.to_owned(),
    ))
}

async fn serve_registration(
    connection: Connection,
    mut response: SendStream,
    registration: &Registration,
    endpoint: &PublicEndpoint,
    ports: &Mutex<PortAllocator>,
    resolver: Option<&TokioResolver>,
    monitor: &Monitor,
) -> io::Result<()> {
    let public = bind_port(endpoint.bind, ports).await?;
    let public_address = advertised_address(&endpoint.host, public.local_addr()?.port());

    write_cbor_frame(
        &mut response,
        &RegistrationResponse::Accepted {
            version: PROTOCOL_VERSION,
            public_address: &public_address,
        },
    )
    .await?;
    response
        .finish()
        .map_err(|error| io::Error::other(format!("finish registration response: {error}")))?;

    let peer = resolve_peer(resolver, connection.remote_address().ip()).await;
    let _client = monitor.register("quic", &registration.client_id, &peer, public_address);
    let mut tunnels = JoinSet::new();

    loop {
        tokio::select! {
            accepted = public.accept() => {
                let (incoming, peer) = accepted?;
                if let Err(error) = incoming.set_nodelay(true) {
                    eprintln!("relay: public connection from {peer}: {error}");
                    continue;
                }
                let tunnel = connection.open_bi().await
                    .map_err(|error| io::Error::other(format!("open QUIC stream: {error}")))?;
                tunnels.spawn(async move {
                    let result = relay_public_connection(incoming, tunnel).await;
                    (peer, result)
                });
            }
            error = connection.closed() => {
                match error {
                    quinn::ConnectionError::ApplicationClosed(close)
                        if close.error_code == quinn::VarInt::from_u32(0) => return Ok(()),
                    quinn::ConnectionError::LocallyClosed => return Ok(()),
                    error => return Err(io::Error::other(format!("QUIC connection closed: {error}"))),
                }
            }
            completed = tunnels.join_next(), if !tunnels.is_empty() => {
                match completed {
                    Some(Ok((_, Ok(())))) => {}
                    Some(Ok((peer, Err(error)))) => eprintln!("relay: public connection from {peer}: {error}"),
                    Some(Err(error)) => eprintln!("relay: public QUIC tunnel task: {error}"),
                    None => {}
                }
            }
        }
    }
}

async fn relay_public_connection(
    incoming: TcpStream,
    (mut send, recv): (SendStream, RecvStream),
) -> io::Result<()> {
    send.write_u8(STREAM_KIND_TCP).await?;
    proxy(incoming, send, recv).await
}

async fn proxy(mut tcp: TcpStream, mut send: SendStream, mut recv: RecvStream) -> io::Result<()> {
    let (mut tcp_read, mut tcp_write) = tcp.split();
    let upload = async {
        tokio::io::copy(&mut tcp_read, &mut send).await?;
        send.finish()
            .map_err(|error| io::Error::other(format!("finish QUIC stream: {error}")))
    };
    let download = async {
        tokio::io::copy(&mut recv, &mut tcp_write).await?;
        tcp_write.shutdown().await
    };
    tokio::try_join!(upload, download)?;
    Ok(())
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
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let crypto = QuicServerConfig::try_from(tls)
        .map_err(|error| invalid(format!("build relay QUIC server crypto: {error}")))?;
    let mut config = ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(transport_config());
    Ok(config)
}

async fn load_server_private_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    let pem = tokio::fs::read(path).await.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read QUIC server private key {}: {error}", path.display()),
        )
    })?;

    let mut reader = pem.as_slice();
    let private_keys = rustls_pemfile::pkcs8_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid(format!("invalid QUIC server private key: {error}")))?;

    let [private_key] = private_keys.as_slice() else {
        return Err(invalid(
            "QUIC server private key must contain exactly one PKCS#8 private key",
        ));
    };
    Ok(private_key.clone_key().into())
}

async fn load_certificate(path: &Path, name: &str) -> io::Result<CertificateDer<'static>> {
    let pem = tokio::fs::read(path).await.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read {name} {}: {error}", path.display()),
        )
    })?;

    let mut reader = pem.as_slice();
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid(format!("invalid {name}: {error}")))?;

    let [certificate] = certificates.as_slice() else {
        return Err(invalid(format!(
            "{name} must contain exactly one certificate"
        )));
    };
    Ok(certificate.clone())
}

fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    config.max_idle_timeout(Some(
        IDLE_TIMEOUT
            .try_into()
            .expect("relay QUIC idle timeout is representable"),
    ));
    config.stream_receive_window(quinn::VarInt::from_u32(STREAM_RECEIVE_WINDOW));
    config.receive_window(quinn::VarInt::from_u32(CONNECTION_RECEIVE_WINDOW));
    config.send_window(SEND_WINDOW);
    config.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    config.max_concurrent_bidi_streams(quinn::VarInt::from_u32(MAX_CONCURRENT_STREAMS));
    Arc::new(config)
}
