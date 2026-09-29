# Bootstrapping AXE

Provision edition inputs, production identities, Store access, and optional relay and remote builders here. For local builds see [README.md](README.md#building-from-source); for Store behavior and publication see [docs/store.md](docs/store.md) and [docs/release.md](docs/release.md).

## Development setup

Run builds and checks in the development shell:

```bash
nix develop .#default
just generate-dev-keys
```

`generate-dev-keys` creates missing development identities without replacing complete key pairs. Do not use them as production trust material.

## Edition root

`AXE_EDITION_ROOT` selects one complete build-input bundle. Missing files are not borrowed from the public checkout.

Set `AXE_EDITION_ROOT` to the edition directory. It must contain `edition.json`, `config/store.json`, `store/trusted/*.pub`, and the generated `store/bootstrap.json`. Include `store/bootstrap-index.cbor.zst` when shipping a signed snapshot. Provision `keys/ssh/host_ed25519` and public CA keys in `keys/ssh/user_ca_keys`.

For a configured TCP relay, provide `keys/relay/token`; for QUIC, provide `keys/relay/quic_server_cert.pem`, `quic_client_cert.pem`, and `quic_client_key.pem`. Additional HTTPS roots go in `store/nix/assets/trusted_ca.pem`. The build validates these inputs. Publisher keys, S3 credentials, and the relay server private key are not embedded.

Create `edition.json` with an edition ID, SSH principals, relay settings, and aliases. To reuse the OSS Store consumer inputs, copy `config/store.json`, `store/bootstrap.json`, `store/bootstrap-index.cbor.zst`, and `store/trusted/` into the corresponding paths under the edition root. Changing embedded inputs requires rebuilding and redistributing `axe`.

## Production identities

Keep `keys/` ignored. Use separate identities for the SSH host, user CA, Store signer, Store publisher, relay, and Nix builders.

Keep only public user CA keys in `keys/ssh/user_ca_keys`. SSH login requires an allowlisted username matching a certificate principal signed by one of these CAs; plain public keys and certificates with critical options are rejected. `--principals` or `AXE_SSHD_PRINCIPALS` replaces the compiled allowlist at runtime.

For a new Store, create the signing identity on the publisher machine:

```bash
cargo run --quiet -p axe-store -- keys generate --output keys/store
chmod 0600 keys/store/signing.key
```

Commit `store/trusted/<key-id>.pub`; keep `keys/store/signing.key` private. Rotate Store trust in this order: distribute a binary containing the new public key, switch the publisher to the matching private key, then remove the old key after all consumers have migrated.

## Store IAM

Consumers need anonymous object reads, not bucket listing or configuration access. The publisher needs bucket-scoped permission to read objects, upload objects, and conditionally update the Index. Keep bucket administration under a separate identity.

For Yandex Object Storage, select the intended cloud and folder before creating the bucket and a dedicated publisher service account. Verify bucket policy, role scope, anonymous object reads, and conditional writes.

Store the publisher access key without trailing whitespace in:

```text
keys/store/s3_access_key_id
keys/store/s3_secret_access_key
```

Publication recipes mount these files rather than inheriting host AWS credentials. Set the endpoint, region, bucket, prefixes, and pinned A/AAAA addresses in `config/store.json`. Publication commands and release checks are in [docs/release.md](docs/release.md).

## Relay endpoints and identities

The OSS edition has no default relay. Configure endpoints in `edition.json` if needed. The default transport is TCP; `enabled_by_default` requires a TCP endpoint and a token of at least 32 bytes. Generate deployment-specific credentials on a controlled machine:

```bash
mkdir -p keys/relay
umask 077
cargo run --quiet -p axe-store -- keys relay-token --output keys/relay/token
cargo run --quiet -p axe-store -- keys relay-identities --output keys/relay
```

Keep `quic_server_key.pem` on the relay host. The server certificate and client identity are embedded in `axe`; `axe-relay` reads the server key and certificates from runtime files.

Build `axe` with `just build-linux-amd64` and the standalone relay with `just build-relay-linux-amd64`. Operators install verified artifacts and credentials, configure relay listeners and the target's stable relay ID, then check the authenticated monitor API. Keep the HTTP monitor on loopback or behind an authenticated HTTPS reverse proxy.

Registration alone does not prove SSH login. Verify certificate authentication, command execution, SFTP, forwarding, and plain-key rejection. `just relay-live-smoke` exercises local TCP and QUIC protocol paths, not remote deployment.

Rotate each relay identity on both peers: replace the server certificate and key together, replace the client certificate and key together, and update the TCP token on relay and targets. Rebuild targets when changing embedded credentials.

## Remote builders

Ordinary Store recipes ignore `nix/builders.conf`. Only `just store-build-remote` and `just store-sync-remote` opt in.

Use a dedicated SSH identity in `keys/nix/id_ed25519`. Install its public key on each builder with a restricted `nix-daemon --stdio` command appropriate for that host. Put real systems, capacities, and supported features in ignored `nix/builders.conf`.

Generate and independently verify pinned host keys:

```bash
just keyscan-nix-builders
ssh-keygen -lf keys/nix/known_hosts
```

Remote recipes require `nix/builders.conf`, `keys/nix/id_ed25519`, and `keys/nix/known_hosts`. Builder operators apply host configuration and restart their Nix daemon; AXE recipes do not deploy or restart remote machines.
