# Bootstrapping AXE

This guide covers building AXE with your own identities, publishing an AXE Store, and configuring remote builders. For a local build without production identities, start with [Building from source](README.md#building-from-source). Release metadata is initially empty, so build from this checkout until the first edition-specific publication.

## Choose your path

| Goal | Read first |
| --- | --- |
| Run AXE locally | [Building from source](README.md#building-from-source) |
| Develop Rust code | Development shell and development keys |
| Add an on-demand AXE Store package | Development shell, development keys, and package checks |
| Build your edition using an existing AXE Store | [Use an existing AXE Store](#use-an-existing-axe-store) |
| Run an SSH relay | Relay endpoints and identities |
| Publish your own AXE Store and AXE releases | Bucket, publisher, signing key, package build, Store sync, and release publication |
| Accelerate AXE Store builds | Remote builders (optional) |

## Files that stay local

`keys/` and `nix/builders.conf` are Git-ignored. Store verification keys in `store/trusted/`, the signed bootstrap snapshot, and the Store consumer configuration are public tracked inputs. Use separate publisher, AXE runtime, SSH CA, relay, and builder identities. The “Embedded in AXE” column applies to production builds with the corresponding endpoint configured; it does not mean private files should be published.

| Path | Purpose | Embedded in `axe`? |
| --- | --- | --- |
| `keys/ssh/host_ed25519` | Embedded SSH server private host key | Yes |
| `keys/ssh/user_ca_keys` | Trusted OpenSSH user CA public keys, one per line | Yes |
| `keys/ssh/dev_user_ca` | Private CA for local development only | No |
| `keys/store/signing.key` | Private Ed25519 key for signing AXE Store metadata | No |
| `store/trusted/*.pub` | AXE Store public verification keys (tracked) | Yes |
| `keys/store/s3_access_key_id` | Publisher S3 access key ID | No |
| `keys/store/s3_secret_access_key` | Publisher S3 secret access key | No |
| `keys/relay/token` | Shared secret for the TCP relay and `sshd` | Only with a configured TCP endpoint |
| `keys/relay/quic_server_cert.pem` | Pinned public relay certificate | Only with a configured QUIC endpoint |
| `keys/relay/quic_server_key.pem` | Relay private key | No |
| `keys/relay/quic_client_cert.pem` | Public `sshd` certificate for the relay | Only with a configured QUIC endpoint |
| `keys/relay/quic_client_key.pem` | Private `sshd` identity | Only with a configured QUIC endpoint |
| `keys/nix/id_ed25519` | Private identity for AXE Store remote builders | No |
| `keys/nix/known_hosts` | Pinned SSH host keys for builders | No |

Keep the production user CA private key outside the checkout and build artifacts. Only the publisher needs the private Store signing key. AXE embeds public trust material and, after a Store sync, a signed bootstrap Index; it does not embed the signing key or S3 credentials. The SSH host key and configured relay client credentials are embedded, so protect release binaries accordingly.

## Edition root

This checkout contains the OSS edition and its `edition.json` (`id: oss`). The build reads `config/store.json`, `config/sshd.json`, `config/relay.json`, `keys/`, `store/trusted/`, `store/bootstrap.json`, and, when present, the signed `store/bootstrap-index.cbor.zst` from one edition root. Release metadata (`nix/axe-releases.json`) belongs to the publishing/Nix package layer, not the executable build. Sources, `config/aliases.json`, and the shared package API remain in the public workspace.

Set only `AXE_EDITION_ROOT` to select a separate edition for an AXE build:

```bash
export AXE_EDITION_ROOT=/path/to/your-edition
```

The Store publisher and release workflows have additional output, flake, and inventory settings in the sections below. They are not needed to build a consumer of an existing Store. Missing required edition files are not borrowed from the public checkout: an incomplete bundle fails with the missing path. `axe --version` and `doctor --json` identify the selected edition. The root flake exports `lib.axeStore.mkPackageSet`; an external flake can supply `additionalCaBundle` and `extraCategories` while sharing build and Store code. This checkout's GitHub release workflow is OSS-only.

Unless noted otherwise, the commands below assume the OSS checkout is the edition root. When using an external edition, inspect and adjust the paths, config, target bucket, and expected edition ID before publishing.

## Use an existing AXE Store

To build a development edition against the OSS Store immediately after checkout, copy only its tracked consumer inputs. Do not copy `keys/`: that directory contains local identities and publisher credentials.

```bash
nix develop .#default
export AXE_EDITION_ROOT="$HOME/my-axe"
export AXE_RELEASE_DIR="$AXE_EDITION_ROOT/dist"
umask 077
mkdir "$AXE_EDITION_ROOT"
mkdir -p "$AXE_EDITION_ROOT/config" "$AXE_EDITION_ROOT/store"
cp config/{store,sshd,relay}.json "$AXE_EDITION_ROOT/config/"
cp store/bootstrap.json store/bootstrap-index.cbor.zst "$AXE_EDITION_ROOT/store/"
cp -R store/trusted "$AXE_EDITION_ROOT/store/"
printf '%s\n' '{"schema_version":1,"id":"my-axe"}' > "$AXE_EDITION_ROOT/edition.json"
just build-linux-amd64
"$AXE_RELEASE_DIR/axe-x86_64-unknown-linux-musl" --version
AXE_STORE_MODE=cache-only "$AXE_RELEASE_DIR/axe-x86_64-unknown-linux-musl" commands
```

The version must say `edition my-axe`; the last command reads the signed built-in inventory without contacting the Store. `just build-linux-amd64` creates missing **development** SSH and relay identities in your edition root. Before distributing this binary, replace them with your own SSH host key and public user CA in `keys/ssh/`, set your principals in `config/sshd.json`, configure relay credentials if needed, and rebuild. Do not ship the automatically generated development identities.

The copied Store URL, pinned IP addresses, trusted keys, bootstrap inventory, and signed snapshot must describe the **same** Store. Public trust keys authenticate Store metadata; the publisher's private signing key and S3 credentials are not needed to build or run this edition. For another Store, get those public inputs from its owner instead of copying the OSS files. If you omit the signed snapshot, AXE can fetch and verify the published Index at runtime with network access, but it has no built-in Store inventory when offline; the release publisher currently requires a snapshot. Neither the build nor the runtime downloads a replacement snapshot file.

For other targets use `just build-linux-arm64` or `just build-darwin-arm64`. You do not need `AXE_STORE_FLAKE` or `AXE_STORE_OUTPUT_DIR` unless you are publishing your own Store.

## Development shell and local keys

Enter the development shell and create keys when setting up a **new development Store**:

```bash
nix develop .#default
just generate-dev-keys
```

Use `generate-dev-keys` for a new Store with no trusted keys yet, not for the checked-in OSS Store: a fresh checkout has public trust but no matching private signing key, and the recipe correctly refuses to invent one. A consumer edition needs only the public keys and can run `just build` without this recipe. The generator preserves complete key pairs; it does not rotate them. Never distribute generated development private keys or use them as production trust material. For production, provision identities independently and verify the trust set before building.

AXE Store recipes run a container via Podman by default. To use Docker explicitly:

```bash
CONTAINER_RUNTIME=docker just store-build
```

Install Podman or Docker on the host; the development shell does not provide the container runtime.

## Production configuration

Changing any embedded input in the table above requires rebuilding and redistributing AXE.

### SSH principals and host identity

The allowlist comes from `config/sshd.json`:

```json
{
  "principals": ["alice", "bob"]
}
```

At login, the username must be on this list and must match a principal in the OpenSSH user certificate. Ordinary public-key authentication and certificates with critical options are rejected.

Create a production host identity once and keep it safe. These commands create a new key; they do not rotate an existing one:

```bash
mkdir -p keys/ssh
ssh-keygen -q -t ed25519 -N '' -C axe-host -f keys/ssh/host_ed25519
rm -f keys/ssh/host_ed25519.pub
chmod 0600 keys/ssh/host_ed25519
```

The OpenSSH private-key file already contains the public component, so a separate `.pub` file is unnecessary. Extract it without writing another file:

```bash
ssh-keygen -y -f keys/ssh/host_ed25519
```

Put trusted **public user CA** keys in `keys/ssh/user_ca_keys`, one per line. Do not put ordinary user public keys there or place the private production CA in the checkout.

For example, a CA operator can issue a short-lived certificate for `alice` after substituting the actual secure CA path and user key:

```bash
ssh-keygen -s /secure/path/to/user_ca \
  -I alice@axe -n alice -V +8h ~/.ssh/id_ed25519.pub
```

At runtime, `--principals alice,bob` or `AXE_SSHD_PRINCIPALS=alice,bob` can replace the entire allowlist. The CLI flag takes precedence over the environment variable.

### AXE Store in SSH sessions

`AXE_STORE_MODE=auto|cache-only|off` sets an upper bound on Store access. `sshd --store-mode MODE` can only tighten that bound for child shell/exec sessions. In `cache-only`, HTTP fetches are disallowed but verified cached packages remain available. In `off`, Store commands are not registered, the derived `.axe-store` is not created, and Store names do not enter the PATH bridge. A CLI flag cannot loosen the environment limit.

To disable the Store entirely:

```bash
AXE_STORE_MODE=off axe sshd --daemon
```

For temporary network loss, leave the mode at `auto`: a transient failure triggers a short persistent retry backoff before AXE tries the Store again. Do not switch a deployment to `off` because of one failed network probe.

### Relay endpoints and identities

QUIC uses two independent Ed25519 identities:

| File | Purpose | Location |
| --- | --- | --- |
| `keys/relay/quic_server_cert.pem` | Pinned public relay certificate | Embedded in `axe` if configured; runtime file for `axe-relay` |
| `keys/relay/quic_server_key.pem` | Relay private identity | Relay host only; never embedded |
| `keys/relay/quic_client_cert.pem` | Public certificate identifying `sshd` to the relay | Embedded in `axe` if configured; runtime file for `axe-relay` |
| `keys/relay/quic_client_key.pem` | Private `sshd` identity | Embedded in `axe` |

`relay-id` identifies a registration in its address and logs; mTLS authenticates it. The checked-in `config/relay.json` disables the relay by default and sets both endpoints to `null`. The example below opts in with illustrative DNS names; it is not the checkout's default configuration.

#### 1. Configure endpoints and generate identities

For a deployment that uses TCP by default and permits QUIC, set `config/relay.json` to values appropriate for your relay:

```json
{
  "enabled_by_default": true,
  "tcp_endpoint": "relay.example.net:6999",
  "quic_endpoint": "relay.example.net:11000"
}
```

An enabled default requires a TCP endpoint and an adequate TCP token even when a session uses QUIC. In the development shell, generate deployment-specific credentials on a controlled machine. Do not reuse development keys:

```bash
mkdir -p keys/relay
umask 077
cargo run --quiet -p axe-store -- keys relay-token \
  --output keys/relay/token
cargo run --quiet -p axe-store -- keys relay-identities \
  --output keys/relay
```

The generator creates a server certificate with `serverAuth` and a client certificate with `clientAuth`. The relay server private key is not read by `crates/axe/build.rs` or embedded. The standalone `axe-relay` server reads its private key and both certificates from runtime files. Keep its client certificate in sync with the certificates issued to `sshd`.

If `enabled_by_default` is `false` and both endpoints are `null`, relay credentials are not embedded and a normal `sshd` launch does not require them. An explicit `--relay` still requires transport credentials before binding: TCP reads `AXE_RELAY_TOKEN`; QUIC uses `AXE_RELAY_QUIC_SERVER_CERT_FILE`, `AXE_RELAY_QUIC_CLIENT_CERT_FILE`, and `AXE_RELAY_QUIC_CLIENT_KEY_FILE` if credentials were not embedded. `--no-relay` disables relay and conflicts with `--relay`.

#### 2. Build and place the artifacts

```bash
just build-linux-amd64
just build-relay-linux-amd64
```

Install verified `axe` on target hosts and a separate verified `axe-relay` binary on the relay host. Place `quic_server_key.pem`, `quic_server_cert.pem`, and `quic_client_cert.pem` on the relay host with restricted permissions. Targets need no additional QUIC files when configured QUIC credentials are embedded. The operator runs the following installation on the intended relay host:

```bash
sudo install -D -m 0755 dist/axe-relay-x86_64-unknown-linux-musl /usr/local/bin/axe-relay
sudo install -D -m 0600 keys/relay/quic_server_key.pem /etc/axe/quic_server_key.pem
sudo install -D -m 0644 keys/relay/quic_server_cert.pem /etc/axe/quic_server_cert.pem
sudo install -D -m 0644 keys/relay/quic_client_cert.pem /etc/axe/quic_client_cert.pem
```

#### 3. Start the relay

Provide the TCP token in a root-only environment file. The following command must be run from the directory with that deployment's `keys/relay/token`:

```bash
{ printf 'AXE_RELAY_TOKEN='; cat keys/relay/token; printf '\n'; } |
  sudo tee /etc/axe/relay.env >/dev/null
sudo chmod 0600 /etc/axe/relay.env
```

Save this example unit as `/etc/systemd/system/axe-relay.service`. `--public-bind` selects the local listener interface; `--public-host` names the externally reachable DNS name or IP advertised to SSH clients. Behind NAT, forward the allocated TCP range to the bind address. Adjust endpoints, firewall, and DNS for the deployment:

```systemd
[Unit]
Description=AXE relay
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
EnvironmentFile=/etc/axe/relay.env
LoadCredential=quic-key:/etc/axe/quic_server_key.pem
LoadCredential=quic-server-cert:/etc/axe/quic_server_cert.pem
LoadCredential=quic-client-cert:/etc/axe/quic_client_cert.pem
ExecStart=/usr/local/bin/axe-relay --tcp-control [::]:6999 --quic-control [::]:11000 --quic-key ${CREDENTIALS_DIRECTORY}/quic-key --quic-server-cert ${CREDENTIALS_DIRECTORY}/quic-server-cert --quic-client-cert ${CREDENTIALS_DIRECTORY}/quic-client-cert --public-bind 0.0.0.0 --public-host relay.example.net --min-port 3000 --max-port 4000 --http 127.0.0.1:7000
Restart=on-failure
RestartSec=5s
DynamicUser=yes
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

[Install]
WantedBy=multi-user.target
```

Allow the following ports in the deployment's firewall:

- TCP `6999` for TCP+yamux control;
- UDP `11000` for QUIC control;
- TCP `3000-4000` for assigned public ports.

`axe-relay` serves its read-only dashboard at `http://127.0.0.1:7000/` and `GET /api/v1/status` on loopback only. For remote operators and agents, use an authenticated HTTPS reverse proxy; do not expose or forward the local listener without access control. The proxy handles TLS and authentication. Locally, inspect with `axe-relay status`, `axe-relay clients --json`, or a browser. From an agent workstation, use `axe-relay --api https://ops.example.net status --json`; if the proxy accepts a bearer token, set `AXE_RELAY_API_TOKEN` in the CLI environment. The proxy hostname can differ from the public SSH address advertised by `--public-host`.

The operator then activates and inspects the unit on that host:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now axe-relay.service
sudo journalctl -fu axe-relay.service
```

#### 4. Connect a target

The operator installs a verified `axe` artifact on a compatible target behind NAT, prepares a working directory, and starts `sshd` using QUIC transport. SSH sessions inherit the OS privileges of the `sshd` process, so choose its service account accordingly. The target connects outbound to the relay and needs no inbound SSH firewall rule. If the target's edition has a configured default relay endpoint, omit `--relay` and `--relay-transport` unless changing transport. OSS has no default endpoint, so this example selects one explicitly. Verify artifact provenance and SHA-256 against the [README download table](README.md#downloads) before delivery.

```bash
sudo install -D -m 0755 dist/axe-x86_64-unknown-linux-musl /usr/local/bin/axe
sudo install -d -m 0700 /var/lib/axe
sudo /usr/local/bin/axe --applet sshd -- \
  --listen 127.0.0.1:6969 \
  --workdir /var/lib/axe \
  --relay-transport quic \
  --relay relay.example.net:11000 \
  --relay-id host-01 --daemon
```

Set `--relay-id` to a value unique among clients of the same relay so that `wait --client-id` identifies one target; an explicit ID stays stable across `sshd` restarts. Without it, the ID is `<pidns>@<user>@<hostname>`, where `<pidns>` is the PID-namespace inode. Processes in one container report the same value across `sshd` restarts, but namespace inodes can coincide across hosts, and hosts without visible procfs share the `-1` fallback. Discover the actual ID with unfiltered `watch --json`; do not assume it was derived from a previous process.

While target `sshd` runs, wait from the agent workstation through the authenticated HTTPS API. The command returns the **assigned, externally reachable** `public_address`; do not guess the port:

```bash
axe-relay --api https://ops.example.net wait --client-id host-01 --transport quic --timeout 60 --json
```

To observe all clients, run `axe-relay --api https://ops.example.net watch --json` before starting the target; `--client-id host-01` filters the feed. Each JSON line reports `present` (already active), `connected`, or `disconnected` with the client's assigned address. To wait for a new registration, ignore `present` and use the next `connected` record. If the bounded event history is lost or the relay restarts, `watch` exits with an error rather than silently skipping events; restart it and inspect `clients`.

Connect to the returned host and port with an OpenSSH client that has an authorized user certificate. A registration does not prove forwarding, and an SSH banner does not prove login. For repeatable local-only verification of launch, wait, and SSH banner over both transports, run `just relay-live-smoke` in the development shell. The operator handles remote installation and activation.

#### 5. Rotate identities

- Server identity: replace `quic_server_cert.pem` and `quic_server_key.pem` on the relay host, and update the pinned server certificate in `axe` build inputs (rebuild/reinstall affected targets) or in each target's runtime certificate path.
- Client identity: replace `quic_client_cert.pem` on the relay host and `quic_client_cert.pem`/`quic_client_key.pem` in target build inputs (rebuild/reinstall affected targets) or their runtime files. The standalone `axe-relay` binary does not embed these identities.
- TCP token: update the relay host's `AXE_RELAY_TOKEN` and each target's runtime token or rebuild targets that embed it.

Coordinate each cutover: each peer trusts one pinned certificate. Registration and disconnect events record transport, `relay-id`, peer, public address, duration, and `active_clients`.

## AXE Store from scratch

AXE Store publishes on-demand packages. Consumers read objects without S3 credentials, but accept packages only after checking the signed Index and manifest, payload size, and complete SHA-256 digest. The default OSS `config/store.json` names the `axe-store` bucket, `store` package prefix, and `axe` release prefix. Those settings do not establish that the bucket, publisher credentials, or releases exist. Confirm bucket ownership and the intended publishing destination first.

### 1. Create a bucket

The following uses [Yandex Object Storage](https://yandex.cloud/en/docs/storage/). Initialize `yc` with your own account and select the intended cloud and folder:

```bash
yc init
```

Create a dedicated bucket only if one has not already been provisioned. Replace the illustrative name with your approved bucket, set that exact name in `config/store.json`, and check current provider flags and permissions before running:

```bash
yc storage bucket create \
  --name your-axe-store-bucket \
  --default-storage-class standard \
  --public-read
```

Consumers need anonymous object reads, not bucket listing or public bucket configuration. Keep listing and configuration private. After publishing, verify the policy with an unauthenticated object request; the bucket-create flag alone does not prove that clients can fetch the intended Index and release objects.

### 2. Provision publisher credentials

Use a publisher service account separate from the identity that administers bucket creation. This command creates the account but does not grant object permissions:

```bash
yc iam service-account create \
  --name axe-store-publisher \
  --description "Publishes signed AXE Store snapshots"
yc iam service-account list
```

Following current provider guidance, grant the account only the bucket-scoped object permissions needed to read existing objects and upload or update published objects, including the conditional Index update. Verify the role, bucket binding scope, and conditional-write behavior in your environment. Neither a role name nor access key creation guarantees those permissions. Keep bucket administration under a separate identity.

After the authorization has been verified, create a static access key for this service account:

```bash
yc iam access-key create \
  --service-account-name axe-store-publisher \
  --description "axe-store publisher"
```

Write the resulting access key ID and secret, without trailing whitespace, to these edition-local files:

```text
keys/store/s3_access_key_id
keys/store/s3_secret_access_key
```

Keep both credentials on the publisher machine, restrict their permissions, and never include them in the release binary. The documented `just` publication recipes mount `keys/store` into the container; they do not forward arbitrary host AWS credential environment variables. Use file-based credentials for these recipes. The publisher reads its private signing key from the same edition-local directory. Generating a key does not verify bucket authorization.

### 3. Set the endpoint and consumer addresses

The OSS checkout already has these values under `storage` in `config/store.json`. Change them only if your endpoint, bucket, or prefixes differ:

```json
{
  "storage": {
    "endpoint": "https://storage.yandexcloud.net",
    "region": "ru-central1",
    "bucket": "axe-store",
    "prefix": "store",
    "release_prefix": "axe"
  }
}
```

This fragment does not replace the file; keep `consumer` settings intact. Consumer URLs are `<endpoint>/<bucket>/<prefix>/...`, and AXE connects only to addresses permitted by `consumer.addresses`. If the endpoint or CDN changes, update its URL and the approved A/AAAA addresses before rebuilding AXE. Inspect the current DNS responses, then independently verify which addresses to pin:

```bash
dig +short A storage.yandexcloud.net
dig +short AAAA storage.yandexcloud.net
```

### 4. Generate the AXE Store signing identity

For a **new Store** whose `store/trusted/` is empty, generate its private signing key and first public trust key in a controlled publisher environment. The OSS Store already has a tracked public key: its publisher must retain the matching private key; a new key cannot recreate it.

```bash
cargo run --quiet -p axe-store -- keys generate --output keys/store
chmod 0600 keys/store/signing.key
```

The generator creates:

```text
keys/store/signing.key
store/trusted/<key-id>.pub
```

The private key stays with the publisher. Commit the matching public keys in `store/trusted/` when the Store trust set changes. All public keys there are embedded in AXE. Do not use `just generate-dev-keys` as a substitute for provisioning a production identity. Rotate without stranding consumers:

1. Add the new public key to `store/trusted/`, rebuild AXE, and distribute that build.
2. Once consumers have the new trust key, switch the publisher to the matching private key.

Keep an old trusted key until no consumer needs metadata signed by it. Also keep the signed bootstrap Index aligned with the current trust set and package inventory; a release build verifies it.

### 5. Prepare package metadata

Package definitions live in `store/nix/packages/*.nix`. The generated bootstrap inventory contains package names, targets, channels, and artifact types:

```bash
just store-bootstrap
just check-store-bootstrap
nix flake check --no-build .
```

Do not hand-edit `store/bootstrap.json`; regenerate it after package changes. It is not the signed Index. `store/bootstrap-index.cbor.zst` is a separate signed snapshot copied from the Store sync output. A release build validates the snapshot against the embedded trust keys and bootstrap inventory. See [Adding a package to AXE Store](docs/store.md#adding-a-package) for the package definition entry point.

### 6. Build and check locally

Build the staged AXE Store tree in its persistent container volume:

```bash
just store-build
```

With default edition and output settings, the staged tree appears under `store/dist/`. Its staged Index is signed, but `store-build` alone neither publishes the Index nor installs the signed bootstrap snapshot for AXE releases. Before the first publication, exercise both smoke paths:

```bash
just store-smoke
just store-nix-smoke
```

`store-smoke` checks signing, deterministic rebuilds, directory publication, cache behavior, and consumer failure policy. `store-nix-smoke` builds real Nix outputs and checks that static Linux artifacts contain no `/nix/store` references. Run development smoke checks with isolated development identities. Build and publish the production snapshot with the intended edition's signing identity.

### 7. Publish the Store snapshot separately

Store publication uses the edition selected by `AXE_EDITION_ROOT` and its `config/store.json`. Check `edition.json`, bucket ownership, the signing identity, and the intended destination first. From the trusted local publisher, run:

```bash
just store-sync
```

`store-sync` regenerates `store/bootstrap.json`, builds and publishes signed Store objects and the Index, then copies the signed staged Index to the edition-local `store/bootstrap-index.cbor.zst`. Commit the updated public bootstrap inventory and snapshot together with public trust changes after reviewing the published Store; never commit `keys/`. It does not build or publish AXE binaries. The public Index can have different bytes because of its publication generation; the release build checks the snapshot signature and inventory.

`just store-build && just store-publish` publishes Store metadata without installing a bootstrap snapshot; do not substitute an old staged Index. `just store-diagnose-upload` tests temporary S3 uploads without publishing an Index. Even with `AXE_STORE_ALLOW_TARGET_REMOVAL=1 just store-sync`, review affected Store consumers explicitly.

For AXE binary and GitHub release publication, see [docs/release.md](docs/release.md). Store publication and website deployment remain separate operations.

## Website publication

The OSS website uses `web/`. Its command inventory, `web/data/registry.json`, is not tracked: the release publisher generates it from the staged x86_64 AXE binary and attaches it to the GitHub Release. The CI website build downloads `registry.json` from the latest GitHub Release; it does not generate development keys, build AXE, or publish Store releases.

`.github/workflows/deploy-web.yml` runs only on a push to `main`. It downloads the command inventory, builds with Hugo 0.166.0, and synchronizes `web/public/` to `s3://axe.buglloc.com/` with `--delete`. Set GitHub Actions secrets `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` for an identity with object write/delete access only to that website bucket. Optional secrets are `AWS_REGION` (default `ru-central1`) and `S3_ENDPOINT_URL` (default `https://storage.yandexcloud.net`). Do not use the AXE Store publisher identity for the website.

Check the build locally, without uploading. `just web-build` (or `just web-serve`) builds a development AXE and generates `web/data/registry.json` from it first:

```bash
nix develop .#default --command just web-build
```

## Remote builders

Remote builds are optional. Ordinary `just store-build` and `just store-sync` ignore `nix/builders.conf` even when it exists. Only these recipes opt in:

```bash
just store-build-remote
just store-sync-remote
```

### 1. Create a separate identity

```bash
mkdir -p keys/nix
ssh-keygen -q -t ed25519 -N '' \
  -C axe-store-nix-builder \
  -f keys/nix/id_ed25519
```

Do not reuse an administrator key or host deployment identity. Register the public half on each builder as a restricted Nix daemon key. The example `authorized_keys` entries are alternatives: install only one:

```text
command="/run/current-system/sw/bin/nix-daemon --stdio",restrict <builder-public-key> axe-store-nix-builder
command="/nix/var/nix/profiles/default/bin/nix-daemon --stdio",restrict <builder-public-key> axe-store-nix-builder
```

The first path is common on NixOS, the second on Darwin; use only a path that actually exists on your builder. The administrator of the builder performs any required host-side configuration and restart.

### 2. Describe builders

Create the ignored `nix/builders.conf` with entries matching your real hosts and capacities:

```conf
builders = ssh-ng://builder@linux-builder.example.net x86_64-linux - 8 1 kvm,big-parallel,nixos-test ; ssh-ng://builder@darwin-builder.example.net aarch64-darwin - 2 1 benchmark,big-parallel
builders-use-substitutes = true
```

After the system name, Nix expects `maxJobs`, `speedFactor`, and features. Declare only features the remote daemon supports. The builder must advertise the same features: the Darwin package graph uses LLVM derivations requiring `big-parallel`, so declaring it only on the client is insufficient. For example, on a nix-darwin host:

```nix
nix.settings.system-features = ["benchmark" "big-parallel"];
```

The builder operator must apply host changes and restart its remote Nix daemon manually. `builders-use-substitutes = true` lets builders fetch dependencies from their configured substituters. Built outputs return directly over SSH into the AXE Store container. Local builds take priority; remote builders handle overflow or targets that cannot be built locally.

### 3. Pin builder host keys

Generate `known_hosts` from configured hosts:

```bash
just keyscan-nix-builders
```

This is trust-on-first-use: network replies cannot independently verify builder identity. Before the first remote build, compare fingerprints with each builder through a separate trusted channel:

```bash
ssh-keygen -lf keys/nix/known_hosts
```

Remote recipes require all three edition-local files:

```text
nix/builders.conf
keys/nix/id_ed25519
keys/nix/known_hosts
```

The recipe passes `nix/builders.conf` via `NIX_USER_CONF_FILES`, mounts the private key at `/root/.ssh/id_ed25519`, and mounts the pinned host keys at `/etc/ssh/ssh_known_hosts`. Remote recipes fail if a file is missing or the configuration is incomplete. For a one-off host-side Nix command, load the same dedicated identity into your SSH agent:

```bash
ssh-add keys/nix/id_ed25519
```

Set `NIX_USER_CONF_FILES="$PWD/nix/builders.conf"` for that individual `nix build` invocation with a verified flake output. The container recipes do this themselves; do not set global remote-builder configuration to enable local recipes.

The package graph cross-builds `aarch64-linux` and CGO-free Go tools for `aarch64-darwin` on a Linux builder. It does not require a native AArch64 builder; other Darwin packages require a Darwin builder.

## Verify a release

A Linux release must be a static ELF of type `EXEC` without `INTERP` or `DT_NEEDED`:

```bash
just build-linux-amd64
file dist/axe-x86_64-unknown-linux-musl
readelf -hW dist/axe-x86_64-unknown-linux-musl
readelf -lW dist/axe-x86_64-unknown-linux-musl
readelf -dW dist/axe-x86_64-unknown-linux-musl
```

After checking the executable format, exercise bundled commands without host tools or ambient Store state. `AXE_STORE_MODE=off` tests the local pipeline, not on-demand package access:

```bash
env -i HOME=/tmp PATH=/nonexistent AXE_STORE_MODE=off \
  dist/axe-x86_64-unknown-linux-musl --no-config --norc --noprofile \
  -c 'printf "b\na\n" | sort | uniq'
```

If certificate authentication changed, start a real `sshd` and at least verify that an ordinary public key is rejected. A positive test needs a separate test CA private key; never use a production CA for tests.

## Troubleshooting

### The build requests `just generate-dev-keys`

At least one required embedded input is missing. For local development only:

```bash
just generate-dev-keys
```

For a production build, inspect the edition-root paths in the table above. Do not generate a random replacement for a missing production identity.

### `check-store-bootstrap` finds a difference

The Nix package set and generated inventory disagree. Regenerate it and recheck:

```bash
just store-bootstrap
just check-store-bootstrap
```

If an old signed bootstrap Index now disagrees with the new inventory, run a fresh Store sync before building AXE releases; do not copy a staged Index by hand.

### A remote recipe cannot reach its builder

Inspect `nix/builders.conf`, the dedicated private key, verified `known_hosts`, and whether the restricted key can reach `nix-daemon --stdio`. Local recipes intentionally ignore the remote configuration.

### A consumer cannot fetch the Index

Check these independently:

1. The **configured** `<endpoint>/<bucket>/<prefix>/index.cbor.zst` is publicly readable without credentials (for the checked-in OSS config, `/axe-store/store/index.cbor.zst`).
2. Endpoint, bucket, and prefix in `config/store.json` match the actual published path; `release_prefix` applies to AXE binaries, not the Store Index.
3. `consumer.addresses` contains the approved current A/AAAA addresses for that endpoint.
4. The binary trusts the publisher's signing key and its embedded signed bootstrap Index matches the current package inventory when built.

Never disable signature, TLS, or digest checks to diagnose a fetch failure: integrity errors must block execution.
