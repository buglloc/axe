set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

source_root := justfile_directory()
edition_root := env_var_or_default("AXE_EDITION_ROOT", source_root)
cargo_target_dir := env_var_or_default("CARGO_TARGET_DIR", source_root + "/target")
out := env_var_or_default("AXE_RELEASE_DIR", source_root + "/dist")
store_output := env_var_or_default("AXE_STORE_OUTPUT_DIR", edition_root + "/store")
store_flake := env_var_or_default("AXE_STORE_FLAKE", source_root)
store_image := "axe-store"
store_volume := env_var_or_default("AXE_STORE_VOLUME", "axe-store-nix")
apple_sdk_url := env_var_or_default("AXE_APPLE_SDK_URL", "https://storage.yandexcloud.net/axe-store/toolchain/MacOSX26.2.sdk.tar.gz")
apple_sdk_sha256 := env_var_or_default("AXE_APPLE_SDK_SHA256", "5f1f3b1a7cd66c6fa8cc699b18de88e5edd4d116bc7d4c25e5fd1a1fec200ec4")
apple_sdk_archive := cargo_target_dir + "/toolchains/MacOSX26.2.sdk.tar.gz"
apple_sdk_root := cargo_target_dir + "/toolchains/MacOSX26.2.sdk"

[positional-arguments]
dev *args:
    @if (( $# )); then exec nix develop . --command "$@"; else exec nix develop .; fi


_require-reference-tools:
    @if [[ -z "${AXE_REFERENCE_PATH:-}" ]]; then echo 'AXE_REFERENCE_PATH is not set; enter `nix develop .#default` before running this recipe' >&2; exit 1; fi

build: build-linux-amd64

build-linux: build-linux-arm64 build-linux-amd64

build-darwin: build-darwin-arm64

build-all: build-linux build-darwin

_web-inventory axe inventory:
    mkdir -p "$(dirname "{{inventory}}")"; tmp=$(mktemp "{{inventory}}.XXXXXX"); store=$(mktemp -d); trap 'rm -f "$tmp"; rm -rf "$store"' EXIT; AXE_STORE_DIR="$store" AXE_STORE_URL=http://127.0.0.1:9 AXE_STORE_ADDRESSES=127.0.0.1 {{axe}} commands | jq -e '{commands: [.commands[] | select(.alias_of == null) | {name, source, category, synopsis}]} | if (.commands | length) > 0 and all(.commands[]; (.synopsis | type) == "string" and (.synopsis | length) > 0) then . else error("commands returned no entries or an entry without synopsis") end' > "$tmp"; mv "$tmp" "{{inventory}}"

web-inventory: _prepare-axe-dev-keys
    AXE_EDITION_ROOT="{{edition_root}}" cargo build --locked -p axe
    just _web-inventory {{cargo_target_dir}}/debug/axe {{edition_root}}/web/data/registry.json

web-build: web-inventory
    hugo --source web --minify --cleanDestinationDir

web-serve: web-inventory
    hugo server --source web --disableFastRender

_prepare-axe-dev-keys:
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{edition_root}}"
    mkdir -p "$root/keys/ssh" "$root/keys/relay" "$root/store/trusted"
    if ! test -s "$root/keys/ssh/host_ed25519"; then ssh-keygen -q -t ed25519 -N '' -C axe-bundled-host -f "$root/keys/ssh/host_ed25519"; fi
    rm -f "$root/keys/ssh/host_ed25519.pub"
    if ! test -s "$root/keys/ssh/user_ca_keys"; then ssh-keygen -q -t ed25519 -N '' -C axe-dev-user-ca -f "$root/keys/ssh/dev_user_ca"; cp "$root/keys/ssh/dev_user_ca.pub" "$root/keys/ssh/user_ca_keys"; fi
    if ! test -s "$root/keys/relay/token"; then cargo run --quiet --manifest-path "{{source_root}}/Cargo.toml" -p axe-store -- keys relay-token --output "$root/keys/relay/token"; fi
    if ! test -s "$root/keys/relay/quic_server_cert.pem" && ! test -s "$root/keys/relay/quic_server_key.pem" && ! test -s "$root/keys/relay/quic_client_cert.pem" && ! test -s "$root/keys/relay/quic_client_key.pem"; then cargo run --quiet --manifest-path "{{source_root}}/Cargo.toml" -p axe-store -- keys relay-identities --output "$root/keys/relay"; elif ! test -s "$root/keys/relay/quic_server_cert.pem" || ! test -s "$root/keys/relay/quic_server_key.pem" || ! test -s "$root/keys/relay/quic_client_cert.pem" || ! test -s "$root/keys/relay/quic_client_key.pem"; then echo "$root/keys/relay QUIC identities are incomplete" >&2; exit 1; fi
    shopt -s nullglob
    trusted_keys=("$root"/store/trusted/*.pub)
    if ((${#trusted_keys[@]} == 0)); then echo "$root/store/trusted has no public keys; run just generate-dev-keys for a new Store or supply the existing Store's trusted keys" >&2; exit 1; fi
    chmod 0600 "$root/keys/ssh/host_ed25519" "$root/keys/relay/token" "$root/keys/relay/quic_server_key.pem" "$root/keys/relay/quic_client_key.pem"

generate-dev-keys:
    #!/usr/bin/env bash
    set -euo pipefail
    root="{{edition_root}}"
    mkdir -p "$root/store/trusted"
    shopt -s nullglob
    trusted_keys=("$root"/store/trusted/*.pub)
    if ! test -s "$root/keys/store/signing.key" && ((${#trusted_keys[@]} == 0)); then cargo run --quiet --manifest-path "{{source_root}}/Cargo.toml" -p axe-store -- keys generate --output "$root/keys/store" --trusted-output "$root/store/trusted"; elif ! test -s "$root/keys/store/signing.key" || ((${#trusted_keys[@]} == 0)); then echo "$root Store signing key/public trust pair is incomplete; a consumer edition does not need a signing key" >&2; exit 1; fi
    chmod 0600 "$root/keys/store/signing.key"
    just _prepare-axe-dev-keys

apple-sdk:
    test -n "{{apple_sdk_url}}" || { echo "AXE_APPLE_SDK_URL is required for Darwin builds" >&2; exit 2; }
    test -n "{{apple_sdk_sha256}}" || { echo "AXE_APPLE_SDK_SHA256 is required for Darwin builds" >&2; exit 2; }
    mkdir -p "{{cargo_target_dir}}/toolchains"
    if ! printf '%s  %s\n' {{apple_sdk_sha256}} {{apple_sdk_archive}} | sha256sum -c - >/dev/null 2>&1; then tmp={{apple_sdk_archive}}.part; rm -f "$tmp"; curl -fL --retry 3 -o "$tmp" {{apple_sdk_url}}; printf '%s  %s\n' {{apple_sdk_sha256}} "$tmp" | sha256sum -c -; mv "$tmp" {{apple_sdk_archive}}; fi
    if ! test -f {{apple_sdk_root}}/SDKSettings.json; then tmp={{apple_sdk_root}}.unpack; rm -rf "$tmp"; mkdir -p "$tmp"; tar --warning=no-unknown-keyword --no-same-owner --no-same-permissions -xzf {{apple_sdk_archive}} -C "$tmp"; test -f "$tmp/MacOSX26.2.sdk/SDKSettings.json"; rm -rf {{apple_sdk_root}}; mv "$tmp/MacOSX26.2.sdk" {{apple_sdk_root}}; rmdir "$tmp"; fi

_build-rust package target:
    #!/usr/bin/env bash
    set -euo pipefail
    artifact="{{cargo_target_dir}}/{{target}}/release/{{package}}"
    rm -f "$artifact"
    case "{{target}}" in
        *-unknown-linux-musl)
            linker="CARGO_TARGET_$(tr 'a-z-' 'A-Z_' <<<"{{target}}")_LINKER"
            env AXE_EDITION_ROOT="{{edition_root}}" "$linker={{source_root}}/tools/rust-lld" cargo build --locked -p {{package}} --release --target {{target}}
            if ! readelf -hW "$artifact" | grep -Eq '^[[:space:]]*Type:[[:space:]]*EXEC'; then echo "$artifact is not a static executable (ELF type must be EXEC)" >&2; exit 1; fi
            if ! file "$artifact" | grep -F 'statically linked' >/dev/null; then echo "$artifact is not statically linked" >&2; exit 1; fi
            if readelf -lW "$artifact" | grep -Eq '^[[:space:]]*INTERP[[:space:]]'; then echo "$artifact contains an INTERP program header" >&2; exit 1; fi
            if readelf -dW "$artifact" | grep -q '(NEEDED)'; then echo "$artifact contains DT_NEEDED entries" >&2; exit 1; fi
            ;;
        *-apple-darwin)
            just apple-sdk
            rustup target add {{target}}
            SDKROOT="{{apple_sdk_root}}" AXE_EDITION_ROOT="{{edition_root}}" cargo zigbuild --locked -p {{package}} --release --target {{target}}
            ;;
        *)
            echo "unsupported target: {{target}}" >&2
            exit 2
            ;;
    esac

    if [[ "{{package}} {{target}}" == "axe x86_64-unknown-linux-musl" ]]; then
        smoke_dir=$(mktemp -d)
        trap 'rm -rf "$smoke_dir"' EXIT
        env -i HOME=/tmp PATH=/nonexistent AXE_STORE_DIR="$smoke_dir" AXE_STORE_URL=http://127.0.0.1:9 AXE_STORE_ADDRESSES=127.0.0.1 "$artifact" --list >/dev/null
        env -i HOME=/tmp PATH=/nonexistent AXE_STORE_DIR="$smoke_dir" AXE_STORE_URL=http://127.0.0.1:9 AXE_STORE_ADDRESSES=127.0.0.1 "$artifact" --no-config --norc --noprofile -c 'commands >/dev/null && ps >/dev/null'
    fi

    mkdir -p "{{out}}"
    staged="{{out}}/{{package}}-{{target}}"
    rm -f "$staged.tmp"
    cp "$artifact" "$staged.tmp"
    mv -f "$staged.tmp" "$staged"

build-linux-arm64: _prepare-axe-dev-keys
    just _build-rust axe aarch64-unknown-linux-musl

build-linux-amd64: _prepare-axe-dev-keys
    just _build-rust axe x86_64-unknown-linux-musl

build-darwin-arm64: _prepare-axe-dev-keys
    just _build-rust axe aarch64-apple-darwin

build-relay-linux-amd64:
    just _build-rust axe-relay x86_64-unknown-linux-musl

build-relay-linux-arm64:
    just _build-rust axe-relay aarch64-unknown-linux-musl

build-relay-darwin-arm64:
    just _build-rust axe-relay aarch64-apple-darwin

build-relay-all: build-relay-linux-amd64 build-relay-linux-arm64 build-relay-darwin-arm64

build-vzik-linux-amd64:
    just _build-rust vzik x86_64-unknown-linux-musl

build-vzik-linux-arm64:
    just _build-rust vzik aarch64-unknown-linux-musl

build-vzik-darwin-arm64:
    just _build-rust vzik aarch64-apple-darwin

build-vzik-all: build-vzik-linux-amd64 build-vzik-linux-arm64 build-vzik-darwin-arm64




# Local release preparation never publishes or creates a tag. Review and commit its output.
[positional-arguments]
release-prepare bump:
    python3 tools/release.py prepare "$1"

# Run only from the trusted publisher machine, after pushing the reviewed commit.
release-publish: _require-reference-tools
    python3 tools/release.py publish --edition-root "{{edition_root}}" --output "{{out}}"

# Exercise both publisher phases without touching GitHub, S3, or tags.
[positional-arguments]
release-check directory: _require-reference-tools
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
    exec ${CONTAINER_RUNTIME:-podman} run --rm --init --network=host "${run_extra[@]}" -v {{store_volume}}:/nix -v "{{store_output}}:/output" -v "{{edition_root}}/config:/workspace/config:ro" -v "{{edition_root}}/keys/store:/workspace/keys/store:ro" -v "{{edition_root}}/store/trusted:/workspace/store/trusted:ro" -v "{{store_flake}}:/flake:ro" {{store_image}} "$@"

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
    ${CONTAINER_RUNTIME:-podman} run --rm --init --network=host -v "{{edition_root}}/config:/workspace/config:ro" -v "{{store_output}}/dist:/workspace/store/dist:ro" -v "{{edition_root}}/keys/store:/workspace/keys/store:ro" -v "{{edition_root}}/store/trusted:/workspace/store/trusted:ro" {{store_image}} publish

[positional-arguments]
store-diagnose-upload *args: store-image
    #!/usr/bin/env bash
    set -euo pipefail
    exec ${CONTAINER_RUNTIME:-podman} run --rm --init --network=host -v "{{edition_root}}/config:/workspace/config:ro" -v "{{edition_root}}/keys/store:/workspace/keys/store:ro" {{store_image}} diagnose-upload "$@"

_sync-store-snapshot: store-bootstrap store-image _store-volume
    removal=(); if [[ ${AXE_STORE_ALLOW_TARGET_REMOVAL:-0} == 1 ]]; then removal+=(--allow-target-removal); fi; just _store-run sync --flake /flake --output /output/dist "${removal[@]}"
    snapshot="{{edition_root}}/store/bootstrap-index.cbor.zst"; staged="{{store_output}}/dist/index.cbor.zst"; test -s "$staged" || { echo "Store sync did not produce signed Index: $staged" >&2; exit 1; }; mkdir -p "$(dirname "$snapshot")"; tmp=$(mktemp "$snapshot.XXXXXX"); trap 'rm -f "$tmp"' EXIT; cp "$staged" "$tmp"; mv -f "$tmp" "$snapshot"; trap - EXIT

store-sync: _sync-store-snapshot

store-sync-remote:
    AXE_STORE_REMOTE=1 just store-sync

store-smoke: _prepare-axe-dev-keys
    bash store/tests/smoke.sh

store-nix-smoke: _prepare-axe-dev-keys
    bash store/tests/nix-smoke.sh

store-bootstrap:
    bootstrap="{{edition_root}}/store/bootstrap.json"; mkdir -p "$(dirname "$bootstrap")"; tmp=$(mktemp "$bootstrap.XXXXXX"); trap 'rm -f "$tmp"' EXIT; nix --extra-experimental-features 'nix-command flakes' eval --raw --apply 'value: (builtins.toJSON value) + "\n"' "{{store_flake}}#lib.axeStoreMetadata" > "$tmp"; mv "$tmp" "$bootstrap"; trap - EXIT

check-store-bootstrap:
    tmp=$(mktemp); trap 'rm -f "$tmp"' EXIT; nix --extra-experimental-features 'nix-command flakes' eval --raw --apply 'value: (builtins.toJSON value) + "\n"' "{{store_flake}}#lib.axeStoreMetadata" > "$tmp"; cmp "{{edition_root}}/store/bootstrap.json" "$tmp"
nix-fmt:
    alejandra store/nix flake.nix

check: _prepare-axe-dev-keys check-store-bootstrap
    AXE_EDITION_ROOT="{{edition_root}}" cargo check --locked --workspace --all-targets --all-features

smoke: _require-reference-tools _prepare-axe-dev-keys
    AXE_EDITION_ROOT="{{edition_root}}" cargo test --locked --workspace --all-features

applet-parity: _require-reference-tools _prepare-axe-dev-keys
    AXE_EDITION_ROOT="{{edition_root}}" cargo test --locked -p axe --all-features --test applet_parity
