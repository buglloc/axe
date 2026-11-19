# Build a custom edition

All editions build from the same public source tree. Each edition sets the ID, SSH host key, trusted user CAs, relay defaults and credentials, command aliases, and AXE Store consumer trust embedded in `axe`. Keep the edition root outside tracked source or under the ignored `target/` directory. Keep private keys out of Git. If you only need different SSH or relay settings, you can reuse the OSS Store.

## Build locally with the OSS Store

Enter the development shell from the public checkout and create a separate edition root. This example uses the OSS Store configuration and trust without a signed bootstrap Index. Unless a verified Index is already cached, `axe` needs network access to discover Store tools. Set `id` and `sshd.principals` for your edition. The ID accepts only lowercase ASCII letters, digits, and hyphens.

```bash
just shell
export AXE_EDITION_ROOT="$PWD/target/my-axe-edition"
umask 077
mkdir -p target
mkdir -m 0700 "$AXE_EDITION_ROOT"
mkdir -p "$AXE_EDITION_ROOT/config" "$AXE_EDITION_ROOT/store/trusted"
cp config/store.json "$AXE_EDITION_ROOT/config/store.json"
cp config/bootstrap.json "$AXE_EDITION_ROOT/config/bootstrap.json"
cp store/bootstrap.json "$AXE_EDITION_ROOT/store/bootstrap.json"
cp store/trusted/*.pub "$AXE_EDITION_ROOT/store/trusted/"
cat > "$AXE_EDITION_ROOT/config/edition.json" <<'JSON'
{
  "schema_version": 3,
  "id": "my-axe",
  "sshd": { "principals": ["axe"] },
  "relay": {
    "enabled_by_default": false,
    "tcp_endpoint": null,
    "quic_endpoint": null
  },
  "aliases": {}
}
JSON
AXE_RELEASE_DIR="$AXE_EDITION_ROOT/dist" just build-linux-amd64
install -Dm755 "$AXE_EDITION_ROOT/dist/axe-x86_64-unknown-linux-musl" "$AXE_EDITION_ROOT/bin/axe"
"$AXE_EDITION_ROOT/bin/axe" --version
"$AXE_EDITION_ROOT/bin/axe" doctor --json
```

Edit `config/bootstrap.json` in the edition root to set `default_args`, for
example `["sshd"]`; an empty array keeps the normal `axe` invocation.

The build recipe generates missing development SSH and relay identities and leaves complete pairs alone. Do not deploy development identities. If you reuse Store trust without its signing key, skip `just generate-dev-keys`: the recipe requires a Store signing key when public trust already exists. Consumer builds do not need the Store signing key.

To discover Store tools offline on first run, copy the signed snapshot to `store/bootstrap-index.cbor.zst` under the edition root before building. The build verifies the snapshot's signature and trust and rejects an inventory that differs from `store/bootstrap.json`. The snapshot contains metadata, not tools; offline execution still needs cached artifacts. For a separate Store, provision its trust, metadata, endpoint, and publisher credentials. See [AXE Store](store.md) and [BOOTSTRAP.md](../BOOTSTRAP.md).

## Prepare a deployable edition

Supply deployment-specific `keys/ssh/host_ed25519` and public user CA keys in `keys/ssh/user_ca_keys` under `AXE_EDITION_ROOT`. Keep CA private keys outside the distributed bundle. `sshd.principals` is the allowlist of login names; the login must also match a principal in the user's SSH certificate. Ordinary public keys are not accepted.

For TCP, set `relay.tcp_endpoint` and provision `keys/relay/token` (at least 32 bytes). `enabled_by_default: true` requires both. For QUIC, set `relay.quic_endpoint` and supply `keys/relay/quic_server_cert.pem`, `quic_client_cert.pem`, and `quic_client_key.pem`; keep the relay server's private key on its host. See [SSH server and relay](ssh-relay.md) for target options and service setup. Configured target-side credentials are embedded in `axe`, so protect the binary as a credential.

Keep publisher signing keys and S3 credentials out of this edition root unless it is also the controlled publisher. Use a separate `AXE_STORE_DIR` for each co-installed edition. Rebuild and redistribute `axe` after changing embedded configuration, trust, or identities. The OSS [release procedure](release.md) publishes only OSS artifacts; a custom edition needs its own publisher, destination, and release process.
