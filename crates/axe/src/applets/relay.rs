use std::future::poll_fn;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use clap::ValueEnum;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use yamux::{Config, Connection, Mode, Stream};

mod quic;

const MAX_CONTROL_MESSAGE: usize = 4096;
const PROTOCOL_VERSION: u8 = 2;
const MAX_MUX_STREAMS: usize = 128;
const MAX_MUX_RECEIVE_WINDOW: usize = 32 * 1024 * 1024;
const STREAM_KIND_TCP: u8 = 1;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum RelayTransport {
    #[default]
    Tcp,
    Quic,
}

impl std::fmt::Display for RelayTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp => formatter.write_str("tcp"),
            Self::Quic => formatter.write_str("quic"),
        }
    }
}

enum ClientExit {
    Disconnected,
    Shutdown,
}

#[derive(Serialize)]
struct RegistrationRequest<'a> {
    version: u8,
    token: &'a str,
    client_id: &'a str,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum RegistrationResponse {
    Accepted { version: u8, public_address: String },
    Rejected { version: u8, message: String },
}

pub(super) async fn load_quic_client_config(
    server_certificate_path: Option<&Path>,
    client_certificate_path: Option<&Path>,
    client_private_key_path: Option<&Path>,
) -> io::Result<quinn::ClientConfig> {
    quic::load_client_config(
        server_certificate_path,
        client_certificate_path,
        client_private_key_path,
    )
    .await
}

pub async fn run_client(
    transport: RelayTransport,
    relay: &str,
    target: &str,
    client_id: &str,
    token: &str,
    quic_config: Option<quinn::ClientConfig>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut failures = 0_u32;

    loop {
        let result = match transport {
            RelayTransport::Tcp => {
                tokio::select! {
                    result = tcp_client_attempt(relay, target, client_id, token) => {
                        result.map(|()| ClientExit::Disconnected)
                    }
                    _ = &mut shutdown => return,
                }
            }
            RelayTransport::Quic => {
                quic::client_attempt(
                    relay,
                    target,
                    client_id,
                    quic_config
                        .as_ref()
                        .expect("QUIC client config was prepared before relay startup"),
                    &mut shutdown,
                )
                .await
            }
        };
        match result {
            Ok(ClientExit::Shutdown) => return,
            Ok(ClientExit::Disconnected) => failures = 0,
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

async fn tcp_client_attempt(
    relay: &str,
    target: &str,
    client_id: &str,
    token: &str,
) -> io::Result<()> {
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(relay))
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

    let mut stream = stream;
    write_cbor_frame(
        &mut stream,
        &RegistrationRequest {
            version: PROTOCOL_VERSION,
            token,
            client_id,
        },
    )
    .await
    .map_err(|error| io::Error::new(error.kind(), format!("register with {relay}: {error}")))?;

    let response = read_cbor_frame::<RegistrationResponse>(&mut stream)
        .await
        .map_err(|error| io::Error::new(error.kind(), format!("register with {relay}: {error}")))?;
    let public_address = match response {
        RegistrationResponse::Accepted {
            version,
            public_address,
        } => {
            if version != PROTOCOL_VERSION {
                return Err(invalid(format!(
                    "endpoint {relay} uses unsupported protocol version {version}"
                )));
            }
            public_address
        }
        RegistrationResponse::Rejected { version, message } => {
            if version != PROTOCOL_VERSION {
                return Err(invalid(format!(
                    "endpoint {relay} uses unsupported protocol version {version}"
                )));
            }
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("registration at {relay} rejected: {message}"),
            ));
        }
    };

    eprintln!("relay: tcp assigned {public_address} for {target}");

    let mut mux = Connection::new(stream.compat(), mux_config(), Mode::Client);
    let target = Arc::<str>::from(target);
    let mut tunnels = JoinSet::new();

    loop {
        tokio::select! {
            inbound = poll_fn(|context| mux.poll_next_inbound(context)) => {
                match inbound {
                    Some(Ok(tunnel)) => {
                        let target = target.clone();
                        tunnels.spawn(async move {
                            let result = forward_to_target(tunnel, &target).await;
                            (target, result)
                        });
                    }
                    Some(Err(error)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            format!("connection to {relay}: multiplexing failed: {error}"),
                        ));
                    }
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            format!("multiplexed connection to {relay} closed"),
                        ));
                    }
                }
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
        }
    }
}

async fn forward_to_target(tunnel: Stream, target: &str) -> io::Result<()> {
    let mut tunnel = tunnel.compat();
    let stream_kind = tunnel.read_u8().await?;
    if stream_kind != STREAM_KIND_TCP {
        return Err(invalid(format!(
            "unsupported multiplexed stream type {stream_kind}"
        )));
    }

    let mut target_stream =
        tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(target))
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

fn retry_delay(failures: u32) -> Duration {
    let ceiling = Duration::from_secs((1_u64 << failures.min(5)).min(30));
    let half = ceiling / 2;

    let jitter = Duration::from_nanos(rand::random::<u64>() % (half.as_nanos() as u64 + 1));
    half + jitter
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    #[derive(Deserialize)]
    struct Registration {
        version: u8,
        token: String,
        client_id: String,
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
                let registration = read_cbor_frame::<Registration>(&mut stream)
                    .await
                    .expect("read relay registration");
                assert_eq!(registration.version, PROTOCOL_VERSION);
                assert_eq!(registration.token, TOKEN);
                assert_eq!(registration.client_id, "test-sshd");
                write_cbor_frame(
                    &mut stream,
                    &RegistrationResponse::Accepted {
                        version: PROTOCOL_VERSION,
                        public_address: "127.0.0.1:3000".to_owned(),
                    },
                )
                .await
                .expect("acknowledge registration");
            }
        });

        let client = tokio::spawn(async move {
            let (_shutdown, shutdown_receiver) = oneshot::channel();
            run_client(
                RelayTransport::Tcp,
                &address.to_string(),
                "127.0.0.1:6969",
                "test-sshd",
                TOKEN,
                None,
                shutdown_receiver,
            )
            .await;
        });

        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("sshd client did not reconnect")
            .expect("fake relay task failed");
        client.abort();
        let _ = client.await;
    }
}
