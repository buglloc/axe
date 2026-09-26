use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::Serialize;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, lookup_host};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use super::{
    ClientExit, PROTOCOL_VERSION, RegistrationResponse, STREAM_KIND_TCP, invalid, read_cbor_frame,
    write_cbor_frame,
};

const ALPN: &[u8] = b"axe-relay-quic/1";
const SERVER_NAME: &str = "axe-relay";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_RECEIVE_WINDOW: u32 = 16 * 1024 * 1024;
const CONNECTION_RECEIVE_WINDOW: u32 = 64 * 1024 * 1024;
const SEND_WINDOW: u64 = 64 * 1024 * 1024;
const MAX_CONCURRENT_STREAMS: u32 = 1024;

#[derive(Serialize)]
struct RegistrationRequest<'a> {
    version: u8,
    client_id: &'a str,
}

pub(super) async fn client_attempt(
    relay: &str,
    target: &str,
    client_id: &str,
    config: &ClientConfig,
    shutdown: &mut oneshot::Receiver<()>,
) -> io::Result<ClientExit> {
    let (endpoint, connection) = tokio::select! {
        result = connect(relay, config) => result?,
        _ = &mut *shutdown => return Ok(ClientExit::Shutdown),
    };
    let registration = async {
        let (mut request, mut response) = connection
            .open_bi()
            .await
            .map_err(|error| io::Error::other(format!("register with {relay}: {error}")))?;
        write_cbor_frame(
            &mut request,
            &RegistrationRequest {
                version: PROTOCOL_VERSION,
                client_id,
            },
        )
        .await
        .map_err(|error| io::Error::new(error.kind(), format!("register with {relay}: {error}")))?;
        request
            .finish()
            .map_err(|error| io::Error::other(format!("register with {relay}: {error}")))?;

        let response = read_cbor_frame::<RegistrationResponse>(&mut response)
            .await
            .map_err(|error| {
                io::Error::new(error.kind(), format!("register with {relay}: {error}"))
            })?;
        match response {
            RegistrationResponse::Accepted {
                version,
                public_address,
            } => {
                if version != PROTOCOL_VERSION {
                    return Err(invalid(format!(
                        "endpoint {relay} uses unsupported protocol version {version}"
                    )));
                }
                Ok(public_address)
            }
            RegistrationResponse::Rejected { version, message } => {
                if version != PROTOCOL_VERSION {
                    return Err(invalid(format!(
                        "endpoint {relay} uses unsupported protocol version {version}"
                    )));
                }
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("registration at {relay} rejected: {message}"),
                ))
            }
        }
    };
    let public_address = tokio::select! {
        result = registration => result?,
        _ = &mut *shutdown => {
            close_connection(&endpoint, &connection).await;
            return Ok(ClientExit::Shutdown);
        }
    };
    eprintln!("relay: quic assigned {public_address} for {target}");

    let target = Arc::<str>::from(target);
    let mut tunnels = JoinSet::new();
    loop {
        tokio::select! {
            tunnel = connection.accept_bi() => {
                let tunnel = tunnel.map_err(|error| {
                    io::Error::other(format!("QUIC connection to {relay} closed: {error}"))
                })?;
                let target = Arc::clone(&target);
                tunnels.spawn(async move {
                    let result = forward_to_target(tunnel, &target).await;
                    (target, result)
                });
            }
            completed = tunnels.join_next(), if !tunnels.is_empty() => {
                match completed {
                    Some(Ok((_, Ok(())))) => {}
                    Some(Ok((target, Err(error)))) => {
                        eprintln!("relay: forwarded connection to {target}: {error}");
                    }
                    Some(Err(error)) => eprintln!("relay: forwarded QUIC tunnel task: {error}"),
                    None => {}
                }
            }
            _ = &mut *shutdown => {
                close_connection(&endpoint, &connection).await;
                return Ok(ClientExit::Shutdown);
            }
        }
    }
}

async fn close_connection(endpoint: &Endpoint, connection: &Connection) {
    connection.close(quinn::VarInt::from_u32(0), b"sshd shutting down");
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
}

async fn connect(relay: &str, config: &ClientConfig) -> io::Result<(Endpoint, Connection)> {
    let addresses = tokio::time::timeout(CONNECT_TIMEOUT, lookup_host(relay))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("resolve {relay}: timed out"),
            )
        })?
        .map_err(|error| io::Error::new(error.kind(), format!("resolve {relay}: {error}")))?;
    let mut attempted = false;
    let mut last_error = None;

    for address in addresses {
        attempted = true;
        let bind_address = match address.ip() {
            IpAddr::V4(_) => SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0),
            IpAddr::V6(_) => SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0),
        };
        let mut endpoint = match Endpoint::client(bind_address) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        endpoint.set_default_client_config(config.clone());
        let connecting = match endpoint.connect(address, SERVER_NAME) {
            Ok(connecting) => connecting,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
            Ok(Ok(connection)) => return Ok((endpoint, connection)),
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => last_error = Some("timed out".to_owned()),
        }
    }

    let message = if attempted {
        format!(
            "connect {relay}: {}",
            last_error.as_deref().unwrap_or("connection failed")
        )
    } else {
        format!("resolve {relay}: no addresses")
    };
    Err(io::Error::other(message))
}

async fn forward_to_target(
    (send, mut recv): (SendStream, RecvStream),
    target: &str,
) -> io::Result<()> {
    let stream_kind = recv.read_u8().await?;
    if stream_kind != STREAM_KIND_TCP {
        return Err(invalid(format!(
            "unsupported QUIC stream type {stream_kind}"
        )));
    }
    let target_stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("connect {target}: timed out"),
            )
        })?
        .map_err(|error| io::Error::new(error.kind(), format!("connect {target}: {error}")))?;
    target_stream.set_nodelay(true)?;
    proxy(target_stream, send, recv).await
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

pub(super) async fn load_client_config(
    server_certificate_path: Option<&Path>,
    client_certificate_path: Option<&Path>,
    client_private_key_path: Option<&Path>,
) -> io::Result<ClientConfig> {
    match (
        server_certificate_path,
        client_certificate_path,
        client_private_key_path,
    ) {
        (None, None, None) => client_config(),
        (Some(server), Some(client), Some(key)) => {
            let server = load_certificate(server, "QUIC server certificate").await?;
            let client = load_certificate(client, "QUIC client certificate").await?;
            let key = load_private_key(key, "QUIC client private key").await?;
            build_client_config(server, client, key).map_err(invalid)
        }
        _ => Err(invalid(
            "QUIC relay credentials require server certificate, client certificate, and client private key files",
        )),
    }
}

fn client_config() -> io::Result<ClientConfig> {
    if crate::embedded::RELAY_QUIC_SERVER_CERT_DER.is_empty()
        || crate::embedded::RELAY_QUIC_CLIENT_CERT_DER.is_empty()
        || crate::embedded::RELAY_QUIC_CLIENT_KEY_DER.is_empty()
    {
        return Err(invalid(
            "relay QUIC credentials are not embedded; set AXE_RELAY_QUIC_SERVER_CERT_FILE, AXE_RELAY_QUIC_CLIENT_CERT_FILE, and AXE_RELAY_QUIC_CLIENT_KEY_FILE",
        ));
    }
    build_client_config(
        server_certificate(),
        client_certificate(),
        client_private_key(),
    )
    .map_err(invalid)
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

async fn load_private_key(path: &Path, name: &str) -> io::Result<PrivateKeyDer<'static>> {
    let pem = tokio::fs::read(path).await.map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read {name} {}: {error}", path.display()),
        )
    })?;
    let mut reader = pem.as_slice();
    let private_keys = rustls_pemfile::pkcs8_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid(format!("invalid {name}: {error}")))?;
    let [private_key] = private_keys.as_slice() else {
        return Err(invalid(format!(
            "{name} must contain exactly one PKCS#8 private key"
        )));
    };
    Ok(private_key.clone_key().into())
}

fn build_client_config(
    server_certificate: CertificateDer<'static>,
    client_certificate: CertificateDer<'static>,
    client_private_key: PrivateKeyDer<'static>,
) -> Result<ClientConfig, String> {
    let mut server_roots = RootCertStore::empty();
    server_roots
        .add(server_certificate)
        .map_err(|error| format!("load relay QUIC server trust: {error}"))?;
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(server_roots)
        .with_client_auth_cert(vec![client_certificate], client_private_key)
        .map_err(|error| format!("load relay QUIC client identity: {error}"))?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicClientConfig::try_from(tls)
        .map_err(|error| format!("build relay QUIC client crypto: {error}"))?;
    let mut config = ClientConfig::new(Arc::new(crypto));
    config.transport_config(transport_config());
    Ok(config)
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

fn server_certificate() -> CertificateDer<'static> {
    CertificateDer::from(crate::embedded::RELAY_QUIC_SERVER_CERT_DER.to_vec())
}

fn client_certificate() -> CertificateDer<'static> {
    CertificateDer::from(crate::embedded::RELAY_QUIC_CLIENT_CERT_DER.to_vec())
}

fn client_private_key() -> PrivateKeyDer<'static> {
    PrivatePkcs8KeyDer::from(crate::embedded::RELAY_QUIC_CLIENT_KEY_DER.to_vec()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use quinn::ServerConfig;
    use quinn::crypto::rustls::QuicServerConfig;

    #[tokio::test]
    async fn rejects_server_using_embedded_client_identity() {
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![
                    load_certificate(&client_certificate_path(), "QUIC client certificate")
                        .await
                        .expect("load QUIC client certificate"),
                ],
                load_private_key(&client_private_key_path(), "QUIC client private key")
                    .await
                    .expect("load QUIC client private key"),
            )
            .expect("build forged QUIC server identity");
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let crypto = QuicServerConfig::try_from(tls).expect("build forged QUIC server crypto");
        let server = Endpoint::server(
            ServerConfig::with_crypto(Arc::new(crypto)),
            "127.0.0.1:0".parse().expect("parse server bind address"),
        )
        .expect("bind forged QUIC server");
        let server_address = server.local_addr().expect("read forged server address");
        let server_task = tokio::spawn(async move {
            server
                .accept()
                .await
                .expect("accept forged QUIC connection attempt")
                .await
        });

        let mut client =
            Endpoint::client("127.0.0.1:0".parse().expect("parse client bind address"))
                .expect("bind QUIC client");
        client.set_default_client_config(
            load_client_config(
                Some(&server_certificate_path()),
                Some(&client_certificate_path()),
                Some(&client_private_key_path()),
            )
            .await
            .expect("load QUIC client config"),
        );
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client
                .connect(server_address, SERVER_NAME)
                .expect("start forged QUIC connection"),
        )
        .await
        .expect("QUIC server authentication did not finish");

        assert!(result.is_err(), "client trusted its own identity as relay");
        assert!(
            server_task
                .await
                .expect("forged server handshake task failed")
                .is_err(),
            "forged relay completed a QUIC handshake"
        );
    }

    fn server_certificate_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../keys/relay/quic_server_cert.pem")
    }

    fn client_certificate_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../keys/relay/quic_client_cert.pem")
    }

    fn client_private_key_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../keys/relay/quic_client_key.pem")
    }
}
