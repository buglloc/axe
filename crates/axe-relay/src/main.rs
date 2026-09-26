mod http;
mod monitor;
mod server;

use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use monitor::{Client, EventBatch, EventKind, Monitor, Status};
use serde::de::DeserializeOwned;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "axe-relay",
    version,
    about = "Standalone AXE relay and monitoring client"
)]
struct Options {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, default_value = "http://127.0.0.1:7000", global = true)]
    api: String,
    #[arg(long, env = "AXE_RELAY_API_TOKEN", global = true)]
    api_token: Option<String>,
    #[arg(long, default_value = "[::]:6999")]
    tcp_control: String,
    #[arg(long, default_value = "[::]:11000")]
    quic_control: String,
    #[arg(long, env = "AXE_RELAY_QUIC_KEY", value_name = "FILE")]
    quic_key: Option<PathBuf>,
    #[arg(long, env = "AXE_RELAY_QUIC_SERVER_CERT_FILE", value_name = "FILE")]
    quic_server_cert: Option<PathBuf>,
    #[arg(long, env = "AXE_RELAY_QUIC_CLIENT_CERT_FILE", value_name = "FILE")]
    quic_client_cert: Option<PathBuf>,
    #[arg(long, default_value = "0.0.0.0")]
    public_bind: IpAddr,
    #[arg(long)]
    public_host: Option<String>,
    #[arg(long, default_value_t = 3000)]
    min_port: u16,
    #[arg(long, default_value_t = 4000)]
    max_port: u16,
    #[arg(long, env = "AXE_RELAY_TOKEN")]
    token: Option<String>,
    #[arg(long, default_value = "127.0.0.1:7000")]
    http: SocketAddr,
}

#[derive(Subcommand)]
enum Command {
    /// Show control listeners, uptime and active client count
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List connected relay clients and their public addresses
    Clients {
        #[arg(long)]
        json: bool,
    },

    /// Wait until an AXE SSH server registers, then print its public address
    Wait {
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        transport: Option<Transport>,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Require a registration newer than this session ID
        #[arg(long)]
        after_id: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Print active clients, then follow connection and disconnection events
    Watch {
        #[arg(long)]
        client_id: Option<String>,
        /// Emit one JSON object per line
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Transport {
    Tcp,
    Quic,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Quic => "quic",
        }
    }
}

struct ApiClient {
    agent: ureq::Agent,
    status_endpoint: String,
    events_endpoint: String,
    authorization: Option<String>,
}

fn main() {
    let options = Options::parse();
    let result = match &options.command {
        Some(command) => query(&options.api, options.api_token.as_deref(), command),
        None => start(options),
    };
    if let Err(error) = result {
        eprintln!("axe-relay: {error}");
        std::process::exit(1);
    }
}

impl ApiClient {
    fn new(api: &str, token: Option<&str>) -> Result<Self, String> {
        let base = url::Url::parse(api).map_err(|error| format!("invalid API URL: {error}"))?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err("API URL must be an origin without credentials, query or path".into());
        }
        let local_http = matches!(base.host(), Some(url::Host::Ipv4(address)) if address.is_loopback())
            || matches!(base.host(), Some(url::Host::Ipv6(address)) if address.is_loopback());
        if base.scheme() != "https" && !(base.scheme() == "http" && local_http) {
            return Err("API URL must use HTTPS or HTTP to a loopback IP".into());
        }
        let status_endpoint = base
            .join("/api/v1/status")
            .map_err(|error| error.to_string())?;
        let events_endpoint = base
            .join("/api/v1/events")
            .map_err(|error| error.to_string())?;
        Ok(Self {
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .max_redirects(0)
                .build()
                .into(),
            status_endpoint: status_endpoint.to_string(),
            events_endpoint: events_endpoint.to_string(),
            authorization: token.map(|token| format!("Bearer {token}")),
        })
    }

    fn status(&self) -> Result<Status, String> {
        let status: Status = self.get(&self.status_endpoint)?;
        if status.version != 1 {
            return Err(format!("unsupported API version {}", status.version));
        }
        Ok(status)
    }

    fn events(&self, after: Option<(Uuid, u64)>) -> Result<EventBatch, String> {
        match after {
            Some((epoch, cursor)) => self.get(&format!(
                "{}?epoch={epoch}&after={cursor}",
                self.events_endpoint
            )),
            None => self.get(&self.events_endpoint),
        }
    }

    fn get<T: DeserializeOwned>(&self, endpoint: &str) -> Result<T, String> {
        let request = self.agent.get(endpoint);
        let mut response = match self.authorization.as_deref() {
            Some(authorization) => request.header("Authorization", authorization).call(),
            None => request.call(),
        }
        .map_err(|error| match error {
            ureq::Error::StatusCode(409) => {
                "event history expired or relay restarted; restart watch".to_owned()
            }
            _ => format!("request failed: {error}"),
        })?;
        let body = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_vec()
            .map_err(|error| format!("read response: {error}"))?;
        serde_json::from_slice(&body).map_err(|error| format!("invalid API response: {error}"))
    }
}

fn query(api: &str, token: Option<&str>, command: &Command) -> Result<(), String> {
    let client = ApiClient::new(api, token)?;
    if let Command::Wait {
        client_id,
        transport,
        timeout,
        after_id,
        json,
    } = command
    {
        let registration = wait_for(&client, client_id, *transport, *timeout, *after_id)?;
        if *json {
            println!(
                "{}",
                serde_json::to_string_pretty(&registration).map_err(|error| error.to_string())?
            );
        } else {
            println!("{}", registration.public_address);
        }
        return Ok(());
    }
    if let Command::Watch { client_id, json } = command {
        return watch(&client, client_id.as_deref(), *json);
    }
    let status = client.status()?;
    match command {
        Command::Status { json: true } => println!(
            "{}",
            serde_json::to_string_pretty(&status).map_err(|error| error.to_string())?
        ),
        Command::Clients { json: true } => println!(
            "{}",
            serde_json::to_string_pretty(&status.clients).map_err(|error| error.to_string())?
        ),
        Command::Status { json: false } => println!(
            "uptime={}s clients={} tcp={} quic={}",
            status.uptime_seconds,
            status.clients.len(),
            status.tcp_control,
            status.quic_control
        ),
        Command::Clients { json: false } => {
            for client in status.clients {
                println!(
                    "{}\t{}\t{}\t{}\t{}s",
                    client.client_id,
                    client.transport,
                    client.peer,
                    client.public_address,
                    client.connected_seconds
                );
            }
        }
        Command::Wait { .. } | Command::Watch { .. } => {
            unreachable!("streaming commands handled before the snapshot")
        }
    }
    Ok(())
}
#[derive(serde::Serialize)]
struct WatchRecord<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    kind: &'static str,
    client: &'a Client,
}

fn watch(client: &ApiClient, client_id: Option<&str>, json: bool) -> Result<(), String> {
    let initial = client.events(None)?;
    let epoch = initial.epoch;
    let mut cursor = initial.cursor;
    let mut stdout = io::stdout().lock();
    for registration in &initial.clients {
        if client_id.is_none_or(|id| id == registration.client_id) {
            print_record(
                &mut stdout,
                json,
                WatchRecord {
                    seq: None,
                    kind: "present",
                    client: registration,
                },
            )?;
        }
    }
    loop {
        let batch = client.events(Some((epoch, cursor)))?;
        for event in &batch.events {
            if client_id.is_some_and(|id| id != event.client.client_id) {
                continue;
            }
            let kind = match event.kind {
                EventKind::Connected => "connected",
                EventKind::Disconnected => "disconnected",
            };
            print_record(
                &mut stdout,
                json,
                WatchRecord {
                    seq: Some(event.seq),
                    kind,
                    client: &event.client,
                },
            )?;
        }
        cursor = batch.cursor;
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn print_record(
    stdout: &mut impl Write,
    json: bool,
    record: WatchRecord<'_>,
) -> Result<(), String> {
    if json {
        serde_json::to_writer(&mut *stdout, &record).map_err(|error| error.to_string())?;
        writeln!(stdout).map_err(|error| error.to_string())?;
    } else {
        writeln!(
            stdout,
            "{}\t{:?}\t{}\t{}",
            record.kind,
            record.client.client_id,
            record.client.transport,
            record.client.public_address
        )
        .map_err(|error| error.to_string())?;
    }
    stdout.flush().map_err(|error| error.to_string())
}

fn wait_for(
    client: &ApiClient,
    client_id: &str,
    transport: Option<Transport>,
    timeout: u64,
    after_id: Option<u64>,
) -> Result<Client, String> {
    let deadline = Duration::from_secs(timeout);
    let started = Instant::now();
    loop {
        let status = client.status()?;
        if let Some(registration) = status.clients.into_iter().find(|client| {
            client.client_id == client_id
                && transport.is_none_or(|transport| client.transport == transport.as_str())
                && after_id.is_none_or(|id| client.id > id)
        }) {
            return Ok(registration);
        }
        let Some(remaining) = deadline.checked_sub(started.elapsed()) else {
            return Err(format!(
                "timed out after {timeout}s waiting for relay client {client_id:?}"
            ));
        };
        std::thread::sleep(remaining.min(Duration::from_millis(250)));
    }
}

fn start(options: Options) -> Result<(), String> {
    if !options.http.ip().is_loopback() {
        return Err("dashboard/API must bind to a loopback address; use an authenticated reverse proxy for remote access".into());
    }
    if options.min_port == 0 || options.min_port > options.max_port {
        return Err("invalid public port range".into());
    }
    let token = options
        .token
        .ok_or("set AXE_RELAY_TOKEN or --token (at least 32 bytes)")?;
    if token.len() < 32 {
        return Err("relay token must contain at least 32 bytes".into());
    }
    let public_host = options
        .public_host
        .ok_or("set --public-host to the address or DNS name reachable by SSH users")?;
    let public_host = match public_host.parse::<std::net::IpAddr>() {
        Ok(address) => address.to_string(),
        Err(_) => match url::Host::parse(&public_host) {
            Ok(url::Host::Domain(domain)) => domain,
            Ok(url::Host::Ipv6(address)) => address.to_string(),
            _ => {
                return Err(
                    "--public-host must be an IP address or DNS name without a port".into(),
                );
            }
        },
    };
    let server_options = server::ServerOptions {
        tcp_control: options.tcp_control,
        quic_control: options.quic_control,
        quic_key: options
            .quic_key
            .ok_or("set AXE_RELAY_QUIC_KEY or --quic-key")?,
        quic_server_cert: options
            .quic_server_cert
            .ok_or("set AXE_RELAY_QUIC_SERVER_CERT_FILE or --quic-server-cert")?,
        quic_client_cert: options
            .quic_client_cert
            .ok_or("set AXE_RELAY_QUIC_CLIENT_CERT_FILE or --quic-client-cert")?,
        public_bind: options.public_bind,
        public_host,
        min_port: options.min_port,
        max_port: options.max_port,
        token,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create runtime: {error}"))?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(options.http).await
            .map_err(|error| format!("bind dashboard {}: {error}", options.http))?;
        let monitor = Arc::new(Monitor::new());
        let dashboard = axum::serve(listener, http::router(Arc::clone(&monitor)))
            .with_graceful_shutdown(async { let _ = shutdown_signal().await; });
        println!("relay: dashboard/API http://{}", options.http);
        tokio::select! {
            result = server::run(server_options, monitor, shutdown_signal()) => result.map_err(|error| error.to_string()),
            result = dashboard => result.map_err(|error| format!("dashboard: {error}")),
        }
    })
}

#[cfg(unix)]
async fn shutdown_signal() -> io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> io::Result<()> {
    tokio::signal::ctrl_c().await
}
