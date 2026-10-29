#!/usr/bin/env sh
# Build the freestanding Native loader: build.sh ARCH CONSTANTS_H OUTPUT
# ARCH is x86_64 or aarch64; CONSTANTS_H comes from bootstrap/generate.py.
set -eu

if [ "$#" -ne 3 ]; then
    printf 'usage: %s x86_64|aarch64 CONSTANTS_H OUTPUT\n' "$0" >&2
    exit 2
fi
arch=$1
constants=$2
output=$3
case $arch in
    x86_64) arch_flags='-mno-red-zone' ;;
    aarch64) arch_flags='-mcpu=generic -mno-outline-atomics' ;;
    *) printf '%s: unsupported architecture %s\n' "$0" "$arch" >&2; exit 2 ;;
esac
root=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cc=${AXE_BOOTSTRAP_CC:-zig cc}
umask 077
tmp=$(mktemp -d "${TMPDIR:-/tmp}/axe-bootstrap.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
cp "$constants" "$tmp/constants.h"

flags="-target $arch-linux-musl $arch_flags -std=c11 -Os -ffreestanding -fno-builtin -nostdinc -nostdlib
    -fno-stack-protector -fno-pie -fno-plt -fno-asynchronous-unwind-tables -fno-unwind-tables
    -ffunction-sections -fdata-sections -Wall -Wextra -Werror"

# shellcheck disable=SC2086
$cc $flags -I"$root/$arch" -I"$root" -I"$tmp" -c "$root/loader.c" -o "$tmp/loader.o"

# shellcheck disable=SC2086
$cc $flags -c "$root/$arch/start.S" -o "$tmp/start.o"

# shellcheck disable=SC2086
$cc $flags -fuse-ld=lld -Wl,-static -Wl,--gc-sections -Wl,--build-id=none -Wl,-z,noexecstack \
    -Wl,-z,max-page-size=4096 -Wl,-s -Wl,-T,"$root/linker.ld" \
    -o "$tmp/axe-bootstrap" "$tmp/start.o" "$tmp/loader.o"

# Release contract: a static ET_EXEC without INTERP or DT_NEEDED.
fail() {
    printf '%s: %s\n' "$0" "$1" >&2
    exit 1
}

readelf -hW "$tmp/axe-bootstrap" | grep -Eq '^[[:space:]]*Type:[[:space:]]*EXEC' ||
    fail 'loader is not an ET_EXEC ELF'
if readelf -lW "$tmp/axe-bootstrap" | grep -Eq '^[[:space:]]*INTERP[[:space:]]'; then
    fail 'loader has an INTERP program header'
fi
if readelf -dW "$tmp/axe-bootstrap" | grep -q '(NEEDED)'; then
    fail 'loader has DT_NEEDED entries'
fi

mkdir -p "$(dirname -- "$output")"
chmod 0755 "$tmp/axe-bootstrap"
mv -f "$tmp/axe-bootstrap" "$output"
