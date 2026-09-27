//! Relay client: keeps one registration alive and forwards every tunnel the
//! relay opens to a local TCP target.

use std::future::poll_fn;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use quinn::crypto::rustls::QuicClientConfig;
use quinn::{Endpoint, RecvStream, SendStream};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::{TcpStream, lookup_host};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use crate::protocol::{
    PROTOCOL_VERSION, QUIC_ALPN, REGISTRATION_TIMEOUT, RegistrationRequest, RegistrationResponse,
    STREAM_KIND_TCP, Transport, configure_control_socket, invalid, mux_config, pem_certificate,
    pem_private_key, quic_transport_config, read_cbor_frame, write_cbor_frame,
};

const QUIC_SERVER_NAME: &str = "axe-relay";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Relay trust and client identity for mutually authenticated QUIC.
pub struct QuicIdentity {
    server_certificate: CertificateDer<'static>,
    client_certificate: CertificateDer<'static>,
    client_private_key: PrivateKeyDer<'static>,
}

impl QuicIdentity {
    /// Accepts DER certificates and a DER PKCS#8 private key.
    pub fn from_der(
        server_certificate: &[u8],
        client_certificate: &[u8],
        client_private_key: &[u8],
    ) -> Self {
        Self {
            server_certificate: CertificateDer::from(server_certificate.to_vec()),
            client_certificate: CertificateDer::from(client_certificate.to_vec()),
            client_private_key: PrivatePkcs8KeyDer::from(client_private_key.to_vec()).into(),
        }
    }

    /// Accepts PEM documents that each contain exactly one item.
    pub fn from_pem(
        server_certificate: &[u8],
        client_certificate: &[u8],
        client_private_key: &[u8],
    ) -> io::Result<Self> {
        Ok(Self {
            server_certificate: pem_certificate(server_certificate, "QUIC server certificate")?,
            client_certificate: pem_certificate(client_certificate, "QUIC client certificate")?,
            client_private_key: pem_private_key(client_private_key, "QUIC client private key")?,
        })
    }
}

pub enum Credentials {
    /// Bearer token sent in the TCP registration.
    Token(String),
    /// Prepared mutually authenticated QUIC configuration.
    Quic(quinn::ClientConfig),
}

impl Credentials {
    pub fn quic(identity: QuicIdentity) -> io::Result<Self> {
        let mut server_roots = RootCertStore::empty();
        server_roots
            .add(identity.server_certificate)
            .map_err(|error| invalid(format!("load relay QUIC server trust: {error}")))?;
        let mut tls = rustls::ClientConfig::builder()
            .with_root_certificates(server_roots)
            .with_client_auth_cert(
                vec![identity.client_certificate],
                identity.client_private_key,
            )
            .map_err(|error| invalid(format!("load relay QUIC client identity: {error}")))?;
        tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];
        let crypto = QuicClientConfig::try_from(tls)
            .map_err(|error| invalid(format!("build relay QUIC client crypto: {error}")))?;
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        config.transport_config(quic_transport_config());
        Ok(Self::Quic(config))
    }

    fn transport(&self) -> Transport {
        match self {
            Self::Token(_) => Transport::Tcp,
            Self::Quic(_) => Transport::Quic,
        }
    }
}

pub struct Client {
    /// Relay control endpoint as `host:port`.
    pub relay: String,
    /// Local TCP address that receives forwarded connections.
    pub target: String,
    pub client_id: String,
    pub credentials: Credentials,
}

enum Exit {
    Shutdown,
    /// The relay dropped an established registration.
    Disconnected(io::Error),
}

enum Control {
    Tcp(TcpStream),
    Quic {
        endpoint: Endpoint,
        connection: quinn::Connection,
    },
}

#[allow(
    clippy::large_enum_variant,
    reason = "one control session lives on the task stack; boxing would add a connection-lifetime allocation"
)]
enum Session {
    Tcp(yamux::Connection<Compat<TcpStream>>),
    Quic {
        endpoint: Endpoint,
        connection: quinn::Connection,
    },
}

enum Tunnel {
    Tcp(yamux::Stream),
    Quic(SendStream, RecvStream),
}

impl Client {
    /// Registers and reconnects with jittered backoff until `shutdown` fires.
    pub async fn run(&self, mut shutdown: oneshot::Receiver<()>) {
        let transport = self.credentials.transport();
        let mut failures = 0_u32;

        loop {
            match self.attempt(&mut shutdown).await {
                Ok(Exit::Shutdown) => return,
                Ok(Exit::Disconnected(error)) => {
                    failures = 0;
                    eprintln!("relay: {transport}: {error}; reconnecting");
                }
                Err(error) => {
                    failures = failures.saturating_add(1).min(6);
                    eprintln!("relay: {transport}: {error}; reconnecting");
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(retry_delay(failures)) => {}
                _ = &mut shutdown => return,
            }
        }
    }

    async fn attempt(&self, shutdown: &mut oneshot::Receiver<()>) -> io::Result<Exit> {
        let mut control = tokio::select! {
            result = self.connect() => result?,
            _ = &mut *shutdown => return Ok(Exit::Shutdown),
        };
        let public_address = tokio::select! {
            result = tokio::time::timeout(REGISTRATION_TIMEOUT, self.register(&mut control)) => result.map_err(|_| io::Error::new(
                io::ErrorKind::TimedOut,
                format!("register with {}: timed out", self.relay),
            ))??,
            _ = &mut *shutdown => {
                control.into_session().close().await;
                return Ok(Exit::Shutdown);
            }
        };
        eprintln!(
            "relay: {} assigned {public_address} for {}",
            self.credentials.transport(),
            self.target
        );

        let mut session = control.into_session();
        let target = Arc::<str>::from(self.target.as_str());
        let mut tunnels = JoinSet::new();
        loop {
            tokio::select! {
                tunnel = session.next_tunnel(&self.relay) => {
                    let tunnel = match tunnel {
                        Ok(tunnel) => tunnel,
                        Err(error) => return Ok(Exit::Disconnected(error)),
                    };
                    let target = Arc::clone(&target);
                    tunnels.spawn(async move {
                        let result = tunnel.forward_to(&target).await;
                        (target, result)
                    });
                }
                completed = tunnels.join_next(), if !tunnels.is_empty() => {
                    match completed {
                        Some(Ok((_, Ok(())))) => {}
                        Some(Ok((target, Err(error)))) => {
                            eprintln!("relay: forwarded connection to {target}: {error}");
                        }
                        Some(Err(error)) => eprintln!("relay: forwarded tunnel task: {error}"),
                        None => {}
                    }
                }
                _ = &mut *shutdown => {
                    session.close().await;
                    return Ok(Exit::Shutdown);
                }
            }
        }
    }

    async fn connect(&self) -> io::Result<Control> {
        match &self.credentials {
            Credentials::Token(_) => connect_tcp(&self.relay).await.map(Control::Tcp),
            Credentials::Quic(config) => {
                let (endpoint, connection) = connect_quic(&self.relay, config).await?;
                Ok(Control::Quic {
                    endpoint,
                    connection,
                })
            }
        }
    }

    async fn register(&self, control: &mut Control) -> io::Result<String> {
        let relay = &self.relay;
        let request = RegistrationRequest {
            version: PROTOCOL_VERSION,
            token: match &self.credentials {
                Credentials::Token(token) => Some(token.into()),
                Credentials::Quic(_) => None,
            },
            client_id: self.client_id.as_str().into(),
        };
        let exchange = async {
            match control {
                Control::Tcp(stream) => {
                    write_cbor_frame(stream, &request).await?;
                    read_cbor_frame::<RegistrationResponse>(stream).await
                }
                Control::Quic { connection, .. } => {
                    let (mut send, mut recv) = connection.open_bi().await?;
                    write_cbor_frame(&mut send, &request).await?;
                    send.finish()?;
                    read_cbor_frame::<RegistrationResponse>(&mut recv).await
                }
            }
        };
        exchange
            .await
            .map_err(|error: io::Error| {
                io::Error::new(error.kind(), format!("register with {relay}: {error}"))
            })?
            .into_public_address(relay)
    }
}

impl Control {
    fn into_session(self) -> Session {
        match self {
            Self::Tcp(stream) => Session::Tcp(yamux::Connection::new(
                stream.compat(),
                mux_config(),
                yamux::Mode::Client,
            )),
            Self::Quic {
                endpoint,
                connection,
            } => Session::Quic {
                endpoint,
                connection,
            },
        }
    }
}

impl Session {
    async fn next_tunnel(&mut self, relay: &str) -> io::Result<Tunnel> {
        match self {
            Self::Tcp(mux) => match poll_fn(|context| mux.poll_next_inbound(context)).await {
                Some(Ok(stream)) => Ok(Tunnel::Tcp(stream)),
                Some(Err(error)) => Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    format!("connection to {relay}: multiplexing failed: {error}"),
                )),
                None => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("multiplexed connection to {relay} closed"),
                )),
            },
            Self::Quic { connection, .. } => connection
                .accept_bi()
                .await
                .map(|(send, recv)| Tunnel::Quic(send, recv))
                .map_err(|error| {
                    io::Error::other(format!("QUIC connection to {relay} closed: {error}"))
                }),
        }
    }

    async fn close(self) {
        if let Self::Quic {
            endpoint,
            connection,
        } = self
        {
            connection.close(quinn::VarInt::from_u32(0), b"sshd shutting down");
            let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
        }
    }
}

impl Tunnel {
    async fn forward_to(self, target: &str) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => forward(stream.compat(), target).await,
            Self::Quic(send, recv) => forward(tokio::io::join(recv, send), target).await,
        }
    }
}

async fn forward(mut tunnel: impl AsyncRead + AsyncWrite + Unpin, target: &str) -> io::Result<()> {
    let stream_kind = tunnel.read_u8().await?;
    if stream_kind != STREAM_KIND_TCP {
        return Err(invalid(format!(
            "unsupported tunnel stream type {stream_kind}"
        )));
    }

    let mut target_stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("connect {target}: timed out"),
            )
        })?
        .map_err(|error| io::Error::new(error.kind(), format!("connect {target}: {error}")))?;
    target_stream.set_nodelay(true)?;

    tokio::io::copy_bidirectional(&mut tunnel, &mut target_stream).await?;
    Ok(())
}

async fn connect_tcp(relay: &str) -> io::Result<TcpStream> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(relay))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("connect {relay}: timed out"),
            )
        })?
        .map_err(|error| io::Error::new(error.kind(), format!("connect {relay}: {error}")))?;
    configure_control_socket(&stream)
        .map_err(|error| io::Error::new(error.kind(), format!("configure {relay}: {error}")))?;
    Ok(stream)
}

async fn connect_quic(
    relay: &str,
    config: &quinn::ClientConfig,
) -> io::Result<(Endpoint, quinn::Connection)> {
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
        let connecting = match endpoint.connect(address, QUIC_SERVER_NAME) {
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

fn retry_delay(failures: u32) -> Duration {
    let ceiling = Duration::from_secs((1_u64 << failures.min(5)).min(30));
    let half = ceiling / 2;

    // Reconnect jitter only spreads clients out; it needs no secure randomness.
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| u64::from(time.subsec_nanos()));
    half + Duration::from_nanos(entropy % (half.as_nanos() as u64 + 1))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use quinn::ServerConfig;
    use quinn::crypto::rustls::QuicServerConfig;
    use tokio::net::TcpListener;

    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn tcp_client(relay: String) -> Client {
        Client {
            relay,
            target: "127.0.0.1:6969".to_owned(),
            client_id: "test-sshd".to_owned(),
            credentials: Credentials::Token(TOKEN.to_owned()),
        }
    }

    #[tokio::test]
    async fn sshd_client_reconnects_after_control_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake relay");
        let address = listener.local_addr().expect("read fake relay address");

        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("accept registration");
                let registration = read_cbor_frame::<RegistrationRequest>(&mut stream)
                    .await
                    .expect("read relay registration");
                assert_eq!(registration.version, PROTOCOL_VERSION);
                assert_eq!(registration.token.as_deref(), Some(TOKEN));
                assert_eq!(registration.client_id, "test-sshd");
                write_cbor_frame(
                    &mut stream,
                    &RegistrationResponse::accepted("127.0.0.1:3000"),
                )
                .await
                .expect("acknowledge registration");
            }
        });

        let client = tokio::spawn(async move {
            let (_shutdown, shutdown_receiver) = oneshot::channel();
            tcp_client(address.to_string()).run(shutdown_receiver).await;
        });

        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("sshd client did not reconnect")
            .expect("fake relay task failed");
        client.abort();
        let _ = client.await;
    }

    #[tokio::test]
    async fn sshd_client_abandons_unanswered_registration() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake relay");
        let address = listener.local_addr().expect("read fake relay address");

        let server = tokio::spawn(async move {
            let (mut silent, _) = listener.accept().await.expect("accept registration");
            read_cbor_frame::<RegistrationRequest>(&mut silent)
                .await
                .expect("read relay registration");
            listener
                .accept()
                .await
                .expect("accept retried registration");
            silent
        });

        let client = tokio::spawn(async move {
            let (_shutdown, shutdown_receiver) = oneshot::channel();
            tcp_client(address.to_string()).run(shutdown_receiver).await;
        });

        tokio::time::timeout(REGISTRATION_TIMEOUT + Duration::from_secs(5), server)
            .await
            .expect("sshd client waited forever for a silent relay")
            .expect("fake relay task failed");
        client.abort();
        let _ = client.await;
    }

    #[tokio::test]
    async fn rejects_server_using_embedded_client_identity() {
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![
                    pem_certificate(&read_key("quic_client_cert.pem"), "QUIC client certificate")
                        .expect("load QUIC client certificate"),
                ],
                pem_private_key(&read_key("quic_client_key.pem"), "QUIC client private key")
                    .expect("load QUIC client private key"),
            )
            .expect("build forged QUIC server identity");
        tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];
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

        let identity = QuicIdentity::from_pem(
            &read_key("quic_server_cert.pem"),
            &read_key("quic_client_cert.pem"),
            &read_key("quic_client_key.pem"),
        )
        .expect("load QUIC client identity");
        let Credentials::Quic(config) = Credentials::quic(identity).expect("build QUIC config")
        else {
            unreachable!("QUIC credentials carry a QUIC config");
        };
        let mut client =
            Endpoint::client("127.0.0.1:0".parse().expect("parse client bind address"))
                .expect("bind QUIC client");
        client.set_default_client_config(config);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client
                .connect(server_address, QUIC_SERVER_NAME)
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

    fn read_key(name: &str) -> Vec<u8> {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../keys/relay")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }
}
