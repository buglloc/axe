# SSH server and relay

`sshd` accepts only OpenSSH user certificates issued by a CA listed in `keys/ssh/user_ca_keys`. The username must be allowlisted and match the certificate principal; plain public keys and certificates with critical options are rejected. Set up keys and the allowlist before starting the server (see [`BOOTSTRAP.md`](../BOOTSTRAP.md)).

```bash
axe sshd --listen '[::]:6969' --workdir .
```

The first successful interactive PTY session on each SSH transport receives a short welcome line pointing to `skill://axe`, `doctor --json`, and `vzik capabilities`. Later shell channels on the same multiplexed transport, remote exec, SFTP, and forwarding receive no welcome output.

## Relay

The target runs `axe sshd`; the separate `axe-relay` daemon runs on a reachable Linux host and assigns public TCP ports to registered targets. The OSS `edition.json` disables relay by default and configures no endpoints. Without `--relay`, no relay task starts. `--relay ENDPOINT` enables the selected transport; `--no-relay` disables it even for editions with a configured default. The flags conflict.

TCP is the default target transport and requires `AXE_RELAY_TOKEN` (at least 32 bytes). Its control registration sends a bearer token, so carry TCP control traffic only over a trusted, protected network (for example a VPN); do not expose it directly to an untrusted network. For QUIC, use `--relay-transport quic` and a UDP endpoint. QUIC uses mutual TLS; target credentials can be embedded by an edition or provided through `AXE_RELAY_QUIC_SERVER_CERT_FILE`, `AXE_RELAY_QUIC_CLIENT_CERT_FILE`, and `AXE_RELAY_QUIC_CLIENT_KEY_FILE`. The relay daemon requires the TCP token and QUIC server key and certificates **even if targets use only one transport**: it starts both control listeners. Generate deployment credentials and configure target endpoints as described in [`BOOTSTRAP.md`](../BOOTSTRAP.md#relay-endpoints-and-identities).

Without `--relay-id`, `sshd` uses `<pidns>@<user>@<hostname>`; this ID can collide across hosts or containers that share a PID namespace. Assign a distinct `--relay-id` when clients must be addressed uniquely. For example, on a target whose QUIC endpoint is `relay.example.org:11000`, supply its readable QUIC files unless the edition already embeds them:

```bash
AXE_RELAY_QUIC_SERVER_CERT_FILE=keys/relay/quic_server_cert.pem \
AXE_RELAY_QUIC_CLIENT_CERT_FILE=keys/relay/quic_client_cert.pem \
AXE_RELAY_QUIC_CLIENT_KEY_FILE=keys/relay/quic_client_key.pem \
axe sshd --listen '[::]:6969' --workdir . --relay relay.example.org:11000 --relay-transport quic --relay-id target-01
```

This starts the target SSH server, **not** the relay daemon. Keep the target's client private key restricted to its runtime user. On a TCP target, use its protected TCP control endpoint (port 6999 in the example below), omit `--relay-transport quic`, and provision the same TCP token to the target. Configure the target's SSH host identity, user CA, and allowlist before starting it. Registration alone does not prove that SSH authentication and forwarding work.

### Linux relay service

On the relay host, obtain an `axe-relay` binary for its architecture from the [release artifacts](../README.md#downloads) and compare its SHA-256 with the published value before installing it. Provision the deployment-specific `keys/relay/token`, `quic_server_key.pem`, `quic_server_cert.pem`, and `quic_client_cert.pem` from a controlled machine; transfer them securely to `keys/relay/` on the relay host for the installation commands below. **Do not** transfer `quic_client_key.pem` to the relay host; that key belongs to the target. Do not commit these credentials or put the token in unit arguments, shell history, or logs.

```bash
sha256sum ./axe-relay  # compare with the SHA-256 for this architecture in README.md
sudo install -o root -g root -m 0755 ./axe-relay /usr/local/bin/axe-relay
/usr/local/bin/axe-relay --version
sudo useradd --system --no-create-home --shell /usr/sbin/nologin axe-relay
sudo install -d -o root -g root -m 0700 /etc/axe-relay
sudo install -o root -g root -m 0600 keys/relay/{token,quic_server_key.pem,quic_server_cert.pem,quic_client_cert.pem} /etc/axe-relay/
```

The example uses `relay.example.org` as the public DNS name; replace it with a name resolving to this host's public IPv4 address. Save the following as `/etc/systemd/system/axe-relay.service` (root-owned, mode 0644). systemd copies the root-only files into a private credentials directory for the service. The shell reads the token from there into `AXE_RELAY_TOKEN` without exposing its value in the unit or command-line arguments; the daemon reads the certificate files from the same directory. The generated token has no trailing newline; the unit accepts a nonempty value read at EOF.

```ini
[Unit]
Description=AXE SSH relay
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=axe-relay
Group=axe-relay
LoadCredential=relay-token:/etc/axe-relay/token
LoadCredential=quic_server_key.pem:/etc/axe-relay/quic_server_key.pem
LoadCredential=quic_server_cert.pem:/etc/axe-relay/quic_server_cert.pem
LoadCredential=quic_client_cert.pem:/etc/axe-relay/quic_client_cert.pem
ExecStart=/bin/sh -c 'AXE_RELAY_TOKEN=; IFS= read -r AXE_RELAY_TOKEN < "$$CREDENTIALS_DIRECTORY/relay-token" || test -n "$$AXE_RELAY_TOKEN" || exit 1; export AXE_RELAY_TOKEN; exec /usr/local/bin/axe-relay --tcp-control 127.0.0.1:6999 --quic-control "[::]:11000" --quic-key "$$CREDENTIALS_DIRECTORY/quic_server_key.pem" --quic-server-cert "$$CREDENTIALS_DIRECTORY/quic_server_cert.pem" --quic-client-cert "$$CREDENTIALS_DIRECTORY/quic_client_cert.pem" --public-bind 0.0.0.0 --public-host relay.example.org --min-port 3000 --max-port 4000 --http 127.0.0.1:7000'
Restart=on-failure
RestartSec=5s
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

The TCP control listener above is loopback-only. For TCP targets on a protected network, change `--tcp-control` to the relay host's private/VPN address and restrict firewall access to those targets. For QUIC targets, allow UDP 11000 to the relay from those targets. Allow incoming TCP 3000–4000 from SSH users: these are assigned public SSH ports, **not** control ports or the target's `--listen` port. Adjust the range and firewall together. The monitor HTTP listener stays at `127.0.0.1:7000`; its dashboard and JSON API do **not** enforce authentication. Never expose port 7000 directly. For remote monitoring, place an authenticated HTTPS reverse proxy in front of this loopback listener and restrict access to the proxy.

After installing the unit, an operator can run:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now axe-relay.service
systemctl status axe-relay.service
axe-relay status
axe-relay wait --client-id target-01 --transport quic --timeout 60
axe-relay watch --client-id target-01
```

`status` reports uptime and active registrations; `wait` prints the target's assigned SSH `HOST:PORT` once it connects, and `watch` follows connections and disconnections (stop it with Ctrl-C). `axe-relay clients` lists current IDs, transports, and addresses; `/api/v1/status` is available locally for JSON monitoring. Use an SSH certificate issued by the configured user CA to verify actual login via the assigned port. If the service fails to start or the target does not register, inspect `journalctl -u axe-relay.service -b --no-pager` for missing credentials, bind errors, or control handshake failures, then check endpoint routing and the relevant TCP/UDP firewall rules without printing secret values.
