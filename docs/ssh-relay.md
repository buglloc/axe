# SSH server and relay

`sshd` accepts only OpenSSH user certificates from the edition's trusted CAs. The login must be allowlisted and match a certificate principal; plain public keys and certificates with critical options are rejected. Configure the SSH host identity, trusted CAs, and default allowlist when building the edition (see [`BOOTSTRAP.md`](../BOOTSTRAP.md)). Use `--principals LOGIN,...` to override the allowlist at runtime.

```bash
axe sshd --listen '[::]:6969' --workdir .
```

Interactive SSH sessions show a short welcome message. Remote exec, SFTP, and forwarding produce no welcome output.

## Session environment

SSH shell and exec sessions start in the selected `workdir`. AXE sets:

```text
HOME=<workdir>/.axe-home
```

AXE creates a private home directory that persists with `workdir`. Sessions do not see the daemon's own home directory.

If `PATH` is unset or empty when `sshd` starts, AXE uses:

```text
/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
```

AXE keeps a non-empty inherited `PATH` and prepends its applet bridge when available.

If `LC_ALL`, `LC_CTYPE`, and `LANG` are all unset or empty after client variables are applied, shell and exec sessions set `LANG=C.UTF-8`. A non-empty value in any of them, including `C`, prevents this. AXE does not set `LC_ALL` or change `TERM`.

SFTP resolves relative paths from `workdir` and `~` from the session home. `scp` resolves paths the same way in its default SFTP mode and in legacy `scp -O` mode.

## Forwarding

Local (`ssh -L`) and reverse (`ssh -R`) forwarding work with TCP and Unix-domain stream sockets, including TCP-to-Unix and Unix-to-TCP. Dynamic forwarding (`ssh -D`) supports SOCKS TCP connections, not UDP.

Agent forwarding requires an explicit client request (`ssh -A`). Each session gets its own private `SSH_AUTH_SOCK`, removed when the session or SSH connection ends. Sessions without forwarding do not inherit the daemon's agent socket. A session can have at most 16 concurrent agent connections.

Forward your agent only to hosts you trust to request signatures. Private keys stay in the client agent, but any process that can reach the forwarded socket can request signatures while the session is open.

## Client environment variables

`sshd` accepts `SendEnv` and `SetEnv` requests sent before the shell, exec, or subsystem starts. The default allowlist is `LANG,LC_*`. Replace it with a comma-separated list such as `--accept-env='LANG,LC_*,TEST_?L*WED'`. Patterns are case-sensitive and follow OpenSSH matching: `*` matches zero or more bytes and `?` matches one byte, anywhere in the pattern. Other characters are literal; character classes, brace expansion, and `!` negation are not supported.

`--accept-env='*'` accepts every name the client sends, including `LD_*`, `DYLD_*`, `SSH_*`, and `AXE*`. It does not copy the client's whole environment. Because it admits loader and shell startup variables, use it only with trusted clients.

Names must be non-empty, at most 128 bytes, and contain neither `=` nor NUL. Values must not contain NUL and are limited to 4096 bytes. Each session accepts at most 64 variables and 32 KiB of names and values combined.

As in OpenSSH, the server overrides some accepted values. AXE sets `HOME`, `AXE`, `AXE_WORK_DIR`, its Store mode, and any configured Store directory after applying client variables. A PTY sets `TERM`, agent forwarding sets `SSH_AUTH_SOCK`, and the applet bridge stays first in `PATH`.

## Signals and terminals

On Unix, `sshd` sends signal requests to the session's process group. A session killed by a signal reports `exit-signal`; otherwise it reports `exit-status`. When the session closes or its main process exits, `sshd` kills the remaining members of the process group. Processes that start their own session are not affected.

PTY sessions apply the client's terminal modes and follow window size changes.

## SFTP extensions

Besides `expand-path@openssh.com`, SFTP supports:

| Extension | Version | Behavior |
| --- | --- | --- |
| `fsync@openssh.com` | 1 | Flush buffered writes and synchronize an open file |
| `posix-rename@openssh.com` | 1 | Rename with replacement using native filesystem semantics |
| `hardlink@openssh.com` | 1 | Create a hard link without replacing an existing destination |
| `statvfs@openssh.com` | 2 | Return native filesystem statistics for a path (Unix) |
| `fstatvfs@openssh.com` | 2 | Return statistics through an open file handle, including an unlinked file (Unix) |
| `limits@openssh.com` | 1 | Advertise packet, read/write, and open-handle limits |

## Relay

Run `axe sshd` on the target. On a reachable host, the separate `axe-relay` daemon assigns the target a public TCP port. The OSS edition leaves relay disabled by default. Use `--relay ENDPOINT` to enable it or `--no-relay` to override an edition's configured default.

TCP is the default transport. It uses a bearer token of at least 32 bytes from the edition or `AXE_RELAY_TOKEN`. Keep TCP control traffic on a trusted network such as a VPN. QUIC uses mutual TLS over UDP. Supply target credentials through the edition or the `AXE_RELAY_QUIC_*_FILE` variables shown below.

The relay daemon needs both a TCP token and QUIC server credentials, even when targets use only one transport. Generate credentials and configure endpoints as described in [`BOOTSTRAP.md`](../BOOTSTRAP.md#relay-endpoints-and-identities).

Each registration allows at most 128 active tunnels; further public connections queue in the listener backlog. Registration, including waiting for a public port, times out after 10 seconds, as does opening a tunnel.

Targets reconnect with jittered exponential backoff of up to 30 seconds. QUIC treats a connection as dead after 30 seconds without traffic. TCP uses OS keepalive with 15-second idle and 5-second probe intervals; the total detection time depends on the OS.

Reconnecting does not resume existing tunnels. Clients must reconnect, and the target may receive a different public port. Query `wait` or the API again. All tunnels of a TCP registration share one TCP connection, so a stalled tunnel can delay the others.

Set a unique `--relay-id` for each target. The default ID can collide across hosts or containers, and registrations with the same ID coexist, making lookups ambiguous. For a QUIC target, supply the certificate files unless the edition embeds them:

```bash
AXE_RELAY_QUIC_SERVER_CERT_FILE=keys/relay/quic_server_cert.pem \
AXE_RELAY_QUIC_CLIENT_CERT_FILE=keys/relay/quic_client_cert.pem \
AXE_RELAY_QUIC_CLIENT_KEY_FILE=keys/relay/quic_client_key.pem \
axe sshd --listen '[::]:6969' --workdir . --relay relay.example.org:11000 --relay-transport quic --relay-id target-01
```

Restrict the client private key to the target's runtime user. For TCP, omit `--relay-transport quic`, use the protected TCP control endpoint (port 6999 below), and set `AXE_RELAY_TOKEN` to the relay's token. The target also needs an SSH host identity, user CA, and allowlist.

### Linux relay service

On the relay host, download the `axe-relay` binary for its architecture from the [release artifacts](../README.md#downloads) and check its SHA-256 against the published value. Provision the deployment-specific `keys/relay/token`, `quic_server_key.pem`, `quic_server_cert.pem`, and `quic_client_cert.pem` on a controlled machine and transfer them securely to `keys/relay/` on the relay host. **Do not** copy `quic_client_key.pem` to the relay host; that key belongs to the target. Do not commit these credentials or put the token in unit arguments, shell history, or logs.

```bash
sha256sum ./axe-relay  # compare with the SHA-256 for this architecture in README.md
sudo install -o root -g root -m 0755 ./axe-relay /usr/local/sbin/axe-relay
/usr/local/sbin/axe-relay --version
sudo install -d -o root -g root -m 0700 /etc/axe-relay
sudo install -o root -g root -m 0600 keys/relay/{token,quic_server_key.pem,quic_server_cert.pem,quic_client_cert.pem} /etc/axe-relay/
```

Replace `relay.example.org` with the relay's public DNS name and save this JSON as `config.json`. All fields are required. Use a bare IP address for `public_bind`. Listener addresses need a port; put IPv6 addresses in brackets.

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
  "http": "[::]:7000",
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

The server requires `--config FILE`. Use `--token FILE` to override the configured `token_file`. Both flags take paths; `AXE_RELAY_TOKEN` on a TCP target holds the token itself. The dashboard and API share a plain HTTP listener that can bind to a private or VPN address. The dashboard asks for the bearer token, and every JSON API request needs `Authorization: Bearer <token>` matching `token_file`.

For TCP targets, change the loopback-only `tcp_control` address to the relay's private or VPN address and allow only those targets to reach it. Never expose this listener to an untrusted network. Allow UDP 11000 from QUIC targets and TCP 3000–6000 from SSH users, and keep the firewall rules in sync with the public port range.

The `axe-relay` client accepts HTTP and HTTPS API URLs. Set `AXE_RELAY_API_URL` and `AXE_RELAY_API_TOKEN`, or pass `--api` and `--api-token`. The default URL is `http://127.0.0.1:7000`. Plain HTTP sends the bearer token unencrypted. Use it only on a trusted private network, or put the API behind an HTTPS reverse proxy.

Start the service and wait for a target:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now axe-relay.service
systemctl status axe-relay.service
export AXE_RELAY_API_URL="https://relay.example.org"
export AXE_RELAY_API_TOKEN="$(sudo cat /etc/axe-relay/token)"
/usr/local/sbin/axe-relay status
/usr/local/sbin/axe-relay wait --client-id target-01 --transport quic --timeout 60
/usr/local/sbin/axe-relay watch --client-id target-01
```

`status` reports uptime and active registrations. `wait` prints the target's SSH `HOST:PORT` once it connects. `watch` follows connections and disconnections until you press Ctrl-C. `axe-relay clients` lists current IDs, transports, and addresses, and `/api/v1/status` returns JSON for monitoring with the same bearer token. To confirm login, connect to the assigned port with an SSH certificate issued by the configured user CA.

If the service fails to start or the target does not register, check `journalctl -u axe-relay.service -b --no-pager` for missing credentials, bind errors, or handshake failures, then check endpoint routing and the TCP/UDP firewall rules. Do not print secret values while debugging.

## Development checks

Run `just smoke` inside the development shell (`just shell`). Relay tests use their own credentials and do not depend on the selected edition.

To run only the relay tests:

```bash
cargo test --locked -p axe-relay --all-features
```

The local relay smoke checks real SSH connections through TCP and QUIC relays, including relay restarts, re-registration, and concurrent traffic:

```bash
cargo build --locked -p axe -p axe-relay
bash scripts/relay-live-smoke.sh target/debug/axe target/debug/axe-relay
```

The SSH interoperability smoke tests AXE against the OpenSSH client. It needs Linux `/proc` and an `SSH_AUTH_SOCK` holding a user certificate. It builds a throwaway edition that trusts only that certificate's CA; it does not start another agent or copy private keys. Run it inside the development shell:

```bash
(
    set -eu
    scratch=$(mktemp -d)
    trap 'rm -rf -- "$scratch"' EXIT
    python3 scripts/ssh-live-smoke.py prepare-edition "$scratch/edition"
    AXE_EDITION_ROOT="$scratch/edition" cargo build --locked -p axe
    cp target/debug/axe "$scratch/axe"
    AXE_EDITION_ROOT="${AXE_EDITION_ROOT:-$PWD}" cargo build --locked -p axe
    python3 scripts/ssh-live-smoke.py run \
        --axe "$scratch/axe" --identity-config "$scratch/edition/smoke-identity.json"
)
```
