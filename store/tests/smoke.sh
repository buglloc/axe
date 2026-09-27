#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
axe="$target_dir/debug/axe"
producer="$target_dir/debug/axe-store"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/axe-store-smoke.XXXXXX")
store_pid=

stop_server() {
    local pid=${1:-}
    if test -n "$pid"; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
}
cleanup() {
    local status=$?
    stop_server "$store_pid"
    if test -n "${AXE_STORE_SMOKE_KEEP:-}"; then
        echo "store smoke artifacts: $tmp" >&2
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
    echo "server did not publish its port" >&2
    return 1
}

start_store_server() {
    rm -f "$tmp/store.port"
    python3 "$root/store/tests/https_server.py" \
        --directory "$tmp/published" \
        --port "${1:-0}" \
        --port-file "$tmp/store.port" \
        --request-log "$tmp/store.requests" &
    store_pid=$!
    wait_for_port "$tmp/store.port"
    store_port=$(cat "$tmp/store.port")
}

cargo build --quiet -p axe -p axe-store
mkdir -p "$tmp/workspace/config" "$tmp/workspace/fixtures/package/bin" \
    "$tmp/path" "$tmp/published"

cat >"$tmp/tool.S" <<'EOF'
.global _start
.section .text
_start:
    cmpq $2, (%rsp)
    jb print
    movq 16(%rsp), %rsi
    movabsq $0x342d746978652d2d, %rax
    cmpq %rax, (%rsi)
    jne print
    cmpw $0x0032, 8(%rsi)
    jne print
    movq $60, %rax
    movq $42, %rdi
    syscall
print:
    movq $1, %rax
    movq $1, %rdi
    leaq output(%rip), %rsi
    movq $9, %rdx
    syscall
    movq $60, %rax
    xorq %rdi, %rdi
    syscall
.section .rodata
output:
    .ascii "store-ok\n"
EOF
cc -nostdlib -static -Wl,--build-id=none -s "$tmp/tool.S" \
    -o "$tmp/workspace/fixtures/rg"
cp "$tmp/workspace/fixtures/rg" "$tmp/workspace/fixtures/package/bin/pkg-smoke"
chmod 0755 "$tmp/workspace/fixtures/rg" \
    "$tmp/workspace/fixtures/package/bin/pkg-smoke"

cp "$root/flake.lock" "$tmp/workspace/flake.lock"
cat >"$tmp/workspace/flake.nix" <<'EOF'
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      pkgs = import nixpkgs { system = "x86_64-linux"; };
      rg = pkgs.runCommand "rg-1.0.0" { } ''
        install -Dm755 ${./fixtures/rg} "$out/bin/rg"
      '';
      package = pkgs.runCommand "pkg-smoke-1.0.0" { } ''
        mkdir -p "$out"
        cp -R ${./fixtures/package}/. "$out/"
      '';
    in
    {
      packages.x86_64-linux = {
        inherit rg;
        pkg-smoke = package;
      };

      lib.axeStoreMetadata = {
        rg = {
          id = "search/rg";
          name = "rg";
          aliases = [ ];
          synopsis = "AXE Store smoke executable";
          channels.stable = "1.0.0";
          targets = [ "x86_64-linux" ];
          artifact = {
            type = "single_binary";
            path = "bin/rg";
          };
        };
        pkg-smoke = {
          id = "smoke/pkg";
          name = "pkg-smoke";
          aliases = [ ];
          synopsis = "AXE Store smoke package";
          channels.stable = "1.0.0";
          targets = [ "x86_64-linux" ];
          artifact = {
            type = "package";
            entrypoint = "bin/pkg-smoke";
          };
        };
      };
    };
}
EOF

cp "$root/config/store.json" "$tmp/workspace/config/store.json"
(
    cd "$tmp/workspace"
    "$producer" keys generate >/dev/null
    "$producer" build --flake . --output store/dist >/dev/null
    snapshot_before=$(sha256sum store/dist/index.cbor.zst store/dist/objects/sha256/*/* store/dist/tools/*/*/manifests/*.cbor | sort)
    "$producer" build --flake . --output store/dist >/dev/null
    snapshot_after=$(sha256sum store/dist/index.cbor.zst store/dist/objects/sha256/*/* store/dist/tools/*/*/manifests/*.cbor | sort)
    test "$snapshot_before" = "$snapshot_after"
    for manifest in store/dist/tools/*/*/manifests/*.cbor; do
        test "$(sha256sum "$manifest" | cut -d ' ' -f 1).cbor" = "$(basename "$manifest")"
    done
    rg_manifests=(store/dist/tools/search/rg/manifests/*.cbor)
    test "${#rg_manifests[@]}" -eq 1
    printf '%s\n' "${rg_manifests[0]#store/dist/}" >"$tmp/manifest-relative"
    package_compatible=false
    for object in store/dist/objects/sha256/*/*; do
        zstd --test --quiet "$object"
        if tar --zstd -tf "$object" 2>/dev/null | grep -q '^bin/pkg-smoke$'; then
            package_compatible=true
        fi
    done
    "$package_compatible"
    "$producer" publish --input store/dist --backend directory \
        --directory "$tmp/published" --config config/store.json >/dev/null
    cmp store/dist/index.cbor.zst "$tmp/published/store/index.cbor.zst"
)
manifest_relative=$(cat "$tmp/manifest-relative")


trusted_key=$(cat "$tmp/workspace"/store/trusted/*.pub)
fixture_shell=$(command -v sh)
cat >"$tmp/path/rg" <<EOF
#!$fixture_shell
printf 'path-ok\n'
printf ran >"$tmp/path-ran"
EOF
chmod 0755 "$tmp/path/rg"

start_store_server 0
store_url="http://127.0.0.1:$store_port/store/"
if AXE_STORE_URL="$store_url" \
    AXE_STORE_ADDRESSES=127.0.0.1 \
    AXE_STORE_DIR="$tmp/untrusted-cache" \
    PATH="$tmp/path" \
    "$axe" rg >"$tmp/untrusted.out" 2>"$tmp/untrusted.err"; then
    echo "AXE accepted Store metadata signed by an untrusted key" >&2
    exit 1
else
    untrusted_status=$?
fi
test "$untrusted_status" -eq 126
grep -Eq 'signature|trusted key|unknown key' "$tmp/untrusted.err"
test ! -e "$tmp/path-ran"
run_axe() {
    local cache=$1
    shift
    AXE_STORE_URL="$store_url" \
    AXE_STORE_ADDRESSES=127.0.0.1 \
    AXE_STORE_TRUSTED_KEY="$trusted_key" \
    AXE_STORE_DIR="$cache" \
    PATH="$tmp/path" \
        "$axe" "$@"
}

cache="$tmp/cache"
run_axe "$cache" rg >"$tmp/online.out" 2>"$tmp/online.err"
metadata_namespaces=("$cache"/metadata/*)
test "${#metadata_namespaces[@]}" -eq 1
test -d "${metadata_namespaces[0]}"
metadata_namespace=${metadata_namespaces[0]}
test "$(cat "$tmp/online.out")" = store-ok
grep -q 'axe: downloading rg 1.0.0' "$tmp/online.err"
grep -q 'axe: downloaded rg 1.0.0' "$tmp/online.err"
! grep -q $'\r' "$tmp/online.err"
test ! -e "$tmp/path-ran"
requests_before=$(wc -l <"$tmp/store.requests")
run_axe "$cache" rg >"$tmp/fresh.out" 2>"$tmp/fresh.err"
requests_after=$(wc -l <"$tmp/store.requests")
test "$requests_before" -eq "$requests_after"
if test -s "$tmp/fresh.err"; then
    cat "$tmp/fresh.err" >&2
    exit 1
fi
run_axe "$cache" refresh-tools >"$tmp/refresh.out" 2>"$tmp/refresh.err"
grep -q 'verified Index generation 1' "$tmp/refresh.out"
grep -q 'GET /store/index.cbor.zst HTTP/1.1" 304' "$tmp/store.requests"
run_axe "$cache" commands --json >"$tmp/commands.json"
python3 - "$tmp/commands.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    commands = json.load(source)["commands"]
assert {"pkg-smoke", "rg"} <= {c["name"] for c in commands if c["source"] == "store"}
PY

# Like the sshd daemon, register from the cache, then revalidate the stale Index on use.
python3 - "$metadata_namespace/index/state.json" <<'PY'
import json, sys
path = sys.argv[1]
state = json.load(open(path, encoding="utf-8"))
state["checked_at"] = 0
json.dump(state, open(path, "w", encoding="utf-8"))
PY
index_requests_before=$(grep -c 'GET /store/index.cbor.zst' "$tmp/store.requests")
__AXE_STORE_INDEX_CACHE_ONLY=1 run_axe "$cache" rg >"$tmp/stale.out" 2>"$tmp/stale.err"
test "$(cat "$tmp/stale.out")" = store-ok
index_requests_after=$(grep -c 'GET /store/index.cbor.zst' "$tmp/store.requests")
test "$index_requests_after" -eq "$((index_requests_before + 1))"

run_axe "$cache" pkg-smoke >"$tmp/package.out" 2>"$tmp/package.err"
test "$(cat "$tmp/package.out")" = store-ok
package_entry=$(printf '%s\n' "$cache"/unpacked/sha256/*/*/tree/bin/pkg-smoke)
printf corrupt >"$package_entry"
run_axe "$cache" pkg-smoke >"$tmp/package-repaired.out" 2>"$tmp/package-repaired.err"
test "$(cat "$tmp/package-repaired.out")" = store-ok

object_digest=$(run_axe "$cache" rg --axe-tool-info | cut -d' ' -f5)
object_requests_before=$(grep -c "/objects/sha256/${object_digest:0:2}/$object_digest" "$tmp/store.requests")
run_axe "$tmp/concurrent-cache" rg >"$tmp/concurrent-one.out" 2>"$tmp/concurrent-one.err" &
first_download=$!
run_axe "$tmp/concurrent-cache" rg >"$tmp/concurrent-two.out" 2>"$tmp/concurrent-two.err" &
second_download=$!
wait "$first_download"
wait "$second_download"
test "$(cat "$tmp/concurrent-one.out")" = store-ok
test "$(cat "$tmp/concurrent-two.out")" = store-ok
object_requests_after=$(grep -c "/objects/sha256/${object_digest:0:2}/$object_digest" "$tmp/store.requests")
test "$object_requests_after" -eq "$((object_requests_before + 1))"

package_requests_before=$(grep -c '/objects/sha256/' "$tmp/store.requests")
run_axe "$tmp/concurrent-package-cache" pkg-smoke >"$tmp/concurrent-package-one.out" 2>"$tmp/concurrent-package-one.err" &
first_package=$!
run_axe "$tmp/concurrent-package-cache" pkg-smoke >"$tmp/concurrent-package-two.out" 2>"$tmp/concurrent-package-two.err" &
second_package=$!
wait "$first_package"
wait "$second_package"
test "$(cat "$tmp/concurrent-package-one.out")" = store-ok
test "$(cat "$tmp/concurrent-package-two.out")" = store-ok
package_requests_after=$(grep -c '/objects/sha256/' "$tmp/store.requests")
test "$package_requests_after" -eq "$((package_requests_before + 1))"
package_object_cache=$(printf '%s\n' "$tmp"/concurrent-package-cache/objects/sha256/*/*)
package_unpacked_cache=$(printf '%s\n' "$tmp"/concurrent-package-cache/unpacked/sha256/*/*)
test "$(stat -c %a "$package_object_cache")" = 700
test "$(stat -c %a "$package_unpacked_cache")" = 700

SHELL=/bin/sh script -qec "AXE_STORE_URL=$store_url AXE_STORE_ADDRESSES=127.0.0.1 AXE_STORE_TRUSTED_KEY=$trusted_key AXE_STORE_DIR=$tmp/cache-tty PATH=$tmp/path $axe rg" "$tmp/tty.log" >/dev/null
grep -q 'axe: downloading rg 1.0.0' "$tmp/tty.log"

stop_server "$store_pid"
store_pid=
AXE_STORE_MODE=cache-only run_axe "$cache" rg >"$tmp/offline.out" 2>"$tmp/offline.err"
test "$(cat "$tmp/offline.out")" = store-ok
! grep -q 'downloading' "$tmp/offline.err"

rm -f "$tmp/path-ran"
mkdir -p "$tmp/empty-cache"
cp -a "$cache/metadata" "$tmp/empty-cache/"
run_axe "$tmp/empty-cache" --norc --noprofile --no-config -c 'rg; echo after' \
    >"$tmp/fallback.out" 2>"$tmp/fallback.err"
grep -q '^path-ok$' "$tmp/fallback.out"
grep -q '^after$' "$tmp/fallback.out"
grep -q 'store temporarily unavailable' "$tmp/fallback.err"
test -e "$tmp/path-ran"

manifest="$tmp/published/store/$manifest_relative"
cp "$manifest" "$tmp/good-manifest"
printf broken >"$manifest"
python3 - "$metadata_namespace/${manifest_relative%.cbor}/state.json" <<'PY'
import json, sys
path = sys.argv[1]
state = json.load(open(path, encoding="utf-8"))
state["checked_at"] = 0
json.dump(state, open(path, "w", encoding="utf-8"))
PY
start_store_server "$store_port"
rm -f "$tmp/path-ran"
set +e
run_axe "$cache" rg >"$tmp/integrity.out" 2>"$tmp/integrity.err"
status=$?
set -e
test "$status" -eq 126
grep -q 'store integrity error' "$tmp/integrity.err"
test ! -e "$tmp/path-ran"
cp "$tmp/good-manifest" "$manifest"

stop_server "$store_pid"
store_pid=
object_digest=$(run_axe "$cache" rg --axe-tool-info | cut -d' ' -f5)
object="$cache/objects/sha256/${object_digest:0:2}/$object_digest/object"
printf corrupt >"$object"
rm -f "$tmp/path-ran"
set +e
run_axe "$cache" rg >"$tmp/corrupt-offline.out" 2>"$tmp/corrupt-offline.err"
status=$?
set -e
test "$status" -eq 126
grep -q 'store integrity error' "$tmp/corrupt-offline.err"
test ! -e "$tmp/path-ran"

test -s "$metadata_namespace/network-backoff.json"
python3 - "$metadata_namespace/network-backoff.json" <<'PY'
import json, sys
path = sys.argv[1]
state = json.load(open(path, encoding="utf-8"))
state["retry_after"] = 0
json.dump(state, open(path, "w", encoding="utf-8"))
PY
start_store_server "$store_port"
run_axe "$cache" rg >"$tmp/repaired.out" 2>"$tmp/repaired.err"
test "$(cat "$tmp/repaired.out")" = store-ok
rm -f "$tmp/path-ran"
set +e
run_axe "$cache" rg --exit-42 >"$tmp/exit42.out" 2>"$tmp/exit42.err"
status=$?
set -e
test "$status" -eq 42
test ! -e "$tmp/path-ran"
(
    store_url="http://localhost:$store_port/store/"
    run_axe "$cache" refresh-tools >"$tmp/other-refresh.out" 2>"$tmp/other-refresh.err"
)
other_namespaces=("$cache"/metadata/*)
test "${#other_namespaces[@]}" -eq 2
other_namespace=
for namespace in "${other_namespaces[@]}"; do
    if test "$namespace" != "$metadata_namespace"; then
        other_namespace=$namespace
    fi
done
test -s "$other_namespace/index/content"

stop_server "$store_pid"
store_pid=
rm -rf "$tmp/recursion-cache" "$tmp/recursive-path"
mkdir "$tmp/recursive-path"
ln "$axe" "$tmp/recursive-path/rg" 2>/dev/null || ln -s "$axe" "$tmp/recursive-path/rg"
set +e
AXE_STORE_URL="$store_url" AXE_STORE_ADDRESSES=127.0.0.1 \
AXE_STORE_TRUSTED_KEY="$trusted_key" AXE_STORE_DIR="$tmp/recursion-cache" \
PATH="$tmp/recursive-path" \
    "$axe" --norc --noprofile --no-config -c 'rg; echo after' \
    >"$tmp/recursion.out" 2>"$tmp/recursion.err"
status=$?
set -e
test "$status" -eq 0
grep -q '^after$' "$tmp/recursion.out"
! grep -q 'using ' "$tmp/recursion.err"

mkdir -p "$cache/unrelated"
printf keep >"$cache/unrelated/data"
run_axe "$cache" clean-tools >"$tmp/clean.out" 2>"$tmp/clean.err"
test "$(cat "$tmp/clean.out")" = "$(printf '%s\nclean-tools: cleaned 1 cache root' "$cache")"
test ! -e "$metadata_namespace"
test -s "$other_namespace/index/content"
test -f "$cache/unrelated/data"
test -s "$object"
test -d "${package_entry%/tree/bin/pkg-smoke}"
test -d "$cache/locks"

echo "store smoke: ok"
