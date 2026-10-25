#!/bin/sh
set -u

root=${1:-.}
vendor=$root/vendor
status=0

fail() {
    printf '%s\n' "check-vendor: $*" >&2
    status=1
}

package_field() {
    field=$1
    awk -v field="$field" '
        $0 == "[package]" { in_package = 1; next }
        in_package && /^\[/ { exit }
        in_package && $1 == field && $2 == "=" {
            value = $3
            gsub(/"/, "", value)
            print value
            exit
        }
    ' "$2"
}

for dir in "$vendor"/*; do
    [ -d "$dir" ] || continue
    name=${dir##*/}
    manifest=$dir/Cargo.toml
    patches=$dir/PATCHES.md

    if [ ! -f "$patches" ]; then
        fail "$name: missing PATCHES.md"
    fi
    if [ ! -f "$manifest" ]; then
        fail "$name: missing Cargo.toml"
    else
        package_name=$(package_field name "$manifest")
        package_version=$(package_field version "$manifest")
        [ -n "$package_name" ] || fail "$name: Cargo.toml has no [package] name"
        [ -n "$package_version" ] || fail "$name: Cargo.toml has no [package] version"
        if [ -f "$patches" ] && [ -n "$package_name" ] && [ -n "$package_version" ]; then
            source=$(sed -n 's/^Source: //p' "$patches" | head -n 1)
            case "$source" in
                *"$package_name"*"$package_version"*) ;;
                *) fail "$name: PATCHES.md Source does not mention $package_name $package_version" ;;
            esac
            vcs=$dir/.cargo_vcs_info.json
            if [ -f "$vcs" ]; then
                sha=$(sed -n 's/.*"sha1"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F][0-9a-fA-F]*\)".*/\1/p' "$vcs" | head -n 1)
                if [ -n "$sha" ]; then
                    case "$(cat "$patches")" in
                        *"$sha"*) ;;
                        *) fail "$name: PATCHES.md does not record .cargo_vcs_info.json revision $sha" ;;
                    esac
                fi
            fi
        fi
    fi

    if ! find "$dir" -type d -name target -print -quit | grep . >/dev/null 2>&1; then
        :
    else
        fail "$name: target/ build directory present"
    fi
    junk=$(find "$dir" -type f \( \
        -name '*.o' -o -name '*.a' -o -name '*.so' -o -name 'Cargo.lock' -o \
        -name '*.rej' -o -name '*~' -o -name '*.swp' -o -name '*.swo' -o \
        -name '.DS_Store' -o -name '*.bak' -o -name '*.tmp' \
    \) -print)
    if [ -n "$junk" ]; then
        printf '%s\n' "$junk" | while IFS= read -r path; do
            printf '%s\n' "check-vendor: $name: forbidden vendored artifact $path" >&2
        done
        status=1
    fi
    bad_backups=$(find "$dir" -type f -name '*.orig' ! -name Cargo.toml.orig -print)
    if [ -n "$bad_backups" ]; then
        printf '%s\n' "$bad_backups" | while IFS= read -r path; do
            printf '%s\n' "check-vendor: $name: forbidden backup file $path" >&2
        done
        status=1
    fi

    if ! find "$dir" -maxdepth 1 -type f -name 'LICENSE*' -print -quit | grep . >/dev/null 2>&1; then
        fail "$name: no top-level LICENSE* file"
    fi
done

exit "$status"
