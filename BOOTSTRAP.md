# Bootstrapping AXE

Use this guide to provision edition inputs, production identities, Store access, relays, and optional remote builders. For local builds see [README.md](README.md#building-from-source); for Store use and publication see [docs/store.md](docs/store.md) and [docs/release.md](docs/release.md).

## Development setup

Run builds and checks in the development shell:

```bash
nix develop .#default
just generate-dev-keys
```

`generate-dev-keys` creates missing development identities without replacing complete key pairs. It requires both a Store signing key and public trust, or neither; consumer editions that reuse an existing Store should follow [docs/editions.md](docs/editions.md) instead. Do not use development identities in production.

## Edition root

`AXE_EDITION_ROOT` selects the edition directory; it defaults to the checkout. Missing inputs are not borrowed from the public checkout.

Provide `edition.json`, `config/store.json`, `store/trusted/*.pub`, `keys/ssh/host_ed25519`, and public CA keys in `keys/ssh/user_ca_keys`. Keep the generated inventory in `store/bootstrap.json`. A signed `store/bootstrap-index.cbor.zst` is optional for development builds but required for OSS releases; when present, it must match the trusted keys and inventory.

For TCP relay credentials, provide `keys/relay/token`; for QUIC, provide `keys/relay/quic_server_cert.pem`, `quic_client_cert.pem`, and `quic_client_key.pem`. Additional HTTPS roots go in `store/nix/assets/trusted_ca.pem`. Store signing keys, S3 credentials, and the relay server private key are not embedded.

See [docs/editions.md](docs/editions.md) for an `edition.json` example and a build using OSS Store trust. Changing embedded configuration, trust, or identities requires rebuilding and redistributing `axe`.

## Production identities

Keep `keys/` ignored. Use separate identities for the SSH host, user CA, Store signer, Store publisher, relay, and Nix builders.

Keep only public user CA keys in `keys/ssh/user_ca_keys`. SSH login requires an allowlisted username matching a certificate principal signed by one of these CAs; plain public keys and certificates with critical options are rejected. `--principals` or `AXE_SSHD_PRINCIPALS` replaces the compiled allowlist at runtime.

For a new Store, create the signing identity on the publisher machine:

```bash
cargo run --quiet -p axe-store -- keys generate --output keys/store --trusted-output store/trusted
chmod 0600 keys/store/signing.key
```

These paths are relative to the current directory; use the intended edition's paths when provisioning another root. Key generation refuses to overwrite existing keys, so use a fresh trust directory for a new Store. Commit `store/trusted/<key-id>.pub`; keep `keys/store/signing.key` private. Rotate Store trust in this order: distribute a binary containing the new public key, switch the publisher to the matching private key, then remove the old key after all consumers have migrated.

## Store IAM

Consumers need anonymous object reads, not bucket listing or configuration access. The publisher needs bucket-scoped permission to read objects, upload objects, and conditionally update the Index. Keep bucket administration under a separate identity.

For Yandex Object Storage, select the intended cloud and folder before creating the bucket and a dedicated publisher service account. Verify bucket policy, role scope, anonymous object reads, and conditional writes.

Store the publisher access key without trailing whitespace in:

```text
keys/store/s3_access_key_id
keys/store/s3_secret_access_key
```

Container Store recipes mount these files rather than inheriting host AWS credentials. The local AXE release publisher can use AWS environment variables instead; see [docs/release.md](docs/release.md#inputs-and-trust). Set the endpoint, region, bucket, prefixes, and pinned A/AAAA addresses in `config/store.json`. See [Store publication](docs/store.md#publication) for commands and safeguards.

## Relay endpoints and identities

The OSS edition has no default relay. Configure endpoints in `edition.json` if needed. The default transport is TCP; `enabled_by_default` requires a TCP endpoint and a token of at least 32 bytes. Generate deployment-specific credentials on a controlled machine:

```bash
mkdir -p keys/relay
umask 077
cargo run --quiet -p axe-store -- keys relay-token --output keys/relay/token
cargo run --quiet -p axe-store -- keys relay-identities --output keys/relay
```

Keep `quic_server_key.pem` on the relay host. The server certificate and client identity are embedded in `axe`; `axe-relay` reads the server key and certificates from runtime files.

Build `axe` with `just build-linux-amd64` and the standalone relay with `just build-relay-linux-amd64`. Install verified artifacts and credentials, configure listeners and a stable target ID, then check the monitor through a protected endpoint. Keep its unauthenticated HTTP listener on loopback or behind an authenticated HTTPS reverse proxy.

Registration alone does not prove SSH login. Verify certificate authentication, command execution, SFTP, forwarding, and plain-key rejection. With matching binaries on `PATH`, `bash scripts/relay-live-smoke.sh axe axe-relay` checks local TCP and QUIC paths, not remote deployment.

Rotate each relay identity on both peers: replace the server certificate and key together, replace the client certificate and key together, and update the TCP token on relay and targets. Rebuild targets when changing embedded credentials.

## Remote builders

Store recipes use local builders by default. `just store-build-remote` and `just store-sync-remote` opt in to `nix/builders.conf`; setting `AXE_STORE_REMOTE=1` also enables it.

Use a dedicated SSH identity in `keys/nix/id_ed25519`. Install its public key on each builder with a restricted `nix-daemon --stdio` command appropriate for that host. Put real systems, capacities, and supported features in ignored `nix/builders.conf`.

Generate and independently verify pinned host keys:

```bash
just keyscan-nix-builders
ssh-keygen -lf keys/nix/known_hosts
```

Remote recipes require `nix/builders.conf`, `keys/nix/id_ed25519`, and `keys/nix/known_hosts`. Builder operators apply host configuration and restart their Nix daemon; AXE recipes do not deploy or restart remote machines.
