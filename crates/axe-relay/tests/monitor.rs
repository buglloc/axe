use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

struct Relay(Child);

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct TestDirectory(tempfile::TempDir);

impl TestDirectory {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("create isolated test directory"))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }
}

fn write_config(
    dir: &TestDirectory,
    api: u16,
    tcp: u16,
    quic: u16,
    public: u16,
    token_file: &Path,
) -> PathBuf {
    let (server_certificate, server_private_key) =
        common::quic_identity("axe-relay", rcgen::ExtendedKeyUsagePurpose::ServerAuth);
    let (client_certificate, _) =
        common::quic_identity("axe-sshd", rcgen::ExtendedKeyUsagePurpose::ClientAuth);
    for (name, contents) in [
        ("quic_server_cert.pem", server_certificate),
        ("quic_server_key.pem", server_private_key),
        ("quic_client_cert.pem", client_certificate),
    ] {
        std::fs::write(dir.path(name), contents).expect("write isolated test QUIC identity");
    }
    let config = dir.path("relay.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "http": format!("127.0.0.1:{api}"),
            "tcp_control": format!("127.0.0.1:{tcp}"),
            "quic_control": format!("127.0.0.1:{quic}"),
            "public_bind": "127.0.0.1",
            "public_host": "relay.example.test",
            "min_port": public,
            "max_port": public,
            "token_file": token_file,
            "quic_key": dir.path("quic_server_key.pem"),
            "quic_server_cert": dir.path("quic_server_cert.pem"),
            "quic_client_cert": dir.path("quic_client_cert.pem"),
        }))
        .unwrap(),
    )
    .expect("write relay config");
    config
}

fn available_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn status(api: u16) -> Option<Value> {
    let response = ureq::get(&format!("http://127.0.0.1:{api}/api/v1/status"))
        .call()
        .ok()?;
    serde_json::from_reader(response.into_body().as_reader()).ok()
}

fn events(api: u16, after: Option<(&str, u64)>) -> Value {
    let query = after.map_or(String::new(), |(epoch, cursor)| {
        format!("?epoch={epoch}&after={cursor}")
    });
    let response = ureq::get(&format!("http://127.0.0.1:{api}/api/v1/events{query}"))
        .call()
        .expect("read relay events");
    serde_json::from_reader(response.into_body().as_reader()).unwrap()
}

fn watch(
    api: u16,
    client_id: Option<&str>,
) -> (Relay, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe-relay"));
    command.args([
        "--api",
        &format!("http://127.0.0.1:{api}"),
        "watch",
        "--json",
    ]);
    if let Some(client_id) = client_id {
        command.args(["--client-id", client_id]);
    }

    let child = command
        .stdout(Stdio::piped())
        .spawn()
        .expect("start connection event feed");
    let mut watcher = Relay(child);
    let (tx, rx) = mpsc::channel();
    let output = watcher.0.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let record: Value = serde_json::from_str(&line.unwrap()).unwrap();
            if tx.send(record).is_err() {
                break;
            }
        }
    });

    (watcher, rx, reader)
}

fn await_status(api: u16, count: usize) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(snapshot) = status(api)
            && snapshot["clients"]
                .as_array()
                .is_some_and(|clients| clients.len() == count)
        {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "relay did not report {count} clients"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn monitoring_tracks_real_tcp_registration_and_disconnect() {
    let dir = TestDirectory::new();
    let api = available_tcp_port();
    let tcp = available_tcp_port();
    let public = available_tcp_port();
    let quic = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let token = "0123456789abcdef0123456789abcdef";
    let token_file = dir.path("token");
    std::fs::write(&token_file, token).expect("write token without final newline");
    let config = write_config(&dir, api, tcp, quic, public, &token_file);
    let child = Command::new(env!("CARGO_BIN_EXE_axe-relay"))
        .args(["--config", config.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start standalone relay");
    let mut relay = Relay(child);
    let empty = await_status(api, 0);
    assert_eq!(empty["version"], 1);
    assert_eq!(empty["tcp_control"], format!("127.0.0.1:{tcp}"));
    let initial = events(api, None);
    assert_eq!(initial["cursor"], 0);
    assert_eq!(initial["clients"].as_array().unwrap().len(), 0);
    let epoch = initial["epoch"].as_str().expect("relay epoch");

    let mut waiter = Command::new(env!("CARGO_BIN_EXE_axe-relay"))
        .args([
            "--api",
            &format!("http://127.0.0.1:{api}"),
            "wait",
            "--client-id",
            "test-agent",
            "--transport",
            "tcp",
            "--timeout",
            "5",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start waiting for registration");
    assert!(
        waiter.try_wait().unwrap().is_none(),
        "wait returned before registration"
    );

    let mut control = TcpStream::connect(("127.0.0.1", tcp)).expect("connect control");
    control
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut frame = Vec::new();
    ciborium::into_writer(
        &serde_json::json!({
            "version": 2, "token": token, "client_id": "test-agent"
        }),
        &mut frame,
    )
    .unwrap();
    control
        .write_all(&(frame.len() as u32).to_be_bytes())
        .unwrap();
    control.write_all(&frame).unwrap();
    let mut length = [0u8; 4];
    control
        .read_exact(&mut length)
        .expect("read registration response length");
    let mut body = vec![0; u32::from_be_bytes(length) as usize];
    control
        .read_exact(&mut body)
        .expect("read registration response");
    let response: Value = ciborium::from_reader(body.as_slice()).unwrap();
    assert_eq!(response["status"], "accepted");
    assert_eq!(
        response["public_address"],
        format!("relay.example.test:{public}")
    );

    let awaited = waiter.wait_with_output().expect("wait for registration");
    assert!(
        awaited.status.success(),
        "{}",
        String::from_utf8_lossy(&awaited.stderr)
    );
    let awaited: Value = serde_json::from_slice(&awaited.stdout).unwrap();
    assert_eq!(awaited["client_id"], "test-agent");
    assert_eq!(
        awaited["public_address"],
        format!("relay.example.test:{public}")
    );

    let active = await_status(api, 1);
    assert_eq!(active["clients"][0]["client_id"], "test-agent");
    assert_eq!(active["clients"][0]["transport"], "tcp");
    assert_eq!(
        active["clients"][0]["public_address"],
        format!("relay.example.test:{public}")
    );
    let current = events(api, None);
    assert_eq!(current["cursor"], 1);
    assert_eq!(current["clients"][0]["client_id"], "test-agent");
    let connected = events(api, Some((epoch, 0)));
    assert_eq!(connected["events"][0]["seq"], 1);
    assert_eq!(connected["events"][0]["kind"], "connected");
    assert_eq!(connected["events"][0]["client"]["client_id"], "test-agent");
    assert_eq!(
        connected["events"][0]["client"]["public_address"],
        format!("relay.example.test:{public}")
    );

    TcpStream::connect(("127.0.0.1", public))
        .expect("public listener binds separately from advertised DNS");
    let output = Command::new(env!("CARGO_BIN_EXE_axe-relay"))
        .args([
            "--api",
            &format!("http://127.0.0.1:{api}"),
            "clients",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let clients: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(clients[0]["client_id"], "test-agent");

    let expired = Command::new(env!("CARGO_BIN_EXE_axe-relay"))
        .args([
            "--api",
            &format!("http://127.0.0.1:{api}"),
            "wait",
            "--client-id",
            "test-agent",
            "--after-id",
            "0",
            "--timeout",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        !expired.status.success(),
        "old registration should not satisfy --after-id"
    );
    assert!(String::from_utf8_lossy(&expired.stderr).contains("timed out"));

    let (mut watcher, rx, reader) = watch(api, Some("test-agent"));
    let (mut all, all_rx, all_reader) = watch(api, None);
    let present = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("current client");
    assert_eq!(present["kind"], "present");
    assert_eq!(present["client"]["client_id"], "test-agent");
    assert!(present.get("seq").is_none());
    let current = all_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("unfiltered current client");
    assert_eq!(current["kind"], "present");
    assert_eq!(current["client"]["client_id"], "test-agent");

    drop(control);
    let empty = await_status(api, 0);
    assert!(empty["clients"].as_array().unwrap().is_empty());
    let history = events(api, Some((epoch, 0)));
    assert_eq!(history["cursor"], 2);
    assert_eq!(history["events"].as_array().unwrap().len(), 2);
    assert_eq!(history["events"][1]["seq"], 2);
    assert_eq!(history["events"][1]["kind"], "disconnected");
    assert_eq!(history["events"][1]["client"]["client_id"], "test-agent");
    assert_eq!(
        history["events"][1]["client"]["public_address"],
        connected["events"][0]["client"]["public_address"]
    );
    let disconnected = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("disconnect event");
    assert_eq!(disconnected["seq"], 2);
    assert_eq!(disconnected["kind"], "disconnected");
    assert_eq!(
        disconnected["client"]["public_address"],
        format!("relay.example.test:{public}")
    );
    let all_disconnected = all_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("unfiltered disconnect event");
    assert_eq!(all_disconnected["seq"], 2);
    assert_eq!(all_disconnected["kind"], "disconnected");

    let conflict = ureq::get(&format!(
        "http://127.0.0.1:{api}/api/v1/events?epoch={epoch}&after=999"
    ))
    .call()
    .unwrap_err();
    assert!(matches!(conflict, ureq::Error::StatusCode(409)));
    let restarted = ureq::get(&format!(
        "http://127.0.0.1:{api}/api/v1/events?epoch=00000000-0000-0000-0000-000000000000&after=2"
    ))
    .call()
    .unwrap_err();
    assert!(matches!(restarted, ureq::Error::StatusCode(409)));

    let mut other = TcpStream::connect(("127.0.0.1", tcp)).unwrap();
    let mut frame = Vec::new();
    ciborium::into_writer(
        &serde_json::json!({
            "version": 2, "token": token, "client_id": "other-agent"
        }),
        &mut frame,
    )
    .unwrap();
    other
        .write_all(&(frame.len() as u32).to_be_bytes())
        .unwrap();
    other.write_all(&frame).unwrap();
    other.read_exact(&mut length).unwrap();
    let mut body = vec![0; u32::from_be_bytes(length) as usize];
    other.read_exact(&mut body).unwrap();
    assert_eq!(
        ciborium::from_reader::<Value, _>(body.as_slice()).unwrap()["status"],
        "accepted"
    );

    let registered = await_status(api, 1);
    assert_eq!(registered["clients"][0]["client_id"], "other-agent");
    drop(other);
    await_status(api, 0);
    assert_eq!(
        events(api, Some((epoch, 2)))["events"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let other_connected = all_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("short-lived new client");
    let other_disconnected = all_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("short-lived client disconnect");
    assert_eq!(other_connected["kind"], "connected");
    assert_eq!(other_connected["client"]["client_id"], "other-agent");
    assert_eq!(other_disconnected["kind"], "disconnected");
    assert_eq!(other_disconnected["client"]["client_id"], "other-agent");
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(1)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(
        watcher.0.try_wait().unwrap().is_none(),
        "watcher exited unexpectedly"
    );
    drop(watcher);
    reader.join().unwrap();
    assert!(all.0.try_wait().unwrap().is_none());
    drop(all);
    all_reader.join().unwrap();
    // SAFETY: the child is still owned and running here; kill only sends it SIGTERM.
    let signaled = unsafe { libc::kill(relay.0.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(signaled, 0, "send SIGTERM to relay");
    assert!(
        relay.0.wait().unwrap().success(),
        "relay should exit cleanly on SIGTERM"
    );
}

#[test]
fn token_file_override_authenticates_tcp_registration_with_crlf() {
    let dir = TestDirectory::new();
    let api = available_tcp_port();
    let tcp = available_tcp_port();
    let public = available_tcp_port();
    let quic = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config_token_file = dir.path("configured-token");
    std::fs::write(&config_token_file, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    let override_file = dir.path("override-token");
    let token = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    std::fs::write(&override_file, format!("{token}\r\n")).unwrap();
    let config = write_config(&dir, api, tcp, quic, public, &config_token_file);
    let relay = Command::new(env!("CARGO_BIN_EXE_axe-relay"))
        .arg("--config")
        .arg(&config)
        .arg("--token")
        .arg(&override_file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start relay with overridden token file");
    let _relay = Relay(relay);
    await_status(api, 0);

    let mut control = TcpStream::connect(("127.0.0.1", tcp)).unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut frame = Vec::new();
    ciborium::into_writer(
        &serde_json::json!({"version": 2, "token": token, "client_id": "overridden"}),
        &mut frame,
    )
    .unwrap();
    control
        .write_all(&(frame.len() as u32).to_be_bytes())
        .unwrap();
    control.write_all(&frame).unwrap();
    let mut length = [0u8; 4];
    control.read_exact(&mut length).unwrap();
    let mut body = vec![0; u32::from_be_bytes(length) as usize];
    control.read_exact(&mut body).unwrap();
    let response: Value = ciborium::from_reader(body.as_slice()).unwrap();
    assert_eq!(response["status"], "accepted");
    assert_eq!(
        response["public_address"],
        format!("relay.example.test:{public}")
    );
    assert_eq!(
        await_status(api, 1)["clients"][0]["client_id"],
        "overridden"
    );
}

#[test]
fn config_rejects_invalid_token_files_and_unknown_fields() {
    let dir = TestDirectory::new();
    let token_file = dir.path("token");
    std::fs::write(&token_file, "short-token").unwrap();
    let config = write_config(&dir, 7000, 6999, 11000, 3000, &token_file);
    let relay = env!("CARGO_BIN_EXE_axe-relay");

    let missing_token = dir.path("missing-token");
    let output = Command::new(relay)
        .arg("--config")
        .arg(&config)
        .arg("--token")
        .arg(&missing_token)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(&missing_token.display().to_string()));

    let output = Command::new(relay)
        .arg("--config")
        .arg(&config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains(&token_file.display().to_string()), "{error}");
    assert!(!error.contains("short-token"), "{error}");

    let mut document: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    document["unexpected"] = Value::Bool(true);
    std::fs::write(&config, serde_json::to_vec(&document).unwrap()).unwrap();
    let output = Command::new(relay)
        .arg("--config")
        .arg(&config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected"));
}

#[test]
fn cli_rejects_remote_plain_http_before_contacting_it() {
    let output = Command::new(env!("CARGO_BIN_EXE_axe-relay"))
        .args(["--api", "http://example.com", "status"])
        .env("AXE_RELAY_API_TOKEN", "secret")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP to a loopback IP"));
}
