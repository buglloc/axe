use std::future::{Future, poll_fn};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use yamux::{Config, Connection, Mode, Stream};

use crate::monitor::Monitor;

mod quic;

const MAX_CONTROL_MESSAGE: usize = 4096;
const PROTOCOL_VERSION: u8 = 2;
const MAX_MUX_STREAMS: usize = 128;
const MAX_MUX_RECEIVE_WINDOW: usize = 32 * 1024 * 1024;
const MAX_CONTROL_CONNECTIONS: usize = 256;
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(10);
const PTR_LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);
const STREAM_KIND_TCP: u8 = 1;

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

struct PublicEndpoint {
    bind: IpAddr,
    host: String,
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

#[derive(Deserialize)]
struct Registration {
    version: u8,
    token: String,
    client_id: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum RegistrationResponse<'a> {
    Accepted {
        version: u8,
        public_address: &'a str,
    },
    Rejected {
        version: u8,
        message: &'a str,
    },
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
    let token = Arc::<str>::from(options.token);
    let public = Arc::new(PublicEndpoint {
        bind: options.public_bind,
        host: options.public_host,
    });
    let permits = Arc::new(Semaphore::new(MAX_CONTROL_CONNECTIONS));
    let ports = Arc::new(Mutex::new(PortAllocator::new(
        options.min_port,
        options.max_port,
    )));
    let resolver = TokioResolver::builder_tokio()
        .ok()
        .and_then(|builder| builder.build().ok());
    let mut controls = JoinSet::new();

    let tcp_address = listener.local_addr()?;
    let quic_address = quic_endpoint.local_addr()?;
    monitor.set_addresses(tcp_address, quic_address);
    println!(
        "relay: tcp control {}; quic control {}; allocating {}:{}-{} (bind {})",
        tcp_address, quic_address, public.host, options.min_port, options.max_port, public.bind
    );

    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    eprintln!("relay: rejecting tcp control connection from {peer}: capacity exhausted");
                    continue;
                };
                let token = Arc::clone(&token);
                let public = Arc::clone(&public);
                let resolver = resolver.clone();
                let ports = Arc::clone(&ports);
                let monitor = Arc::clone(&monitor);
                controls.spawn(async move {
                    let _permit = permit;
                    let result = handle_control(
                        stream, &token, &public, &ports,
                        REGISTRATION_TIMEOUT, resolver.as_ref(), &monitor,
                    ).await;
                    ("tcp", peer, result)
                });
            }
            accepted = quic_endpoint.accept() => {
                let Some(incoming) = accepted else {
                    return Err(io::Error::other("QUIC endpoint closed"));
                };
                let peer = incoming.remote_address();
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    eprintln!("relay: rejecting quic control connection from {peer}: capacity exhausted");
                    incoming.refuse();
                    continue;
                };
                let public = Arc::clone(&public);
                let resolver = resolver.clone();
                let ports = Arc::clone(&ports);
                let monitor = Arc::clone(&monitor);
                controls.spawn(async move {
                    let _permit = permit;
                    let result = quic::handle_control(
                        incoming, &public, &ports,
                        REGISTRATION_TIMEOUT, resolver.as_ref(), &monitor,
                    ).await;
                    ("quic", peer, result)
                });
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
            }
            result = &mut shutdown => {
                result?;
                break;
            }
        }
    }

    quic_endpoint.close(quinn::VarInt::from_u32(0), b"relay shutting down");
    controls.abort_all();
    while controls.join_next().await.is_some() {}
    Ok(())
}

async fn handle_control(
    mut stream: TcpStream,
    expected_token: &str,
    public: &PublicEndpoint,
    ports: &Mutex<PortAllocator>,
    registration_timeout: Duration,
    resolver: Option<&TokioResolver>,
    monitor: &Monitor,
) -> io::Result<()> {
    configure_control_socket(&stream)?;
    let registration = match tokio::time::timeout(
        registration_timeout,
        read_cbor_frame::<Registration>(&mut stream),
    )
    .await
    {
        Ok(Ok(registration)) => registration,
        Ok(Err(error)) => {
            let _ = write_cbor_frame(
                &mut stream,
                &RegistrationResponse::Rejected {
                    version: PROTOCOL_VERSION,
                    message: "invalid registration",
                },
            )
            .await;
            return Err(error);
        }
        Err(_) => {
            let _ = write_cbor_frame(
                &mut stream,
                &RegistrationResponse::Rejected {
                    version: PROTOCOL_VERSION,
                    message: "registration timed out",
                },
            )
            .await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "registration timed out",
            ));
        }
    };
    if registration.version != PROTOCOL_VERSION {
        return reject_registration(&mut stream, "unsupported protocol version").await;
    }
    if !constant_time_eq(&registration.token, expected_token) {
        return reject_registration(&mut stream, "authentication failed").await;
    }
    if registration.client_id.is_empty() {
        return reject_registration(&mut stream, "invalid registration").await;
    }
    serve_registration(
        stream,
        &registration.client_id,
        public,
        ports,
        resolver,
        monitor,
    )
    .await
}

async fn reject_registration(stream: &mut TcpStream, message: &str) -> io::Result<()> {
    let response = RegistrationResponse::Rejected {
        version: PROTOCOL_VERSION,
        message,
    };
    let _ = write_cbor_frame(stream, &response).await;
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        message.to_owned(),
    ))
}

async fn serve_registration(
    mut stream: TcpStream,
    client_id: &str,
    endpoint: &PublicEndpoint,
    ports: &Mutex<PortAllocator>,
    resolver: Option<&TokioResolver>,
    monitor: &Monitor,
) -> io::Result<()> {
    let public = bind_port(endpoint.bind, ports).await?;
    let public_address = advertised_address(&endpoint.host, public.local_addr()?.port());
    write_cbor_frame(
        &mut stream,
        &RegistrationResponse::Accepted {
            version: PROTOCOL_VERSION,
            public_address: &public_address,
        },
    )
    .await?;

    let peer = resolve_peer(resolver, stream.peer_addr()?.ip()).await;
    let _client = monitor.register("tcp", client_id, &peer, public_address);
    let mut mux = Connection::new(stream.compat(), mux_config(), Mode::Server);
    let mut tunnels = JoinSet::new();

    loop {
        tokio::select! {
            accepted = public.accept() => {
                let (incoming, peer) = accepted?;
                if let Err(error) = incoming.set_nodelay(true) {
                    eprintln!("relay: public connection from {peer}: {error}");
                    continue;
                }
                let tunnel = poll_fn(|context| mux.poll_new_outbound(context)).await
                    .map_err(|error| io::Error::new(io::ErrorKind::ConnectionAborted,
                        format!("open multiplexed stream: {error}")))?;
                tunnels.spawn(async move {
                    let result = relay_public_connection(incoming, tunnel).await;
                    (peer, result)
                });
            }
            inbound = poll_fn(|context| mux.poll_next_inbound(context)) => {
                match inbound {
                    Some(Ok(stream)) => drop(stream),
                    Some(Err(error)) => return Err(io::Error::new(io::ErrorKind::ConnectionAborted,
                        format!("multiplexed connection: {error}"))),
                    None => return Ok(()),
                }
            }
            completed = tunnels.join_next(), if !tunnels.is_empty() => {
                match completed {
                    Some(Ok((_, Ok(())))) => {}
                    Some(Ok((peer, Err(error)))) => eprintln!("relay: public connection from {peer}: {error}"),
                    Some(Err(error)) => eprintln!("relay: public tunnel task: {error}"),
                    None => {}
                }
            }
        }
    }
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

async fn relay_public_connection(mut incoming: TcpStream, tunnel: Stream) -> io::Result<()> {
    let mut tunnel = tunnel.compat();
    tunnel.write_u8(STREAM_KIND_TCP).await?;
    tokio::io::copy_bidirectional(&mut incoming, &mut tunnel).await?;
    Ok(())
}

async fn bind_port(host: IpAddr, ports: &Mutex<PortAllocator>) -> io::Result<TcpListener> {
    ports.lock().await.bind(host).await
}

fn configure_control_socket(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    SockRef::from(stream).set_tcp_keepalive(
        &TcpKeepalive::new()
            .with_time(Duration::from_secs(15))
            .with_interval(Duration::from_secs(5)),
    )
}

fn mux_config() -> Config {
    let mut config = Config::default();
    config.set_max_num_streams(MAX_MUX_STREAMS);
    config.set_max_connection_receive_window(Some(MAX_MUX_RECEIVE_WINDOW));
    config
}

async fn write_cbor_frame(
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

async fn read_cbor_frame<T: DeserializeOwned>(
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

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
