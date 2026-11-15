# SSH server and relay

`sshd` accepts only OpenSSH user certificates from the edition's trusted CAs. The login must be allowlisted and match a certificate principal; plain public keys and certificates with critical options are rejected. Configure the SSH host identity, trusted CAs, and default allowlist when building the edition (see [`BOOTSTRAP.md`](../BOOTSTRAP.md)). Use `--principals LOGIN,...` to override the allowlist at runtime.

```bash
axe sshd --listen '[::]:6969' --workdir .
```

Interactive SSH sessions show a short welcome message. Remote exec, SFTP, and forwarding produce no welcome output.

## SSH session environment

SSH shell and exec sessions start in the selected `workdir`. AXE sets:

```text
HOME=<workdir>/.axe-home
```

The directory is created with private permissions and persists with the workdir. It is used by tools for home-based configuration and state; the daemon's inherited home is not exposed to sessions.

If `PATH` is unset or empty when `sshd` starts, AXE uses:

```text
/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
```

The AXE applet bridge remains at the front of `PATH` when it is available. A non-empty inherited `PATH` is preserved.

SFTP resolves relative paths from `workdir` and `~` from the session home. The `expand-path@openssh.com` extension keeps `scp`'s default SFTP mode and `scp -O`'s legacy remote-shell mode consistent.

### Forwarding and session requests

TCP and Unix-domain stream sockets support local (`ssh -L`) and reverse (`ssh -R`) forwarding, including TCP-to-Unix and Unix-to-TCP endpoints. Dynamic forwarding (`ssh -D`) supports SOCKS TCP connections, not UDP.

Agent forwarding requires an explicit client request (`ssh -A`). Each session channel receives its own `SSH_AUTH_SOCK`, backed by a mode-0600 socket in a mode-0700 temporary directory. Socket connections and the directory are removed when that session ends or the SSH connection closes. Sessions without forwarding do not inherit the daemon's agent socket. At most 16 agent connections run concurrently per session.

Forward only to hosts trusted to request signatures with the agent's keys. Private keys stay in the client agent, but a process able to access the forwarded socket can request signatures while the session is open.

`SendEnv` and `SetEnv` requests are accepted before shell/exec/subsystem startup. The default allowlist is `LANG,LC_*`. Replace it with a comma-separated list such as `--accept-env='LANG,LC_*,TEST_?L*WED'`. Patterns are case-sensitive and follow OpenSSH matching: `*` matches zero or more bytes and `?` matches one byte, anywhere in the pattern. Other characters are literal; character classes, brace expansion, and `!` negation are not supported.

`--accept-env='*'` accepts every environment name without a denylist, including `LD_*`, `DYLD_*`, `SSH_*`, and `AXE*`. It only permits variables the client actually sends; it does not copy the client's entire environment. Accept-all permits loader and shell startup variables, so use it only with trusted clients.

Names must be nonempty, at most 128 bytes, and contain neither `=` nor NUL; values must contain no NUL and be at most 4096 bytes. Each session accepts at most 64 variables and 32 KiB of combined names and values.

As in OpenSSH, accepted values can be superseded by server-generated session state. AXE sets `HOME`, `AXE`, `AXE_WORK_DIR`, its Store policy, and any configured Store directory after client variables. A PTY supplies `TERM`; agent forwarding supplies the real `SSH_AUTH_SOCK` after any client-provided socket value. The managed applet bridge remains at the front of `PATH`.

On Unix, SSH signal requests target the session child's process group. Signal termination is reported through `exit-signal`; normal termination uses `exit-status`. Closing a session kills its process group and reaps the child. Remaining group members are also killed when the child exits, before its PID can be reused. Processes that deliberately create another session are outside this group.

PTY requests apply supported native terminal modes, including echo/canonical/flow-control flags, control characters, and baud rates. Window changes update the active terminal.

### SFTP extensions

Alongside `expand-path@openssh.com`, SFTP implements:

| Extension | Version | Behavior |
| --- | --- | --- |
| `fsync@openssh.com` | 1 | Flush buffered writes and synchronize an open file |
| `posix-rename@openssh.com` | 1 | Rename with replacement using native filesystem semantics |
| `hardlink@openssh.com` | 1 | Create a hard link without replacing an existing destination |
| `statvfs@openssh.com` | 2 | Return native filesystem statistics for a path (Unix) |
| `fstatvfs@openssh.com` | 2 | Return statistics through an open file handle, including an unlinked file (Unix) |
| `limits@openssh.com` | 1 | Advertise packet, read/write, and open-handle limits |

Malformed extension payloads are rejected before mutation. Closed or non-file handles fail `fsync` and `fstatvfs`.


## Relay

The target runs `axe sshd`; the separate `axe-relay` daemon assigns public TCP ports on a reachable host. Relay is disabled by default in the OSS edition. Use `--relay ENDPOINT` to enable it or `--no-relay` to override an edition's configured default.

TCP is the default transport. It uses a bearer token of at least 32 bytes, supplied by the edition or `AXE_RELAY_TOKEN`; keep its control traffic on a trusted network such as a VPN. QUIC uses mutual TLS over UDP. Supply target credentials through the edition or the `AXE_RELAY_QUIC_*_FILE` variables shown below.

The relay daemon requires a TCP token and QUIC server credentials even when targets use only one transport. Generate credentials and configure endpoints as described in [`BOOTSTRAP.md`](../BOOTSTRAP.md#relay-endpoints-and-identities).

Each registration admits at most 128 active tunnels. Additional public connections wait in the listener backlog instead of overflowing the TCP stream multiplexer. Registration, including waiting for a public port, has a 10-second deadline; opening a tunnel also has a 10-second deadline.

Clients reconnect with jittered exponential backoff capped at 30 seconds. The backoff resets after a registration remains established for 30 seconds, not after a briefly accepted connection. QUIC sends keepalives every 5 seconds and uses a 30-second idle timeout. TCP enables OS keepalive with 15-second idle and 5-second probe intervals; the retry count and total failure-detection time remain OS-dependent.

Reconnection does not resume existing tunnels. Applications must reconnect, and the replacement registration may receive a different public port: query `wait` or the API again. Duplicate client IDs can coexist as independent registrations; use unique IDs to avoid ambiguous lookup. TCP tunnels share one TCP connection and therefore share transport head-of-line blocking.


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

The server requires `--config FILE`; `--token FILE` can override its `token_file`. Both accept file paths, whereas `AXE_RELAY_TOKEN` on the TCP target contains the token itself. The dashboard/API listener is plain HTTP and may bind a reachable private or VPN address. The dashboard asks for the bearer token; every JSON API request requires `Authorization: Bearer <token>` matching `token_file`.

For TCP targets, change the loopback-only `tcp_control` address to the relay's private/VPN address and restrict access to those targets. Never expose this bearer-token listener to an untrusted network. Allow UDP 11000 from QUIC targets and TCP 3000–6000 from SSH users; keep the public port range and firewall rules in sync.

The CLI accepts both HTTPS and HTTP API origins. Set `AXE_RELAY_API_URL` and `AXE_RELAY_API_TOKEN` to let the environment select the relay, or pass `--api` and `--api-token` to override them. The default API URL is `http://127.0.0.1:7000`. Plain HTTP sends the bearer token without transport encryption; use it only on a trusted private network or put the API behind an HTTPS reverse proxy.

After installing the unit, an operator can run:

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

`status` reports uptime and active registrations; `wait` prints the target's assigned SSH `HOST:PORT` once it connects, and `watch` follows connections and disconnections (stop it with Ctrl-C). `axe-relay clients` lists current IDs, transports, and addresses; `/api/v1/status` is available for JSON monitoring with the same bearer token. Use an SSH certificate issued by the configured user CA to verify actual login via the assigned port. If the service fails to start or the target does not register, inspect `journalctl -u axe-relay.service -b --no-pager` for missing credentials, bind errors, or control handshake failures, then check endpoint routing and the relevant TCP/UDP firewall rules without printing secret values.

## Development checks

Run `just smoke` inside the development shell (`just shell`). Relay tests use their own credentials, independent of the selected edition.

To run only the relay tests:

```bash
cargo test --locked -p axe-relay --all-features
```

The local relay smoke verifies real SSH banners over TCP and QUIC, supervised-daemon registration, relay crash/re-registration, concurrent binary traffic, half-close, idle-stream isolation, and listener/target cleanup:

```bash
cargo build --locked -p axe -p axe-relay
bash scripts/relay-live-smoke.sh target/debug/axe target/debug/axe-relay
```


The local SSH interoperability smoke uses the current certificate-bearing `SSH_AUTH_SOCK`. It creates an isolated edition containing only that certificate issuer's public trust and a disposable host key; it does not create another agent or copy authentication private keys. Run inside the development shell:

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

The smoke covers OpenSSH forwarding, agent signing and isolation, environment policy, actual PTY settings/resize, signal exits, process cleanup, concurrent sessions, SFTP wire extensions, and OpenSSH SFTP transfers. It requires Linux `/proc`. Direct SSH signal requests and environment requests after exec are covered by the Rust tests because the OpenSSH CLI cannot issue them.

