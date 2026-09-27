# Bootstrapping AXE

This guide covers inputs that an operator must provision: edition identities, Store IAM, relay deployment, and optional remote builders. Use [README.md](README.md#building-from-source) for local builds, [docs/store.md](docs/store.md) for package and Store behavior, and [docs/release.md](docs/release.md) for release publication.

## Development setup

Run builds and checks in the development shell:

```bash
nix develop .#default
just generate-dev-keys
```

`generate-dev-keys` creates only missing development identities. It refuses to replace a complete key pair. Never distribute generated development private keys or use them as production trust material.

## Edition root

`AXE_EDITION_ROOT` selects one complete build-input bundle. Missing files are not borrowed from the public checkout.

```bash
export AXE_EDITION_ROOT=/path/to/edition
```

Required public inputs:

- `edition.json`: schema version, edition ID, SSH principals, and relay defaults;
- `config/store.json`: Store endpoint, limits, pinned addresses, and cache roots;
- `store/trusted/*.pub`: Store verification keys;
- `store/bootstrap.json`: generated package inventory;
- `store/bootstrap-index.cbor.zst`: signed bootstrap snapshot when the edition ships one.

Required local identities:

- `keys/ssh/host_ed25519`: embedded SSH host private key;
- `keys/ssh/user_ca_keys`: trusted OpenSSH user CA public keys, one per line.

Conditional inputs:

- `keys/relay/token` when a TCP endpoint is configured;
- `keys/relay/quic_server_cert.pem`, `quic_client_cert.pem`, and `quic_client_key.pem` when a QUIC endpoint is configured;
- `store/nix/assets/trusted_ca.pem` for additional HTTPS roots.

The build validates all JSON, aliases, SSH keys, certificates, Store trust, and bootstrap metadata before compiling the binary. `config/aliases.json` is source-owned and shared by all editions. Publisher keys, S3 credentials, and the relay server private key are never embedded.

A minimal `edition.json` uses schema 2:

```json
{
  "schema_version": 2,
  "id": "my-axe",
  "sshd": {
    "principals": ["alice", "bob"]
  },
  "relay": {
    "enabled_by_default": false,
    "tcp_endpoint": null,
    "quic_endpoint": null
  }
}
```

To start from the OSS Store consumer inputs:

```bash
umask 077
mkdir -p "$AXE_EDITION_ROOT/config" "$AXE_EDITION_ROOT/store"
cp config/store.json "$AXE_EDITION_ROOT/config/"
cp store/bootstrap.json store/bootstrap-index.cbor.zst "$AXE_EDITION_ROOT/store/"
cp -R store/trusted "$AXE_EDITION_ROOT/store/"
```

Create `edition.json` and provision SSH identities before building. `axe --version` and `doctor --json` report the selected edition. Changing an embedded input requires rebuilding and redistributing `axe`.

## Production identities

Keep `keys/` ignored. Use separate identities for the SSH host, user CA, Store signer, Store publisher, relay, and Nix builders.

Create the SSH host identity once:

```bash
mkdir -p keys/ssh
umask 077
ssh-keygen -q -t ed25519 -N '' -C axe-host -f keys/ssh/host_ed25519
rm -f keys/ssh/host_ed25519.pub
```

Put only public user CA keys in `keys/ssh/user_ca_keys`. A login is accepted only when the username is allowlisted in `edition.json`, matches a certificate principal, and the certificate is signed by one of these CAs. Plain public keys and certificates with critical options are rejected. Runtime `--principals` or `AXE_SSHD_PRINCIPALS` replaces the compiled allowlist.

For a new Store, create the signing identity on the publisher machine:

```bash
cargo run --quiet -p axe-store -- keys generate --output keys/store
chmod 0600 keys/store/signing.key
```

Commit `store/trusted/<key-id>.pub`; keep `keys/store/signing.key` private. Rotate Store trust in this order: distribute a binary containing the new public key, switch the publisher to the matching private key, then remove the old key after all consumers have migrated.

## Store IAM

Consumers need anonymous object reads, not bucket listing or configuration access. The publisher needs bucket-scoped permission to read objects, upload objects, and conditionally update the Index. Keep bucket administration under a separate identity.

For Yandex Object Storage, create the bucket and publisher account only after selecting the intended cloud and folder:

```bash
yc init
yc storage bucket create --name your-axe-store-bucket \
  --default-storage-class standard --public-read
yc iam service-account create --name axe-store-publisher \
  --description "Publishes signed AXE Store snapshots"
yc iam access-key create --service-account-name axe-store-publisher \
  --description "axe-store publisher"
```

Verify the actual bucket policy, role scope, anonymous object fetch, and conditional writes. Write the access key without trailing whitespace to:

```text
keys/store/s3_access_key_id
keys/store/s3_secret_access_key
```

The publication recipes mount these files; they do not inherit arbitrary host AWS credentials. Configure the exact endpoint, region, bucket, prefixes, and pinned A/AAAA addresses in `config/store.json`. See [docs/store.md](docs/store.md) for package checks and [docs/release.md](docs/release.md) for publication commands and release verification.

## Relay endpoints and identities

The OSS edition has no default relay. A deployment can configure endpoints in `edition.json`:

```json
"relay": {
  "enabled_by_default": true,
  "tcp_endpoint": "relay.example.net:6999",
  "quic_endpoint": "relay.example.net:11000"
}
```

The default transport is TCP, so `enabled_by_default` requires a TCP endpoint and a token of at least 32 bytes. Generate deployment-specific credentials on a controlled machine:

```bash
mkdir -p keys/relay
umask 077
cargo run --quiet -p axe-store -- keys relay-token --output keys/relay/token
cargo run --quiet -p axe-store -- keys relay-identities --output keys/relay
```

`quic_server_key.pem` stays on the relay host. The server certificate and client identity are embedded in `axe`; the standalone `axe-relay` reads the server key and certificates from runtime files.

Build the artifacts:

```bash
just build-linux-amd64
just build-relay-linux-amd64
```

The operator installs verified artifacts and credentials. A typical relay command is:

```bash
AXE_RELAY_TOKEN="$(cat /etc/axe/relay-token)" \
axe-relay \
  --tcp-control '[::]:6999' \
  --quic-control '[::]:11000' \
  --quic-key /etc/axe/quic_server_key.pem \
  --quic-server-cert /etc/axe/quic_server_cert.pem \
  --quic-client-cert /etc/axe/quic_client_cert.pem \
  --public-bind 0.0.0.0 \
  --public-host relay.example.net \
  --min-port 3000 --max-port 4000 \
  --http 127.0.0.1:7000
```

Allow TCP `6999`, UDP `11000`, and the assigned TCP range `3000-4000`. Keep the HTTP monitor on loopback or behind an authenticated HTTPS reverse proxy.

On a target behind NAT, the operator starts `axe sshd` with an explicit stable relay ID:

```bash
axe --applet sshd -- \
  --listen 127.0.0.1:6969 \
  --workdir /var/lib/axe \
  --relay-transport quic \
  --relay relay.example.net:11000 \
  --relay-id host-01 --daemon
```

Wait through the authenticated monitor API and use the returned address:

```bash
axe-relay --api https://ops.example.net \
  wait --client-id host-01 --transport quic --timeout 60 --json
```

A registration is not proof of SSH login. Verify certificate authentication, command execution, SFTP, forwarding, and plain-key rejection. `just relay-live-smoke` covers local TCP and QUIC protocol paths; operators install and activate AXE on remote machines.

Rotate each relay identity on both peers: replace the server certificate and key together, replace the client certificate and key together, and update the TCP token on relay and targets. Rebuild targets when changing embedded credentials.

## Remote builders

Ordinary Store recipes ignore `nix/builders.conf`. Only `just store-build-remote` and `just store-sync-remote` opt in.

Create a dedicated identity:

```bash
mkdir -p keys/nix
ssh-keygen -q -t ed25519 -N '' -C axe-store-nix-builder \
  -f keys/nix/id_ed25519
```

Install its public half on each builder with one restricted command matching that host:

```text
command="/run/current-system/sw/bin/nix-daemon --stdio",restrict <public-key> axe-store-nix-builder
command="/nix/var/nix/profiles/default/bin/nix-daemon --stdio",restrict <public-key> axe-store-nix-builder
```

Create ignored `nix/builders.conf` with real systems, capacities, and only supported features:

```conf
builders = ssh-ng://builder@linux-builder.example.net x86_64-linux - 8 1 kvm,big-parallel,nixos-test ; ssh-ng://builder@darwin-builder.example.net aarch64-darwin - 2 1 benchmark,big-parallel
builders-use-substitutes = true
```

Generate and independently verify pinned host keys:

```bash
just keyscan-nix-builders
ssh-keygen -lf keys/nix/known_hosts
```

Remote recipes require `nix/builders.conf`, `keys/nix/id_ed25519`, and `keys/nix/known_hosts`. Builder operators apply host configuration and restart their Nix daemon; AXE recipes do not deploy or restart remote machines.
