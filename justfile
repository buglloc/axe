set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

source_root := justfile_directory()
edition_root := env_var_or_default("AXE_EDITION_ROOT", source_root)
cargo_target_dir := env_var_or_default("CARGO_TARGET_DIR", source_root + "/target")
bin := "axe"
out := env_var_or_default("AXE_RELEASE_DIR", source_root + "/dist")
store_output := env_var_or_default("AXE_STORE_OUTPUT_DIR", edition_root + "/store")
store_flake := env_var_or_default("AXE_STORE_FLAKE", source_root)
web_inventory := env_var_or_default("AXE_WEB_INVENTORY", edition_root + "/web/data/registry.json")
store_image := "axe-store"
store_volume := env_var_or_default("AXE_STORE_VOLUME", "axe-store-nix")
apple_sdk_url := env_var_or_default("AXE_APPLE_SDK_URL", "https://storage.yandexcloud.net/axe-store/toolchain/MacOSX26.2.sdk.tar.gz")
apple_sdk_sha256 := env_var_or_default("AXE_APPLE_SDK_SHA256", "5f1f3b1a7cd66c6fa8cc699b18de88e5edd4d116bc7d4c25e5fd1a1fec200ec4")
apple_sdk_archive := cargo_target_dir + "/toolchains/MacOSX26.2.sdk.tar.gz"
apple_sdk_root := cargo_target_dir + "/toolchains/MacOSX26.2.sdk"

[positional-arguments]
dev *args:
    @if (( $# )); then exec nix develop . --command "$@"; else exec nix develop .; fi


build: build-linux-amd64

build-linux: build-linux-arm64 build-linux-amd64

build-darwin: build-darwin-arm64

build-all: build-linux build-darwin

_web-inventory axe:
    inventory="{{web_inventory}}"; mkdir -p "$(dirname "$inventory")"; tmp=$(mktemp "$inventory.XXXXXX"); store=$(mktemp -d); trap 'rm -f "$tmp"; rm -rf "$store"' EXIT; AXE_STORE_DIR="$store" AXE_STORE_URL=http://127.0.0.1:9 AXE_STORE_ADDRESSES=127.0.0.1 {{axe}} commands | jq -e '{commands: [.commands[] | select(.alias_of == null) | {name, source, category, synopsis}]} | if all(.commands[]; (.synopsis | type) == "string" and (.synopsis | length) > 0) then . else error("commands returned an entry without synopsis") end' > "$tmp"; mv "$tmp" "$inventory"

web-inventory: _prepare-axe-dev-keys
    AXE_EDITION_ROOT="{{edition_root}}" cargo build --locked -p axe
    just _web-inventory {{cargo_target_dir}}/debug/axe

web-build: web-inventory
    hugo --source web --minify --cleanDestinationDir

web-serve: web-inventory
    hugo server --source web --disableFastRender

_prepare-axe-dev-keys:
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{edition_root}}"
    mkdir -p "$root/keys/ssh" "$root/keys/relay" "$root/keys/store/trusted"
    if ! test -s "$root/keys/ssh/host_ed25519"; then ssh-keygen -q -t ed25519 -N '' -C axe-bundled-host -f "$root/keys/ssh/host_ed25519"; fi
    rm -f "$root/keys/ssh/host_ed25519.pub"
    if ! test -s "$root/keys/ssh/user_ca_keys"; then ssh-keygen -q -t ed25519 -N '' -C axe-dev-user-ca -f "$root/keys/ssh/dev_user_ca"; cp "$root/keys/ssh/dev_user_ca.pub" "$root/keys/ssh/user_ca_keys"; fi
    if ! test -s "$root/keys/relay/token"; then (cd "$root" && cargo run --quiet --manifest-path "{{source_root}}/Cargo.toml" -p axe-store -- keys relay-token); fi
    if ! test -s "$root/keys/relay/quic_server_cert.pem" && ! test -s "$root/keys/relay/quic_server_key.pem" && ! test -s "$root/keys/relay/quic_client_cert.pem" && ! test -s "$root/keys/relay/quic_client_key.pem"; then (cd "$root" && cargo run --quiet --manifest-path "{{source_root}}/Cargo.toml" -p axe-store -- keys relay-identities); elif ! test -s "$root/keys/relay/quic_server_cert.pem" || ! test -s "$root/keys/relay/quic_server_key.pem" || ! test -s "$root/keys/relay/quic_client_cert.pem" || ! test -s "$root/keys/relay/quic_client_key.pem"; then echo "$root/keys/relay QUIC identities are incomplete" >&2; exit 1; fi
    shopt -s nullglob
    trusted_keys=("$root"/keys/store/trusted/*.pub)
    if ((${#trusted_keys[@]} == 0)); then echo "$root/keys/store/trusted has no public keys; run just generate-dev-keys" >&2; exit 1; fi
    chmod 0600 "$root/keys/ssh/host_ed25519" "$root/keys/relay/token" "$root/keys/relay/quic_server_key.pem" "$root/keys/relay/quic_client_key.pem"

generate-dev-keys:
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{edition_root}}"
    mkdir -p "$root/keys/store/trusted"
    shopt -s nullglob
    trusted_keys=("$root"/keys/store/trusted/*.pub)
    if ! test -s "$root/keys/store/signing.key" && ((${#trusted_keys[@]} == 0)); then (cd "$root" && cargo run --quiet --manifest-path "{{source_root}}/Cargo.toml" -p axe-store -- keys generate); elif ! test -s "$root/keys/store/signing.key" || ((${#trusted_keys[@]} == 0)); then echo "$root/keys/store signing/trusted pair is incomplete" >&2; exit 1; fi
    chmod 0600 "$root/keys/store/signing.key"
    just _prepare-axe-dev-keys

apple-sdk:
    test -n "{{apple_sdk_url}}" || { echo "AXE_APPLE_SDK_URL is required for Darwin builds" >&2; exit 2; }
    test -n "{{apple_sdk_sha256}}" || { echo "AXE_APPLE_SDK_SHA256 is required for Darwin builds" >&2; exit 2; }
    mkdir -p "{{cargo_target_dir}}/toolchains"
    if ! printf '%s  %s\n' {{apple_sdk_sha256}} {{apple_sdk_archive}} | sha256sum -c - >/dev/null 2>&1; then tmp={{apple_sdk_archive}}.part; rm -f "$tmp"; curl -fL --retry 3 -o "$tmp" {{apple_sdk_url}}; printf '%s  %s\n' {{apple_sdk_sha256}} "$tmp" | sha256sum -c -; mv "$tmp" {{apple_sdk_archive}}; fi
    if ! test -f {{apple_sdk_root}}/SDKSettings.json; then tmp={{apple_sdk_root}}.unpack; rm -rf "$tmp"; mkdir -p "$tmp"; tar --warning=no-unknown-keyword --no-same-owner --no-same-permissions -xzf {{apple_sdk_archive}} -C "$tmp"; test -f "$tmp/MacOSX26.2.sdk/SDKSettings.json"; rm -rf {{apple_sdk_root}}; mv "$tmp/MacOSX26.2.sdk" {{apple_sdk_root}}; rmdir "$tmp"; fi

_build-darwin target: apple-sdk
    rustup target add {{target}}
    SDKROOT="{{apple_sdk_root}}" AXE_EDITION_ROOT="{{edition_root}}" cargo zigbuild --locked -p axe --release --target {{target}}

build-darwin-arm64: _prepare-axe-dev-keys
    just _build-darwin aarch64-apple-darwin
    just _stage-artifact {{cargo_target_dir}}/aarch64-apple-darwin/release/{{bin}} {{out}}/{{bin}}-aarch64-apple-darwin

_verify-linux artifact:
    if ! readelf -hW "{{artifact}}" | grep -Eq '^[[:space:]]*Type:[[:space:]]*EXEC'; then echo "{{artifact}} is not a static executable (ELF type must be EXEC)" >&2; exit 1; fi; if ! file "{{artifact}}" | grep -F 'statically linked' >/dev/null; then echo "{{artifact}} is not statically linked" >&2; exit 1; fi; if readelf -lW "{{artifact}}" | grep -Eq '^[[:space:]]*INTERP[[:space:]]'; then echo "{{artifact}} contains an INTERP program header" >&2; exit 1; fi; if readelf -dW "{{artifact}}" | grep -q '(NEEDED)'; then echo "{{artifact}} contains DT_NEEDED entries" >&2; exit 1; fi

_stage-artifact source destination:
    mkdir -p {{out}}
    staged={{destination}}.tmp; rm -f "$staged"; cp {{source}} "$staged"; mv -f "$staged" {{destination}}

build-linux-arm64: _prepare-axe-dev-keys
    rm -f {{cargo_target_dir}}/aarch64-unknown-linux-musl/release/{{bin}}
    AXE_EDITION_ROOT="{{edition_root}}" CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$PWD/tools/rust-lld" cargo build --locked -p axe --release --target aarch64-unknown-linux-musl
    just _verify-linux {{cargo_target_dir}}/aarch64-unknown-linux-musl/release/{{bin}}
    just _stage-artifact {{cargo_target_dir}}/aarch64-unknown-linux-musl/release/{{bin}} {{out}}/{{bin}}-aarch64-unknown-linux-musl

build-linux-amd64: _prepare-axe-dev-keys
    rm -f {{cargo_target_dir}}/x86_64-unknown-linux-musl/release/{{bin}}
    AXE_EDITION_ROOT="{{edition_root}}" CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$PWD/tools/rust-lld" cargo build --locked -p axe --release --target x86_64-unknown-linux-musl
    just _verify-linux {{cargo_target_dir}}/x86_64-unknown-linux-musl/release/{{bin}}
    smoke_dir=$(mktemp -d); trap 'rm -rf "$smoke_dir"' EXIT; env -i HOME=/tmp PATH=/nonexistent AXE_STORE_DIR="$smoke_dir" AXE_STORE_URL=http://127.0.0.1:9 AXE_STORE_ADDRESSES=127.0.0.1 {{cargo_target_dir}}/x86_64-unknown-linux-musl/release/{{bin}} --list >/dev/null; env -i HOME=/tmp PATH=/nonexistent AXE_STORE_DIR="$smoke_dir" AXE_STORE_URL=http://127.0.0.1:9 AXE_STORE_ADDRESSES=127.0.0.1 {{cargo_target_dir}}/x86_64-unknown-linux-musl/release/{{bin}} --no-config --norc --noprofile -c 'commands >/dev/null && ps >/dev/null'
    just _stage-artifact {{cargo_target_dir}}/x86_64-unknown-linux-musl/release/{{bin}} {{out}}/{{bin}}-x86_64-unknown-linux-musl

build-relay-linux-amd64:
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$PWD/tools/rust-lld" cargo build --locked -p axe-relay --release --target x86_64-unknown-linux-musl
    just _verify-linux {{cargo_target_dir}}/x86_64-unknown-linux-musl/release/axe-relay
    just _stage-artifact {{cargo_target_dir}}/x86_64-unknown-linux-musl/release/axe-relay {{out}}/axe-relay-x86_64-unknown-linux-musl

build-relay-linux-arm64:
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$PWD/tools/rust-lld" cargo build --locked -p axe-relay --release --target aarch64-unknown-linux-musl
    just _verify-linux {{cargo_target_dir}}/aarch64-unknown-linux-musl/release/axe-relay
    just _stage-artifact {{cargo_target_dir}}/aarch64-unknown-linux-musl/release/axe-relay {{out}}/axe-relay-aarch64-unknown-linux-musl

build-relay-darwin-arm64: apple-sdk
    rustup target add aarch64-apple-darwin
    SDKROOT="{{apple_sdk_root}}" cargo zigbuild --locked -p axe-relay --release --target aarch64-apple-darwin
    just _stage-artifact {{cargo_target_dir}}/aarch64-apple-darwin/release/axe-relay {{out}}/axe-relay-aarch64-apple-darwin

build-relay-all: build-relay-linux-amd64 build-relay-linux-arm64 build-relay-darwin-arm64



# Local release preparation never publishes or creates a tag. Review and commit its output.
[positional-arguments]
release-prepare bump:
    python3 tools/release.py prepare "$1"

# Run only from the trusted publisher machine, after pushing the reviewed commit.
release-publish:
    python3 tools/release.py publish --edition-root "{{edition_root}}" --output "{{out}}"

# Exercise both publisher phases without touching GitHub, S3, or tags.
[positional-arguments]
release-check directory:
    python3 tools/release.py publish --edition-root "{{edition_root}}" --output "{{out}}" --local-directory "$1"

store-image:
    TMPDIR=/tmp ${CONTAINER_RUNTIME:-podman} build -f store/Containerfile -t {{store_image}} .

_store-volume:
    ${CONTAINER_RUNTIME:-podman} volume inspect {{store_volume}} >/dev/null 2>&1 || ${CONTAINER_RUNTIME:-podman} volume create {{store_volume}} >/dev/null


[positional-arguments]
_store-run *args:
    #!/usr/bin/env bash
    set -euo pipefail
    run_extra=()
    case "${AXE_STORE_REMOTE:-0}" in
        0) ;;
        1)
            if [[ ! -f "{{edition_root}}/nix/builders.conf" ]]; then
                echo 'AXE_STORE_REMOTE=1 requires edition nix/builders.conf; see BOOTSTRAP.md "Remote builder"' >&2
                exit 1
            fi
            if [[ ! -f "{{edition_root}}/keys/nix/id_ed25519" || ! -f "{{edition_root}}/keys/nix/known_hosts" ]]; then
                echo 'AXE_STORE_REMOTE=1 requires edition keys/nix/id_ed25519 and keys/nix/known_hosts; see BOOTSTRAP.md "Remote builder"' >&2
                exit 1
            fi
            run_extra+=(
                -e NIX_USER_CONF_FILES=/workspace/nix/builders.conf
                -v "{{edition_root}}/nix/builders.conf:/workspace/nix/builders.conf:ro"
                -v "{{edition_root}}/keys/nix/id_ed25519:/root/.ssh/id_ed25519:ro"
                -v "{{edition_root}}/keys/nix/known_hosts:/etc/ssh/ssh_known_hosts:ro"
            )
            ;;
        *)
            echo 'AXE_STORE_REMOTE must be 0 or 1' >&2
            exit 2
            ;;
    esac
    exec ${CONTAINER_RUNTIME:-podman} run --rm --init "${run_extra[@]}" -v {{store_volume}}:/nix -v "{{store_output}}:/output" -v "{{edition_root}}/config:/workspace/config:ro" -v "{{edition_root}}/keys/store:/workspace/keys/store:ro" -v "{{store_flake}}:/flake:ro" {{store_image}} "$@"

keyscan-nix-builders:
    #!/usr/bin/env bash
    set -euo pipefail
    builders="{{edition_root}}/nix/builders.conf"
    keys="{{edition_root}}/keys/nix"
    if [[ ! -f "$builders" ]]; then
        echo 'edition nix/builders.conf not found; see BOOTSTRAP.md "Remote builder"' >&2
        exit 1
    fi
    mkdir -p "$keys"
    hosts=$(sed -n 's/^ *builders *= *//p' "$builders" | tr ' ' '\n' | grep -E '^ssh(-ng)?://' | sed -E 's|^ssh(-ng)?://([^@/]+@)?||' | sort -u)
    if [[ -z "$hosts" ]]; then
        echo 'no ssh:// builders found in edition nix/builders.conf' >&2
        exit 1
    fi
    known=$(mktemp "$keys/known_hosts.XXXXXX")
    trap 'rm -f "$known"' EXIT
    for host in $hosts; do
        echo "scanning host key: $host" >&2
        ssh-keyscan -t ed25519 "$host" >>"$known"
    done
    test -s "$known"
    mv -f "$known" "$keys/known_hosts"
    trap - EXIT

store-build: store-image _store-volume
    just _store-run build --flake /flake --output /output/dist

store-build-remote:
    AXE_STORE_REMOTE=1 just store-build

store-publish: store-image
    ${CONTAINER_RUNTIME:-podman} run --rm --init -v "{{edition_root}}/config:/workspace/config:ro" -v "{{store_output}}/dist:/workspace/store/dist:ro" -v "{{edition_root}}/keys/store:/workspace/keys/store:ro" {{store_image}} publish

[positional-arguments]
store-diagnose-upload *args: store-image
    #!/usr/bin/env bash
    set -euo pipefail
    exec ${CONTAINER_RUNTIME:-podman} run --rm --init -v "{{edition_root}}/config:/workspace/config:ro" -v "{{edition_root}}/keys/store:/workspace/keys/store:ro" {{store_image}} diagnose-upload "$@"

_sync-store-snapshot: store-bootstrap store-image _store-volume
    removal=(); if [[ ${AXE_STORE_ALLOW_TARGET_REMOVAL:-0} == 1 ]]; then removal+=(--allow-target-removal); fi; just _store-run sync --flake /flake --output /output/dist "${removal[@]}"
    snapshot="{{edition_root}}/store/bootstrap-index.cbor.zst"; staged="{{store_output}}/dist/index.cbor.zst"; test -s "$staged" || { echo "Store sync did not produce signed Index: $staged" >&2; exit 1; }; mkdir -p "$(dirname "$snapshot")"; tmp=$(mktemp "$snapshot.XXXXXX"); trap 'rm -f "$tmp"' EXIT; cp "$staged" "$tmp"; mv -f "$tmp" "$snapshot"; trap - EXIT

store-sync: _sync-store-snapshot

store-sync-remote:
    AXE_STORE_REMOTE=1 just store-sync

store-smoke: generate-dev-keys
    bash store/tests/smoke.sh

store-nix-smoke: generate-dev-keys
    bash store/tests/nix-smoke.sh

store-bootstrap:
    bootstrap="{{edition_root}}/store/bootstrap.json"; mkdir -p "$(dirname "$bootstrap")"; tmp=$(mktemp "$bootstrap.XXXXXX"); trap 'rm -f "$tmp"' EXIT; nix --extra-experimental-features 'nix-command flakes' eval --raw --apply 'value: (builtins.toJSON value) + "\n"' "{{store_flake}}#lib.axeStoreMetadata" > "$tmp"; mv "$tmp" "$bootstrap"; trap - EXIT

check-store-bootstrap:
    tmp=$(mktemp); trap 'rm -f "$tmp"' EXIT; nix --extra-experimental-features 'nix-command flakes' eval --raw --apply 'value: (builtins.toJSON value) + "\n"' "{{store_flake}}#lib.axeStoreMetadata" > "$tmp"; cmp "{{edition_root}}/store/bootstrap.json" "$tmp"
nix-fmt:
    alejandra store/nix flake.nix

benchmark-opt-level: generate-dev-keys
    python3 tools/benchmark-opt-level.py

check: _prepare-axe-dev-keys check-store-bootstrap
    AXE_EDITION_ROOT="{{edition_root}}" cargo check --locked --workspace --all-targets --all-features

smoke: generate-dev-keys
    AXE_EDITION_ROOT="{{edition_root}}" cargo test --locked --workspace --all-features


relay-live-smoke: generate-dev-keys
    AXE_EDITION_ROOT="{{edition_root}}" cargo build --locked -p axe -p axe-relay
    AXE_EDITION_ROOT="{{edition_root}}" bash scripts/relay-live-smoke.sh {{cargo_target_dir}}/debug/axe {{cargo_target_dir}}/debug/axe-relay

applet-parity: generate-dev-keys
    AXE_EDITION_ROOT="{{edition_root}}" cargo test --locked -p axe --all-features --test applet_parity
