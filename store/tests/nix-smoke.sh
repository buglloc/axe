#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
mkdir -p "$target_dir"
tmp=$(mktemp -d "$target_dir/axe-store-nix-smoke.XXXXXX")
server_pid=

cleanup() {
    local status=$?
    if test -n "$server_pid"; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    if test -n "${AXE_STORE_NIX_SMOKE_KEEP:-}"; then
        echo "AXE Store Nix smoke artifacts: $tmp" >&2
    else
        rm -rf "$tmp"
    fi
    return "$status"
}
trap cleanup EXIT

wait_for_port() {
    local file=$1
    for _ in $(seq 1 100); do
        test -s "$file" && return 0
        sleep 0.05
    done
    echo "AXE Store Nix smoke server did not publish its port" >&2
    return 1
}

lock_before=$(sha256sum "$root/flake.lock" | cut -d ' ' -f 1)
mkdir -p "$tmp/output" "$tmp/inspect/src"
cp "$root/flake.lock" "$tmp/flake.lock"
cat >"$tmp/flake.nix" <<'EOF'
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      linux = import nixpkgs { system = "x86_64-linux"; };
      darwin = import nixpkgs { system = "aarch64-darwin"; };
      missing = linux.runCommand "missing-nix-package-1.0.0" { } ''
        echo "intentional target-local failure" >&2
        exit 1
      '';
    in
    {
      packages.x86_64-linux = {
        hello = linux.pkgsStatic.hello;
        inherit missing;
      };
      packages.aarch64-darwin.hello = darwin.hello;

      lib.axeStoreMetadata = {
        hello = {
          id = "nix-smoke/hello";
          name = "hello";
          aliases = [ ];
          synopsis = "Nix smoke fixture";
          channels.stable = "2.12.3";
          targets = [ "x86_64-linux" "aarch64-darwin" ];
          artifact = {
            type = "single_binary";
            path = "bin/hello";
          };
        };
        missing = {
          id = "nix-smoke/missing";
          name = "missing-nix-package";
          aliases = [ ];
          synopsis = null;
          channels.stable = "1.0.0";
          targets = [ "x86_64-linux" ];
          artifact = {
            type = "single_binary";
            path = "bin/missing";
          };
        };
      };
    };
}
EOF

cargo build --quiet -p axe -p axe-store
"$target_dir/debug/axe-store" keys generate \
    --output "$tmp/keys/store" --trusted-output "$tmp/store/trusted" >/dev/null
signing_key=$(cat "$tmp/keys/store/signing.key")
AXE_STORE_SIGNING_KEY="$signing_key" "$target_dir/debug/axe-store" build \
    --flake "$tmp" --output "$tmp/output" >"$tmp/build.log" 2>&1

grep -Fq 'unsupported: nix-smoke/missing x86_64-linux:' "$tmp/build.log"
test ! -d "$tmp/output/tools/nix-smoke/missing/manifests"
manifest=$(python3 - "$tmp/output" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

output = Path(sys.argv[1])
index = json.loads((output / "index.json").read_text(encoding="utf-8"))
assert "missing-nix-package" not in index["tools"]
entry = index["tools"]["hello"]
digest = entry["manifest_sha256"]
manifest = output / "tools" / entry["id"] / "manifests" / f"{digest}.cbor"
assert hashlib.sha256(manifest.read_bytes()).hexdigest() == digest
print(manifest)
PY
)

cat >"$tmp/inspect/Cargo.toml" <<EOF
[package]
name = "axe-store-nix-smoke-inspect"
version = "0.0.0"
edition = "2024"

[workspace]

[dependencies]
axe-artifact = { path = "$root/crates/axe-artifact" }
ed25519-dalek = "2.2"
EOF
cat >"$tmp/inspect/src/main.rs" <<'EOF'
use std::{env, fs};

use axe_artifact::{ToolManifest, TrustedKeys, verify_document};
use ed25519_dalek::VerifyingKey;

fn main() {
    let mut args = env::args_os().skip(1);
    let manifest = fs::read(args.next().expect("manifest path")).expect("read manifest");
    let encoded = fs::read_to_string(args.next().expect("key path")).expect("read key");
    let encoded = encoded.trim().as_bytes();
    assert_eq!(encoded.len(), 64, "public key must be lowercase hex");
    let mut bytes = [0_u8; 32];
    for (index, pair) in encoded.as_chunks::<2>().0.iter().enumerate() {
        bytes[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    let mut trusted = TrustedKeys::new();
    trusted.insert(VerifyingKey::from_bytes(&bytes).expect("valid public key"));
    let manifest: ToolManifest = verify_document(&manifest, &trusted).expect("verified manifest");
    let version = manifest.channels.get("stable").expect("stable channel");
    for (target, artifact) in &manifest.versions.get(version).expect("stable version").targets {
        println!("{target} {}", artifact.fully_static);
    }
}

fn nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("invalid hex"),
    }
}
EOF
trusted_path=$(printf '%s\n' "$tmp"/store/trusted/*.pub | sort | sed -n '1p')
cargo run --quiet --manifest-path "$tmp/inspect/Cargo.toml" -- \
    "$manifest" "$trusted_path" \
    >"$tmp/manifest.txt"
grep -Fxq 'x86_64-linux true' "$tmp/manifest.txt"
if ! grep -Fxq 'aarch64-darwin false' "$tmp/manifest.txt"; then
    grep -Fq 'unsupported: nix-smoke/hello aarch64-darwin:' "$tmp/build.log"
fi

python3 "$root/store/tests/https_server.py" \
    --directory "$tmp/output" --port-file "$tmp/store.port" &
server_pid=$!
wait_for_port "$tmp/store.port"
port=$(cat "$tmp/store.port")
trusted_key=$(cat "$trusted_path")
info=$(AXE_STORE_URL="http://127.0.0.1:$port/" \
    AXE_STORE_ADDRESSES=127.0.0.1 \
    AXE_STORE_TRUSTED_KEY="$trusted_key" \
    AXE_STORE_DIR="$tmp/client" \
    "$target_dir/debug/axe" hello --axe-tool-info)
printf '%s\n' "$info" | grep -Fq 'hello 2.12.3 x86_64-linux SingleBinary '
printf '%s\n' "$info" | grep -Fq ' static=true'
digest=$(printf '%s\n' "$info" | cut -d ' ' -f 5)
object="$tmp/output/objects/sha256/${digest:0:2}/$digest"
test -s "$object"
zstd -q -d -f "$object" -o "$tmp/hello"
file "$tmp/hello" | grep -Fq 'statically linked'
if readelf -l "$tmp/hello" | grep -q 'INTERP'; then
    echo 'Nix Store Linux artifact has an ELF interpreter' >&2
    exit 1
fi
if readelf -d "$tmp/hello" 2>&1 | grep -q 'NEEDED'; then
    echo 'Nix Store Linux artifact has dynamic dependencies' >&2
    exit 1
fi
if grep -a -q '/nix/store/' "$tmp/hello"; then
    echo 'Nix Store Linux artifact embeds a /nix/store path' >&2
    exit 1
fi

lock_after=$(sha256sum "$root/flake.lock" | cut -d ' ' -f 1)
test "$lock_before" = "$lock_after"
echo 'AXE Store Nix smoke: ok'
