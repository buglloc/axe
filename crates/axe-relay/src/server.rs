use std::future::{Future, poll_fn};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use axe_relay::Transport;
use axe_relay::protocol::{
    MAX_TUNNELS, PROTOCOL_VERSION, REGISTRATION_TIMEOUT, RegistrationRequest, RegistrationResponse,
    STREAM_KIND_TCP, TUNNEL_OPEN_TIMEOUT, configure_control_socket, mux_config, read_cbor_frame,
    write_cbor_frame,
};
use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use crate::monitor::Monitor;

mod quic;

const MAX_CONTROL_CONNECTIONS: usize = 256;
const PTR_LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);

pub struct ServerOptions {
    pub tcp_control: String,
    pub quic_control: String,
    pub quic_key: PathBuf,
    pub quic_server_cert: PathBuf,
    pub quic_client_cert: PathBuf,
    pub public_bind: IpAddr,
    pub public_host: String,
    pub min_port: u16,
    pub max_port: u16,
    pub token: String,
}

struct PortAllocator {
    min_port: u16,
    max_port: u16,
    next_port: u16,
}

impl PortAllocator {
    fn new(min_port: u16, max_port: u16) -> Self {
        Self {
            min_port,
            max_port,
            next_port: min_port,
        }
    }

    fn successor(&self, port: u16) -> u16 {
        if port == self.max_port {
            self.min_port
        } else {
            port + 1
        }
    }

    async fn bind(&mut self, host: IpAddr) -> io::Result<TcpListener> {
        let start = self.next_port;
        let mut port = start;
        loop {
            match TcpListener::bind(SocketAddr::new(host, port)).await {
                Ok(listener) => {
                    self.next_port = self.successor(port);
                    return Ok(listener);
                }
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
                Err(error) => return Err(error),
            }

            port = self.successor(port);
            if port == start {
                break;
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "no public port available in {}-{}",
                self.min_port, self.max_port
            ),
        ))
    }
}

/// State shared by every control connection.
struct Relay {
    token: String,
    public_bind: IpAddr,
    public_host: String,
    ports: Mutex<PortAllocator>,
    resolver: Option<TokioResolver>,
    monitor: Arc<Monitor>,
    registration_timeout: Duration,
}

/// A control connection accepted by one transport listener.
#[allow(
    clippy::large_enum_variant,
    reason = "an accepted connection is moved directly into its task; boxing would add an allocation"
)]
enum Accepted {
    Tcp(TcpStream),
    Quic(quinn::Incoming),
}

/// An established control channel whose registration is not answered yet.
enum Control {
    Tcp(TcpStream),
    Quic {
        connection: quinn::Connection,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
    },
}

/// A registered control channel that carries tunnels.
#[allow(
    clippy::large_enum_variant,
    reason = "one control session lives on the task stack; boxing would add a connection-lifetime allocation"
)]
enum Session {
    Tcp(yamux::Connection<Compat<TcpStream>>),
    Quic(quinn::Connection),
}

enum Tunnel {
    Tcp(yamux::Stream),
    /// QUIC streams are opened by the tunnel task so a stalled peer cannot
    /// block the registration loop.
    Quic(quinn::Connection),
}

pub async fn run(
    options: ServerOptions,
    monitor: Arc<Monitor>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    if options.min_port == 0 || options.min_port > options.max_port {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid public port range",
        ));
    }

    if options.token.len() < 32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "token must contain at least 32 bytes",
        ));
    }

    let listener = TcpListener::bind(&options.tcp_control).await?;
    let quic_endpoint = quic::server_endpoint(
        &options.quic_control,
        &options.quic_key,
        &options.quic_server_cert,
        &options.quic_client_cert,
    )
    .await?;

    let relay = Arc::new(Relay {
        token: options.token,
        public_bind: options.public_bind,
        public_host: options.public_host,
        ports: Mutex::new(PortAllocator::new(options.min_port, options.max_port)),
        resolver: TokioResolver::builder_tokio()
            .ok()
            .and_then(|builder| builder.build().ok()),
        monitor,
        registration_timeout: REGISTRATION_TIMEOUT,
    });

    let tcp_address = listener.local_addr()?;
    let quic_address = quic_endpoint.local_addr()?;
    relay.monitor.set_addresses(tcp_address, quic_address);
    println!(
        "relay: tcp control {}; quic control {}; allocating {}:{}-{} (bind {})",
        tcp_address,
        quic_address,
        relay.public_host,
        options.min_port,
        options.max_port,
        relay.public_bind
    );

    serve(
        listener,
        quic_endpoint,
        relay,
        MAX_CONTROL_CONNECTIONS,
        shutdown,
    )
    .await
}

async fn serve(
    listener: TcpListener,
    quic_endpoint: quinn::Endpoint,
    relay: Arc<Relay>,
    control_connections: usize,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let permits = Arc::new(Semaphore::new(control_connections));
    let mut controls = JoinSet::new();

    tokio::pin!(shutdown);
    loop {
        let (accepted, transport, peer) = tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                (Accepted::Tcp(stream), Transport::Tcp, peer)
            }
            accepted = quic_endpoint.accept() => {
                let Some(incoming) = accepted else {
                    return Err(io::Error::other("QUIC endpoint closed"));
                };
                let peer = incoming.remote_address();
                (Accepted::Quic(incoming), Transport::Quic, peer)
            }
            completed = controls.join_next(), if !controls.is_empty() => {
                match completed {
                    Some(Ok((_, _, Ok(())))) => {}
                    Some(Ok((transport, peer, Err(error)))) => {
                        eprintln!("relay: {transport} control connection from {peer}: {error}");
                    }
                    Some(Err(error)) => eprintln!("relay: control task: {error}"),
                    None => {}
                }
                continue;
            }
            result = &mut shutdown => {
                result?;
                break;
            }
        };

        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            eprintln!(
                "relay: rejecting {transport} control connection from {peer}: capacity exhausted"
            );
            if let Accepted::Quic(incoming) = accepted {
                incoming.refuse();
            }
            continue;
        };
        let relay = Arc::clone(&relay);
        controls.spawn(async move {
            let _permit = permit;
            let result = relay.handle_control(accepted).await;
            (transport, peer, result)
        });
    }

    quic_endpoint.close(quinn::VarInt::from_u32(0), b"relay shutting down");
    controls.abort_all();
    while controls.join_next().await.is_some() {}
    Ok(())
}

impl Relay {
    /// Authenticates one control channel, then serves its registration.
    ///
    /// One deadline bounds establishment and the registration request on both
    /// transports, so an idle client cannot hold a control permit.
    async fn handle_control(&self, accepted: Accepted) -> io::Result<()> {
        let deadline = Instant::now() + self.registration_timeout;
        let mut control = match accepted {
            Accepted::Tcp(stream) => {
                configure_control_socket(&stream)?;
                Control::Tcp(stream)
            }
            Accepted::Quic(incoming) => tokio::time::timeout_at(deadline, quic::accept(incoming))
                .await
                .map_err(|_| registration_timed_out())??,
        };

        let request = match tokio::time::timeout_at(deadline, control.read_request()).await {
            Ok(Ok(request)) => request,
            Ok(Err(error)) => {
                let _ = self
                    .reject(&mut control, "invalid registration", deadline)
                    .await;
                return Err(error);
            }
            Err(_) => {
                return Err(registration_timed_out());
            }
        };

        let expected_token = match control {
            Control::Tcp(_) => Some(self.token.as_str()),
            Control::Quic { .. } => None,
        };
        match registration_error(&request, expected_token) {
            Some(message) => self.reject(&mut control, message, deadline).await,
            None => {
                self.serve_registration(control, &request.client_id, deadline)
                    .await
            }
        }
    }

    async fn reject(
        &self,
        control: &mut Control,
        message: &str,
        deadline: Instant,
    ) -> io::Result<()> {
        let response = RegistrationResponse::rejected(message);
        let _ = tokio::time::timeout_at(deadline, control.respond(&response)).await;
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            message.to_owned(),
        ))
    }

    async fn serve_registration(
        &self,
        mut control: Control,
        client_id: &str,
        deadline: Instant,
    ) -> io::Result<()> {
        let public = tokio::time::timeout_at(deadline, async {
            self.ports.lock().await.bind(self.public_bind).await
        })
        .await
        .map_err(|_| registration_timed_out())??;
        let public_address = advertised_address(&self.public_host, public.local_addr()?.port());

        tokio::time::timeout_at(
            deadline,
            control.respond(&RegistrationResponse::accepted(&public_address)),
        )
        .await
        .map_err(|_| registration_timed_out())??;

        let transport = control.transport();
        let peer = resolve_peer(self.resolver.as_ref(), control.peer()?).await;
        let _client = self
            .monitor
            .register(transport.as_str(), client_id, &peer, public_address);
        let mut session = control.into_session();
        let mut tunnels = JoinSet::new();

        let result = loop {
            tokio::select! {
                accepted = public.accept(), if tunnels.len() < MAX_TUNNELS => {
                    let (incoming, peer) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => break Err(error),
                    };
                    if let Err(error) = incoming.set_nodelay(true) {
                        eprintln!("relay: public connection from {peer}: {error}");
                        continue;
                    }
                    let tunnel = match session.open_tunnel().await {
                        Ok(tunnel) => tunnel,
                        Err(error) => break Err(error),
                    };
                    tunnels.spawn(async move {
                        let result = tunnel.relay(incoming).await;
                        (peer, result)
                    });
                }
                result = session.closed() => break result,
                completed = tunnels.join_next(), if !tunnels.is_empty() => {
                    match completed {
                        Some(Ok((_, Ok(())))) => {}
                        Some(Ok((peer, Err(error)))) => eprintln!("relay: public connection from {peer}: {error}"),
                        Some(Err(error)) => eprintln!("relay: public {transport} tunnel task: {error}"),
                        None => {}
                    }
                }
            }
        };
        tunnels.shutdown().await;
        session.close();
        drop(public);
        result
    }
}

/// Returns the rejection message for an invalid registration. TCP clients
/// must present the bearer token; QUIC clients already proved their identity.
fn registration_error(
    request: &RegistrationRequest,
    expected_token: Option<&str>,
) -> Option<&'static str> {
    if request.version != PROTOCOL_VERSION {
        return Some("unsupported protocol version");
    }
    if let Some(expected_token) = expected_token {
        let Some(token) = request.token.as_deref() else {
            return Some("invalid registration");
        };
        if !constant_time_eq(token, expected_token) {
            return Some("authentication failed");
        }
    }
    if request.client_id.is_empty() {
        return Some("invalid registration");
    }
    None
}

impl Control {
    fn transport(&self) -> Transport {
        match self {
            Self::Tcp(_) => Transport::Tcp,
            Self::Quic { .. } => Transport::Quic,
        }
    }

    fn peer(&self) -> io::Result<IpAddr> {
        match self {
            Self::Tcp(stream) => Ok(stream.peer_addr()?.ip()),
            Self::Quic { connection, .. } => Ok(connection.remote_address().ip()),
        }
    }

    async fn read_request(&mut self) -> io::Result<RegistrationRequest<'static>> {
        match self {
            Self::Tcp(stream) => read_cbor_frame(stream).await,
            Self::Quic { recv, .. } => read_cbor_frame(recv).await,
        }
    }

    async fn respond(&mut self, response: &RegistrationResponse<'_>) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => write_cbor_frame(stream, response).await,
            Self::Quic { send, .. } => {
                write_cbor_frame(send, response).await?;
                send.finish().map_err(|error| {
                    io::Error::other(format!("finish registration response: {error}"))
                })
            }
        }
    }

    fn into_session(self) -> Session {
        match self {
            Self::Tcp(stream) => Session::Tcp(yamux::Connection::new(
                stream.compat(),
                mux_config(),
                yamux::Mode::Server,
            )),
            Self::Quic { connection, .. } => Session::Quic(connection),
        }
    }
}

impl Session {
    async fn open_tunnel(&mut self) -> io::Result<Tunnel> {
        match self {
            Self::Tcp(mux) => tokio::time::timeout(
                TUNNEL_OPEN_TIMEOUT,
                poll_fn(|context| {
                    // Yamux only drives frames and removes dropped streams while
                    // polling inbound; outbound polling alone cannot free capacity.
                    loop {
                        match mux.poll_next_inbound(context) {
                            Poll::Ready(Some(Ok(stream))) => drop(stream),
                            Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                            Poll::Ready(None) => {
                                return Poll::Ready(Err(yamux::ConnectionError::Closed));
                            }
                            Poll::Pending => break,
                        }
                    }
                    mux.poll_new_outbound(context)
                }),
            )
            .await
            .map_err(|_| tunnel_open_timed_out())?
            .map(Tunnel::Tcp)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    format!("open multiplexed stream: {error}"),
                )
            }),
            Self::Quic(connection) => Ok(Tunnel::Quic(connection.clone())),
        }
    }

    /// Completes when the client closes the control channel.
    async fn closed(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(mux) => loop {
                match poll_fn(|context| mux.poll_next_inbound(context)).await {
                    Some(Ok(stream)) => drop(stream),
                    Some(Err(error)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            format!("multiplexed connection: {error}"),
                        ));
                    }
                    None => return Ok(()),
                }
            },
            Self::Quic(connection) => match connection.closed().await {
                quinn::ConnectionError::ApplicationClosed(close)
                    if close.error_code == quinn::VarInt::from_u32(0) =>
                {
                    Ok(())
                }
                quinn::ConnectionError::LocallyClosed => Ok(()),
                error => Err(io::Error::other(format!("QUIC connection closed: {error}"))),
            },
        }
    }

    fn close(self) {
        if let Self::Quic(connection) = self {
            connection.close(quinn::VarInt::from_u32(0), b"registration ended");
        }
    }
}

impl Tunnel {
    async fn relay(self, incoming: TcpStream) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => relay_tcp(incoming, stream.compat()).await,
            Self::Quic(connection) => {
                let (send, recv) = tokio::time::timeout(TUNNEL_OPEN_TIMEOUT, connection.open_bi())
                    .await
                    .map_err(|_| tunnel_open_timed_out())?
                    .map_err(|error| io::Error::other(format!("open QUIC stream: {error}")))?;
                relay_tcp(incoming, tokio::io::join(recv, send)).await
            }
        }
    }
}

async fn relay_tcp(
    mut incoming: TcpStream,
    mut tunnel: impl AsyncRead + AsyncWrite + Unpin,
) -> io::Result<()> {
    tunnel.write_u8(STREAM_KIND_TCP).await?;
    tokio::io::copy_bidirectional(&mut incoming, &mut tunnel).await?;
    Ok(())
}

fn registration_timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "registration timed out")
}

fn tunnel_open_timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "open tunnel timed out")
}

fn advertised_address(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(address) => SocketAddr::new(address, port).to_string(),
        Err(_) => format!("{host}:{port}"),
    }
}

async fn resolve_peer(resolver: Option<&TokioResolver>, address: IpAddr) -> String {
    let Some(resolver) = resolver else {
        return address.to_string();
    };

    let lookup = tokio::time::timeout(PTR_LOOKUP_TIMEOUT, resolver.reverse_lookup(address)).await;
    let name = match lookup {
        Ok(Ok(response)) => response.answers().iter().find_map(|record| {
            if let RData::PTR(name) = &record.data {
                Some(name.to_string())
            } else {
                None
            }
        }),
        Ok(Err(_)) | Err(_) => None,
    };
    name.map(|name| name.trim_end_matches('.').to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| address.to_string())
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

#[cfg(test)]
mod tests {
    use axe_relay::client::{Credentials, QuicIdentity};

    use super::*;
    use crate::test_support::quic_identity;
    use tokio::io::AsyncReadExt;

    struct ForwardingHarness {
        relay: Arc<Relay>,
        client: Option<axe_relay::client::Client>,
        tasks: JoinSet<()>,
        public_reservation: Option<TcpListener>,
        _keys: tempfile::TempDir,
    }

    impl ForwardingHarness {
        async fn new(transport: Transport) -> Self {
            let keys = tempfile::tempdir().expect("create test keys");
            let (server_certificate, server_private_key) =
                quic_identity("axe-relay", rcgen::ExtendedKeyUsagePurpose::ServerAuth);
            let (client_certificate, client_private_key) =
                quic_identity("axe-sshd", rcgen::ExtendedKeyUsagePurpose::ClientAuth);
            for (name, contents) in [
                ("server.key", server_private_key.as_bytes()),
                ("server.pem", server_certificate.as_bytes()),
                ("client.pem", client_certificate.as_bytes()),
            ] {
                std::fs::write(keys.path().join(name), contents).expect("write test keys");
            }
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let tcp_address = listener.local_addr().unwrap();
            let endpoint = quic::server_endpoint(
                "127.0.0.1:0",
                &keys.path().join("server.key"),
                &keys.path().join("server.pem"),
                &keys.path().join("client.pem"),
            )
            .await
            .unwrap();
            let quic_address = endpoint.local_addr().unwrap();
            let public = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let public_port = public.local_addr().unwrap().port();
            let token = "0123456789abcdef0123456789abcdef";
            let relay = Arc::new(Relay {
                token: token.to_owned(),
                public_bind: IpAddr::from([127, 0, 0, 1]),
                public_host: "127.0.0.1".to_owned(),
                ports: Mutex::new(PortAllocator::new(public_port, public_port)),
                resolver: None,
                monitor: Arc::new(Monitor::new()),
                registration_timeout: REGISTRATION_TIMEOUT,
            });
            relay.monitor.set_addresses(tcp_address, quic_address);
            let credentials = match transport {
                Transport::Tcp => Credentials::Token(token.to_owned()),
                Transport::Quic => Credentials::quic(
                    QuicIdentity::from_pem(
                        server_certificate.as_bytes(),
                        client_certificate.as_bytes(),
                        client_private_key.as_bytes(),
                    )
                    .unwrap(),
                )
                .unwrap(),
            };
            let client = axe_relay::client::Client {
                relay: match transport {
                    Transport::Tcp => tcp_address,
                    Transport::Quic => quic_address,
                }
                .to_string(),
                target: String::new(),
                client_id: "forwarding-test".to_owned(),
                credentials,
            };
            let mut tasks = JoinSet::new();
            let server_relay = Arc::clone(&relay);
            tasks.spawn(async move {
                serve(listener, endpoint, server_relay, 8, std::future::pending())
                    .await
                    .unwrap();
            });
            Self {
                relay,
                client: Some(client),
                tasks,
                public_reservation: Some(public),
                _keys: keys,
            }
        }

        async fn start_client(&mut self) -> tokio::sync::oneshot::Sender<()> {
            let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut client = self.client.take().unwrap();
            client.target = target.local_addr().unwrap().to_string();
            drop(self.public_reservation.take());
            self.tasks.spawn(async move {
                let mut connections = JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = target.accept() => {
                            let (mut stream, _) = accepted.unwrap();
                            connections.spawn(async move {
                                stream.write_all(b"ready").await?;
                                let (mut read, mut write) = stream.split();
                                tokio::io::copy(&mut read, &mut write).await?;
                                write.shutdown().await
                            });
                        }
                        _ = connections.join_next(), if !connections.is_empty() => {}
                    }
                }
            });
            let (stop, stopped) = tokio::sync::oneshot::channel();
            self.tasks.spawn(async move { client.run(stopped).await });
            stop
        }

        async fn clients(&self, count: usize) -> Vec<crate::monitor::Client> {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let clients = self.relay.monitor.snapshot().unwrap().clients;
                    if clients.len() == count {
                        return clients;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("registration state did not converge")
        }
    }

    #[tokio::test]
    async fn tcp_capacity_preserves_registration_and_recovers_after_half_close() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut harness = ForwardingHarness::new(Transport::Tcp).await;
            let stop = harness.start_client().await;
            let initial = harness.clients(1).await.remove(0);
            let mut peers = Vec::new();
            for _ in 0..MAX_TUNNELS {
                let mut peer = TcpStream::connect(&initial.public_address).await.unwrap();
                let mut ready = [0; 5];
                peer.read_exact(&mut ready).await.unwrap();
                assert_eq!(&ready, b"ready");
                peers.push(peer);
            }
            let mut overflow = TcpStream::connect(&initial.public_address).await.unwrap();
            let mut ready = [0; 5];
            assert!(
                tokio::time::timeout(Duration::from_millis(100), overflow.read_exact(&mut ready))
                    .await
                    .is_err(),
                "excess traffic must wait, not tear down the registration"
            );
            let mut first = peers.remove(0);
            first.write_all(b"half-close-response").await.unwrap();
            first.shutdown().await.unwrap();
            let mut echoed = Vec::new();
            first.read_to_end(&mut echoed).await.unwrap();
            assert_eq!(echoed, b"half-close-response");
            overflow.read_exact(&mut ready).await.unwrap();
            assert_eq!(&ready, b"ready");
            assert_eq!(harness.clients(1).await[0].id, initial.id);
            stop.send(()).unwrap();
            harness.clients(0).await;
            let mut closed = [0];
            assert_eq!(overflow.read(&mut closed).await.unwrap(), 0);
            let rebound = TcpListener::bind(&initial.public_address).await;
            assert!(rebound.is_ok(), "public port leaked after client shutdown");
            harness.tasks.shutdown().await;
        })
        .await
        .expect("capacity/half-close scenario stalled");
    }

    #[tokio::test]
    async fn quic_concurrent_tunnels_preserve_half_close_and_release_port() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut harness = ForwardingHarness::new(Transport::Quic).await;
            let stop = harness.start_client().await;
            let initial = harness.clients(1).await.remove(0);
            let mut traffic = JoinSet::new();
            for sequence in 0..16_u8 {
                let address = initial.public_address.clone();
                traffic.spawn(async move {
                    let mut peer = TcpStream::connect(address).await.unwrap();
                    let mut ready = [0; 5];
                    peer.read_exact(&mut ready).await.unwrap();
                    assert_eq!(&ready, b"ready");
                    let payload = vec![sequence; 128 * 1024];
                    let (mut read, mut write) = peer.split();
                    let (_, received) = tokio::join!(
                        async {
                            write.write_all(&payload).await.unwrap();
                            write.shutdown().await.unwrap();
                        },
                        async {
                            let mut echoed = Vec::new();
                            read.read_to_end(&mut echoed).await.unwrap();
                            echoed
                        }
                    );
                    assert_eq!(received, payload);
                });
            }
            while let Some(result) = traffic.join_next().await {
                result.unwrap();
            }
            assert_eq!(harness.clients(1).await[0].id, initial.id);
            stop.send(()).unwrap();
            harness.clients(0).await;
            assert!(
                TcpListener::bind(&initial.public_address).await.is_ok(),
                "QUIC public port leaked after shutdown"
            );
            harness.tasks.shutdown().await;
        })
        .await
        .expect("QUIC concurrent half-close scenario stalled");
    }

    #[tokio::test]
    async fn registration_deadline_includes_waiting_for_allocator() {
        tokio::time::timeout(REGISTRATION_TIMEOUT + Duration::from_secs(5), async {
            let mut harness = ForwardingHarness::new(Transport::Tcp).await;
            let allocator = harness.relay.ports.lock().await;
            let mut control = TcpStream::connect(&harness.client.as_ref().unwrap().relay)
                .await
                .unwrap();
            write_cbor_frame(
                &mut control,
                &RegistrationRequest {
                    version: PROTOCOL_VERSION,
                    token: Some("0123456789abcdef0123456789abcdef".into()),
                    client_id: "allocator-blocked".into(),
                },
            )
            .await
            .unwrap();
            let response = read_cbor_frame::<RegistrationResponse>(&mut control).await;
            match response {
                Ok(_) => panic!("registration outlived its allocator deadline"),
                Err(error) => assert!(matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                )),
            }
            drop(allocator);
            let stop = harness.start_client().await;
            harness.clients(1).await;
            stop.send(()).unwrap();
            harness.clients(0).await;
            harness.tasks.shutdown().await;
        })
        .await
        .expect("allocator contention bypassed registration deadline");
    }

    #[tokio::test]
    async fn exhausted_single_port_range_recovers_after_listener_drop() {
        let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = reservation.local_addr().unwrap();
        let mut allocator = PortAllocator::new(address.port(), address.port());
        assert_eq!(
            allocator.bind(address.ip()).await.unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        drop(reservation);
        let allocated = allocator.bind(address.ip()).await.unwrap();
        assert_eq!(allocated.local_addr().unwrap(), address);
        assert_eq!(
            allocator.bind(address.ip()).await.unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        drop(allocated);
        assert_eq!(
            allocator
                .bind(address.ip())
                .await
                .unwrap()
                .local_addr()
                .unwrap(),
            address
        );
    }

    #[tokio::test]
    async fn silent_quic_registration_releases_control_capacity() {
        let keys = tempfile::tempdir().expect("create isolated test key directory");
        let (server_certificate, server_private_key) =
            quic_identity("axe-relay", rcgen::ExtendedKeyUsagePurpose::ServerAuth);
        let (client_certificate, client_private_key) =
            quic_identity("axe-sshd", rcgen::ExtendedKeyUsagePurpose::ClientAuth);
        for (name, contents) in [
            ("quic_server_key.pem", server_private_key.as_bytes()),
            ("quic_server_cert.pem", server_certificate.as_bytes()),
            ("quic_client_cert.pem", client_certificate.as_bytes()),
        ] {
            std::fs::write(keys.path().join(name), contents).expect("write test QUIC identity");
        }
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind TCP control");
        let quic_endpoint = quic::server_endpoint(
            "127.0.0.1:0",
            &keys.path().join("quic_server_key.pem"),
            &keys.path().join("quic_server_cert.pem"),
            &keys.path().join("quic_client_cert.pem"),
        )
        .await
        .expect("bind QUIC control");
        let quic_address = quic_endpoint.local_addr().expect("read QUIC address");
        let relay = Arc::new(Relay {
            token: String::new(),
            public_bind: IpAddr::from([127, 0, 0, 1]),
            public_host: "127.0.0.1".to_owned(),
            ports: Mutex::new(PortAllocator::new(1024, u16::MAX)),
            resolver: None,
            monitor: Arc::new(Monitor::new()),
            registration_timeout: Duration::from_millis(300),
        });
        let (_stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(listener, quic_endpoint, relay, 1, async move {
            let _ = stopped.await;
            Ok(())
        }));

        let identity = QuicIdentity::from_pem(
            server_certificate.as_bytes(),
            client_certificate.as_bytes(),
            client_private_key.as_bytes(),
        )
        .expect("parse QUIC identity");
        let Credentials::Quic(config) = Credentials::quic(identity).expect("build QUIC config")
        else {
            unreachable!("QUIC credentials carry a QUIC config");
        };
        let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().expect("parse address"))
            .expect("bind QUIC client");
        client.set_default_client_config(config);

        let silent = client
            .connect(quic_address, "axe-relay")
            .expect("start silent connection")
            .await
            .expect("authenticate silent client");
        let (mut send, _recv) = silent.open_bi().await.expect("open registration stream");
        send.write_all(&[0])
            .await
            .expect("start registration frame");
        tokio::time::timeout(Duration::from_secs(5), silent.closed())
            .await
            .expect("relay kept a silent registration open");

        let registered = client
            .connect(quic_address, "axe-relay")
            .expect("start registering connection")
            .await
            .expect("authenticate registering client");
        let (mut send, mut recv) = registered
            .open_bi()
            .await
            .expect("open registration stream");
        let request = RegistrationRequest {
            version: PROTOCOL_VERSION,
            token: None,
            client_id: "after-timeout".into(),
        };
        write_cbor_frame(&mut send, &request)
            .await
            .expect("send registration");
        send.finish().expect("finish registration");
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            read_cbor_frame::<RegistrationResponse>(&mut recv),
        )
        .await
        .expect("relay did not answer after capacity recovered")
        .expect("read registration response");
        response
            .into_public_address("test relay")
            .expect("relay accepted registration after capacity recovered");

        server.abort();
    }
}
