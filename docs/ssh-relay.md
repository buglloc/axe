# SSH server and relay

`sshd` accepts only OpenSSH user certificates from the edition's trusted CAs. The login must be allowlisted and match a certificate principal; plain public keys and certificates with critical options are rejected. Configure the SSH host identity, trusted CAs, and default allowlist when building the edition (see [`BOOTSTRAP.md`](../BOOTSTRAP.md)). Use `--principals LOGIN,...` to override the allowlist at runtime.

```bash
axe sshd --listen '[::]:6969' --workdir .
```

Interactive SSH sessions show a short welcome message. Remote exec, SFTP, and forwarding produce no welcome output.

## Relay

The target runs `axe sshd`; the separate `axe-relay` daemon assigns public TCP ports on a reachable host. Relay is disabled by default in the OSS edition. Use `--relay ENDPOINT` to enable it or `--no-relay` to override an edition's configured default.

TCP is the default transport. It uses a bearer token of at least 32 bytes, supplied by the edition or `AXE_RELAY_TOKEN`; keep its control traffic on a trusted network such as a VPN. QUIC uses mutual TLS over UDP. Supply target credentials through the edition or the `AXE_RELAY_QUIC_*_FILE` variables shown below.

The relay daemon requires a TCP token and QUIC server credentials even when targets use only one transport. Generate credentials and configure endpoints as described in [`BOOTSTRAP.md`](../BOOTSTRAP.md#relay-endpoints-and-identities).

Assign a unique `--relay-id` to distinguish targets; the default ID can collide across hosts or containers. For a QUIC target, supply the certificate files unless the edition embeds them:

```bash
AXE_RELAY_QUIC_SERVER_CERT_FILE=keys/relay/quic_server_cert.pem \
AXE_RELAY_QUIC_CLIENT_CERT_FILE=keys/relay/quic_client_cert.pem \
AXE_RELAY_QUIC_CLIENT_KEY_FILE=keys/relay/quic_client_key.pem \
axe sshd --listen '[::]:6969' --workdir . --relay relay.example.org:11000 --relay-transport quic --relay-id target-01
```

Keep the client private key restricted to the target's runtime user. For TCP, omit `--relay-transport quic`, use the protected TCP control endpoint (port 6999 below), and set `AXE_RELAY_TOKEN` to the relay's token. The target also needs an SSH host identity, user CA, and allowlist.

### Linux relay service

On the relay host, obtain an `axe-relay` binary for its architecture from the [release artifacts](../README.md#downloads) and compare its SHA-256 with the published value before installing it. Provision the deployment-specific `keys/relay/token`, `quic_server_key.pem`, `quic_server_cert.pem`, and `quic_client_cert.pem` from a controlled machine; transfer them securely to `keys/relay/` on the relay host for the installation commands below. **Do not** transfer `quic_client_key.pem` to the relay host; that key belongs to the target. Do not commit these credentials or put the token in unit arguments, shell history, or logs.

```bash
sha256sum ./axe-relay  # compare with the SHA-256 for this architecture in README.md
sudo install -o root -g root -m 0755 ./axe-relay /usr/local/sbin/axe-relay
/usr/local/sbin/axe-relay --version
sudo install -d -o root -g root -m 0700 /etc/axe-relay
sudo install -o root -g root -m 0600 keys/relay/{token,quic_server_key.pem,quic_server_cert.pem,quic_client_cert.pem} /etc/axe-relay/
```

Replace `relay.example.org` with the relay's public DNS name. Save this JSON as `config.json`; all fields are required. `public_bind` is a bare IP address, while listener addresses include a port and bracket IPv6.

```json
{
  "tcp_control": "[::1]:6999",
  "quic_control": "[::]:11000",
  "quic_key": "/etc/axe-relay/quic_server_key.pem",
  "quic_server_cert": "/etc/axe-relay/quic_server_cert.pem",
  "quic_client_cert": "/etc/axe-relay/quic_client_cert.pem",
  "public_bind": "::",
  "public_host": "relay.example.org",
  "min_port": 3000,
  "max_port": 6000,
  "http": "[::1]:7000",
  "token_file": "/etc/axe-relay/token"
}
```

Install the configuration:

```bash
sudo install -o root -g root -m 0600 config.json /etc/axe-relay/config.json
```

Save this unit as `/etc/systemd/system/axe-relay.service` (root-owned, mode 0644):

```ini
[Unit]
Description=AXE SSH relay
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/sbin/axe-relay --config /etc/axe-relay/config.json
Restart=on-failure
RestartSec=5s
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

The server requires `--config FILE`; `--token FILE` can override its `token_file`. Both accept file paths, whereas `AXE_RELAY_TOKEN` on the TCP target contains the token itself.

For TCP targets, change the loopback-only `tcp_control` address to the relay's private/VPN address and restrict access to those targets. Never expose this bearer-token listener to an untrusted network. Allow UDP 11000 from QUIC targets and TCP 3000–6000 from SSH users; keep the public port range and firewall rules in sync.

The dashboard and JSON API have no authentication. Keep port 7000 on loopback. Remote monitoring requires an authenticated HTTPS reverse proxy with restricted access.

After installing the unit, an operator can run:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now axe-relay.service
systemctl status axe-relay.service
/usr/local/sbin/axe-relay status
/usr/local/sbin/axe-relay wait --client-id target-01 --transport quic --timeout 60
/usr/local/sbin/axe-relay watch --client-id target-01
```

`status` reports uptime and active registrations; `wait` prints the target's assigned SSH `HOST:PORT` once it connects, and `watch` follows connections and disconnections (stop it with Ctrl-C). `axe-relay clients` lists current IDs, transports, and addresses; `/api/v1/status` is available locally for JSON monitoring. Use an SSH certificate issued by the configured user CA to verify actual login via the assigned port. If the service fails to start or the target does not register, inspect `journalctl -u axe-relay.service -b --no-pager` for missing credentials, bind errors, or control handshake failures, then check endpoint routing and the relevant TCP/UDP firewall rules without printing secret values.

## Development checks

Run `just smoke` inside the development shell (`just shell`). Relay tests use their own credentials, independent of the selected edition.

To run only the relay tests:

```bash
cargo test --locked -p axe-relay --all-features
```
