#!/usr/bin/env python3
"""Render AXE bootstrap programs from a release bundle.

    python3 bootstrap/generate.py BUNDLE.json OUTPUT_DIR

The bundle is the object built by tools/release.py::bootstrap_bundle:

    {"version": "0.11.0", "default_args": [],
     "targets": {"x86_64-linux": {"host": ..., "path": ..., "ports": {...},
                                  "addresses": [...], "size": ...,
                                  "sha256": ..., "ascon_hash256": ...}}}

OUTPUT_DIR receives the universal scripts `sh`, `py`, `js`, `pl` and one
`<target>.h` constants header per target for bootstrap/native/build.sh.
Output is deterministic. Exit status: 0 on success, 1 on an invalid bundle or
I/O error, 2 on invalid usage.
"""

from __future__ import annotations

import ipaddress
import json
import re
import sys
from pathlib import Path
from typing import Any

TEMPLATES = Path(__file__).resolve().parent / "templates"
# Target name -> `uname -m` spellings accepted by the scripts.
MACHINES = {"x86_64-linux": ("x86_64", "amd64"), "aarch64-linux": ("aarch64", "arm64")}
MAX_DEFAULT_ARGS = 8
SCRIPTS = {"sh": "shell.sh.in", "py": "python.py.in", "js": "node.js.in", "pl": "perl.pl.in"}

_VERSION = re.compile(r"[0-9A-Za-z.+-]+")
_HOST = re.compile(r"[A-Za-z0-9.-]+")
# Printable ASCII without space, quotes, backslash, or fragment: every value is
# embedded in shell, C, Perl, and JavaScript literals.
_PATH = re.compile(r"/[!$%&()*+,\-./0-9:;=?@A-Z_a-z~]*")
_ARG = re.compile(r"[ -~]*")
_HEX64 = re.compile(r"[0-9a-f]{64}")


class BundleError(ValueError):
    """The release bundle cannot be rendered safely."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise BundleError(message)


def _port(value: object, name: str) -> int:
    _require(type(value) is int and 1 <= value <= 65535, f"{name} must be a TCP port")
    return value


def _addresses(value: object, name: str) -> list[str]:
    _require(isinstance(value, list) and bool(value), f"{name} must be a non-empty array")

    parsed = []
    for item in value:
        _require(isinstance(item, str), f"{name} entries must be strings")
        try:
            parsed.append(ipaddress.ip_address(item))
        except ValueError as error:
            raise BundleError(f"{name}: {error}") from error
    _require(len(set(parsed)) == len(parsed), f"{name} contains duplicates")

    # IPv6 first, input order within a family.
    return [str(address) for address in sorted(parsed, key=lambda address: -address.version)]


def _target(manifest: object, name: str) -> dict[str, Any]:
    _require(isinstance(manifest, dict), f"{name} must be an object")
    keys = {"host", "path", "ports", "addresses", "size", "sha256", "ascon_hash256"}
    _require(set(manifest) == keys, f"{name} needs exactly {', '.join(sorted(keys))}")

    host = manifest["host"]
    _require(isinstance(host, str) and _HOST.fullmatch(host) is not None, f"{name}.host is invalid")

    path = manifest["path"]
    _require(isinstance(path, str) and _PATH.fullmatch(path) is not None, f"{name}.path is invalid")

    ports = manifest["ports"]
    _require(isinstance(ports, dict) and set(ports) == {"https", "http"}, f"{name}.ports needs https and http")

    size = manifest["size"]
    _require(type(size) is int and size > 0, f"{name}.size must be a positive integer")

    for digest in ("sha256", "ascon_hash256"):
        value = manifest[digest]
        _require(isinstance(value, str) and _HEX64.fullmatch(value) is not None, f"{name}.{digest} is invalid")

    return {
        "host": host.lower(),
        "path": path,
        "ports": {scheme: _port(ports[scheme], f"{name}.ports.{scheme}") for scheme in ("https", "http")},
        "addresses": _addresses(manifest["addresses"], f"{name}.addresses"),
        "size": size,
        "sha256": manifest["sha256"],
        "ascon_hash256": manifest["ascon_hash256"],
    }


def validate(bundle: object) -> dict[str, Any]:
    """Return the canonical bundle or raise BundleError."""

    _require(isinstance(bundle, dict), "bundle must be an object")
    _require(
        set(bundle) == {"version", "default_args", "targets"},
        "bundle needs exactly version, default_args, targets",
    )

    version = bundle["version"]
    _require(isinstance(version, str) and _VERSION.fullmatch(version) is not None, "invalid version")

    default_args = bundle["default_args"]
    _require(
        isinstance(default_args, list)
        and len(default_args) <= MAX_DEFAULT_ARGS
        and all(isinstance(item, str) and _ARG.fullmatch(item) for item in default_args),
        f"default_args must hold at most {MAX_DEFAULT_ARGS} printable ASCII strings",
    )

    targets = bundle["targets"]
    _require(isinstance(targets, dict) and bool(targets), "targets must be a non-empty object")
    for target in targets:
        _require(target in MACHINES, f"unsupported target {target}")

    return {
        "version": version,
        "default_args": list(default_args),
        "targets": {target: _target(targets[target], f"targets.{target}") for target in sorted(targets)},
    }


def _shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


def _perl_literal(value: object) -> str:
    if isinstance(value, str):
        return "'" + value.replace("\\", "\\\\").replace("'", "\\'") + "'"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, list):
        return "[" + ", ".join(_perl_literal(item) for item in value) + "]"
    if isinstance(value, dict):
        return "{" + ", ".join(f"{_perl_literal(key)} => {_perl_literal(item)}" for key, item in value.items()) + "}"
    raise TypeError(f"unsupported Perl value {type(value).__name__}")


def _script_data(bundle: dict[str, Any]) -> dict[str, Any]:
    """The subset embedded in scripts; Ascon is verified only by Native."""

    return {
        "default_args": bundle["default_args"],
        "targets": {
            target: {key: value for key, value in manifest.items() if key != "ascon_hash256"}
            for target, manifest in bundle["targets"].items()
        },
    }


def _shell_targets(bundle: dict[str, Any]) -> str:
    blocks = []
    for target, manifest in bundle["targets"].items():
        assignments = {
            "TARGET": target,
            "HOST": manifest["host"],
            "HTTPS_PORT": str(manifest["ports"]["https"]),
            "HTTP_PORT": str(manifest["ports"]["http"]),
            "URL_PATH": manifest["path"],
            "ADDRESSES": " ".join(manifest["addresses"]),
            "SIZE": str(manifest["size"]),
            "SHA256": manifest["sha256"],
        }
        lines = [f"    {'|'.join((*MACHINES[target], target))})"]
        lines.extend(f"        {name}={_shell_quote(value)}" for name, value in assignments.items())
        lines.append("        ;;")
        blocks.append("\n".join(lines))
    return "\n".join(blocks)


def _render(template: str, replacements: dict[str, str]) -> bytes:
    text = (TEMPLATES / template).read_text(encoding="utf-8")
    for marker, value in replacements.items():
        text = text.replace(f"@@{marker}@@", value)
    unresolved = re.findall(r"@@[A-Z_]+@@", text)
    if unresolved:
        raise RuntimeError(f"{template}: unresolved markers {', '.join(unresolved)}")
    return text.encode("ascii")


def render_scripts(bundle: dict[str, Any]) -> dict[str, bytes]:
    """Render the universal `sh`, `py`, `js`, and `pl` programs."""

    data = _script_data(bundle)
    data_json = json.dumps(data, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
    default_args = " ".join(_shell_quote(argument) for argument in bundle["default_args"])
    return {
        "sh": _render("shell.sh.in", {"TARGETS": _shell_targets(bundle), "DEFAULT_ARGS": default_args}),
        "py": _render("python.py.in", {"DATA": data_json}),
        "js": _render("node.js.in", {"DATA": data_json}),
        "pl": _render("perl.pl.in", {"DATA": _perl_literal(data)}),
    }


def _bytes(value: bytes) -> str:
    return ", ".join(f"0x{byte:02x}" for byte in value)


def _c_string(value: str) -> str:
    # Values are printable ASCII; `?` is escaped to rule out trigraphs.
    escaped = value.replace("\\", "\\\\").replace('"', '\\"').replace("?", "\\?")
    return f'"{escaped}"'


def native_header(bundle: dict[str, Any], target: str) -> str:
    """Return the constants header compiled into the Native loader."""

    manifest = bundle["targets"][target]
    addresses = [ipaddress.ip_address(address) for address in manifest["addresses"]]
    entries = "".join(
        f"    {{{10 if address.version == 6 else 2}, {{{_bytes(address.packed.ljust(16, b'\0'))}}}}},\n"
        for address in addresses
    )
    arguments = "".join(f"    (char *){_c_string(argument)},\n" for argument in bundle["default_args"])
    return (
        "#ifndef AXE_BOOTSTRAP_CONSTANTS_H\n"
        "#define AXE_BOOTSTRAP_CONSTANTS_H\n\n"
        f"#define AXE_HTTP_HOST {_c_string(manifest['host'])}\n"
        f"#define AXE_HTTP_PATH {_c_string(manifest['path'])}\n"
        f"#define AXE_HTTP_PORT {manifest['ports']['http']}\n"
        f"#define AXE_PAYLOAD_SIZE ((axe_usize){manifest['size']}UL)\n"
        f"#define AXE_ADDRESS_COUNT {len(addresses)}\n"
        f"#define AXE_DEFAULT_ARGC {len(bundle['default_args'])}\n\n"
        "static const struct axe_address axe_bootstrap_addresses[AXE_ADDRESS_COUNT] = {\n"
        f"{entries}}};\n"
        f"static const axe_u8 axe_bootstrap_digest[32] = {{{_bytes(bytes.fromhex(manifest['ascon_hash256']))}}};\n"
        "static char *const axe_bootstrap_default_args[AXE_DEFAULT_ARGC + 1] = {\n"
        f"{arguments}    (char *)0,\n}};\n\n"
        "#endif\n"
    )


def generate(bundle: object, output: Path) -> dict[str, Any]:
    """Validate *bundle* and write every generated file into *output*."""

    canonical = validate(bundle)
    output.mkdir(parents=True, exist_ok=True)
    for name, data in render_scripts(canonical).items():
        (output / name).write_bytes(data)
        (output / name).chmod(0o755)
    for target in canonical["targets"]:
        (output / f"{target}.h").write_text(native_header(canonical, target), encoding="ascii")
    return canonical


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: generate.py BUNDLE.json OUTPUT_DIR", file=sys.stderr)
        return 2
    try:
        generate(json.loads(Path(argv[0]).read_text(encoding="utf-8")), Path(argv[1]))
    except (OSError, ValueError, RuntimeError) as error:
        print(f"bootstrap generator: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
