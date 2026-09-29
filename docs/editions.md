# Build a custom edition

An edition changes the inputs embedded in `axe`: its ID, SSH host key and trusted user CAs, relay defaults and credentials, command aliases, and AXE Store consumer trust. It uses the same public source tree. Keep the edition root outside tracked source (or under ignored `target/`) and keep private keys out of Git. An edition that only needs different SSH or relay settings can use the existing OSS Store without publishing another Store.

## Build locally with the OSS Store

From the public checkout, enter the development shell and create an isolated edition root. This example uses the OSS Store configuration and public trust, but no embedded signed Index. With network access, `axe` can fetch and verify the published Index; on a first run offline, it needs an existing verified cache to discover Store tools. Replace `my-axe` and the SSH principal with your own values. The edition ID must contain only lowercase ASCII letters, digits, and hyphens.

```bash
nix develop .#default
export AXE_EDITION_ROOT="$PWD/target/my-axe-edition"
umask 077
mkdir -p target
mkdir -m 0700 "$AXE_EDITION_ROOT"
mkdir -p "$AXE_EDITION_ROOT/config" "$AXE_EDITION_ROOT/store/trusted"
cp config/store.json "$AXE_EDITION_ROOT/config/store.json"
cp store/bootstrap.json "$AXE_EDITION_ROOT/store/bootstrap.json"
cp store/trusted/*.pub "$AXE_EDITION_ROOT/store/trusted/"
cat > "$AXE_EDITION_ROOT/edition.json" <<'JSON'
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

The build recipe generates missing **development** SSH and relay identities in this edition root. It does not replace complete pairs. Do not deploy those identities as production credentials. Do not run `just generate-dev-keys` for an edition that reuses Store trust without owning the matching private Store signing key: that recipe requires a complete signing-key/public-trust pair. Building the consumer does not require the signing key.

For an offline bootstrap, copy a current `store/bootstrap-index.cbor.zst` into the edition root before building. The build verifies its signature, trusted key, and inventory against `store/bootstrap.json`; a stale snapshot fails the build. Without a snapshot, offline Store discovery requires a previously verified cache. To publish a different Store, provision its own trusted public keys, metadata, endpoint, and publisher credentials separately; see [AXE Store](store.md) and [BOOTSTRAP.md](../BOOTSTRAP.md).

## Prepare a deployable edition

Supply deployment-specific `keys/ssh/host_ed25519` and public user CA keys in `keys/ssh/user_ca_keys` under `AXE_EDITION_ROOT`. Keep CA private keys outside the distributed bundle. `sshd.principals` is the allowlist of login names; the login must also match a principal in the user's SSH certificate. Ordinary public keys are not accepted.

For a configured TCP relay, set `relay.tcp_endpoint` and provision `keys/relay/token` (at least 32 bytes). Set `enabled_by_default: true` only with a TCP endpoint and token. For QUIC, set `relay.quic_endpoint` and supply `keys/relay/quic_server_cert.pem`, `quic_client_cert.pem`, and `quic_client_key.pem`; the relay server's private key stays on the relay host. See [SSH server and relay](ssh-relay.md) for target options and service setup. The build embeds target-side credentials when their transport is configured, so protect and redistribute the binary accordingly.

Keep publisher signing keys and S3 credentials out of this edition root unless it is also the controlled publisher. Use a separate `AXE_STORE_DIR` for each co-installed edition. Rebuild and redistribute `axe` after changing embedded configuration, trust, or identities. The OSS [release procedure](release.md) publishes only OSS artifacts; a custom edition needs its own publisher, destination, and release process.
