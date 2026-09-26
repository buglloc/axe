# SSH server and relay

`sshd` accepts only OpenSSH user certificates issued by a CA listed in `keys/ssh/user_ca_keys`. The username must be allowlisted and match the certificate principal; plain public keys and certificates with critical options are rejected. Set up keys and the allowlist before starting the server (see [`BOOTSTRAP.md`](../BOOTSTRAP.md)).

```bash
./dist/axe-x86_64-unknown-linux-musl --applet sshd -- --listen '[::]:6969' --workdir .
```

The first successful interactive PTY session on each SSH transport receives a short welcome line pointing to `skill://axe`, `doctor --json`, and `vzik capabilities`. Later shell channels on the same multiplexed transport, remote exec, SFTP, and forwarding receive no welcome output.

## Relay

If a target cannot be reached from outside, `sshd` can register outbound with a relay. The OSS edition disables relay by default (`config/relay.json` sets `enabled_by_default` to `false` and configures no endpoints); without `--relay`, no relay task starts. `--relay ENDPOINT` enables the selected transport; `--no-relay` disables it even for editions with a configured default. The flags conflict.

TCP is the default relay transport and requires `AXE_RELAY_TOKEN` (at least 32 bytes). Use `--relay-transport quic` for QUIC; its mTLS credentials can be embedded by an edition or provided through `AXE_RELAY_QUIC_SERVER_CERT_FILE`, `AXE_RELAY_QUIC_CLIENT_CERT_FILE`, and `AXE_RELAY_QUIC_CLIENT_KEY_FILE`. See [`BOOTSTRAP.md`](../BOOTSTRAP.md#relay-endpoints-and-identities) for endpoint and server setup.

Without `--relay-id`, `sshd` uses `<pidns>@<user>@<hostname>`; this ID can collide across hosts or containers that share a PID namespace. Assign a distinct `--relay-id` when clients must be addressed uniquely. The relay dashboard/API listens on loopback by default; remote access requires an authenticated HTTPS proxy.

Targets behind NAT connect outbound with `axe sshd`. `axe-relay watch [--client-id ID]` follows arrivals and departures; `axe-relay wait --client-id ID` returns one assigned SSH `HOST:PORT`. The read-only dashboard (`/`), JSON status API (`/api/v1/status`), and `status`/`clients` commands show active registrations. See [`BOOTSTRAP.md`](../BOOTSTRAP.md#relay-endpoints-and-identities) for deployment.
