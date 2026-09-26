#!/usr/bin/env bash
set -euo pipefail

if (($# != 2)); then
    echo 'usage: relay-live-smoke.sh AXE_BIN AXE_RELAY_BIN' >&2
    exit 2
fi
axe=$1
relay=$2
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
keys="${AXE_EDITION_ROOT:-$root}/keys/relay"
scratch=$(mktemp -d)
relay_pid=
sshd_pid=
daemon_pid=
watch_pid=
cleanup() {
    result=$?
    trap - EXIT
    if [[ -n "$watch_pid" ]]; then kill "$watch_pid" 2>/dev/null || :; wait "$watch_pid" 2>/dev/null || :; fi
    if [[ -n "$daemon_pid" ]]; then kill "$daemon_pid" 2>/dev/null || :; fi
    if [[ -n "$sshd_pid" ]]; then kill "$sshd_pid" 2>/dev/null || :; wait "$sshd_pid" 2>/dev/null || :; fi
    if [[ -n "$relay_pid" ]]; then kill "$relay_pid" 2>/dev/null || :; wait "$relay_pid" 2>/dev/null || :; fi
    if ((result != 0)); then
        echo 'relay logs:' >&2
        cat "$scratch/relay.log" >&2
        for log in "$scratch"/sshd-*.log; do
            if [[ -f "$log" ]]; then echo "$log:" >&2; cat "$log" >&2; fi
        done
        if [[ -f "$scratch/watch.jsonl" ]]; then cat "$scratch/watch.jsonl" >&2; fi
        if [[ -f "$scratch/watch.err" ]]; then cat "$scratch/watch.err" >&2; fi
    fi
    rm -rf -- "$scratch"
    exit "$result"
}
trap cleanup EXIT

wait_watch_event() {
    python3 - "$scratch/watch.jsonl" "$1" "$2" "$3" <<'PY'
import json
import pathlib
import sys
import time
path, kind, client_id, address = sys.argv[1:]
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    if pathlib.Path(path).exists():
        for line in pathlib.Path(path).read_text().splitlines(keepends=True):
            if not line.endswith('\n'):
                continue
            event = json.loads(line)
            if (event["kind"] == kind
                    and event["client"]["client_id"] == client_id
                    and event["client"]["public_address"] == address):
                raise SystemExit(0)
    time.sleep(.05)
raise SystemExit(f'watch did not report {kind} for {client_id} at {address}')
PY
}

default_client_id() {
    python3 - <<'PY'
import os
import pwd
import socket
try:
    pidns = os.stat('/proc/self/ns/pid').st_ino
except OSError:
    pidns = -1
try:
    username = pwd.getpwuid(os.geteuid()).pw_name or str(os.geteuid())
except (KeyError, OSError):
    username = str(os.geteuid())
hostname = socket.gethostname() or 'unknown'
print(f'{pidns}@{username}@{hostname}')
PY
}

# Bind several sockets simultaneously so the selected ports are distinct.
ports=$(python3 - <<'PY'
import socket
sockets = [socket.socket(socket.AF_INET, socket.SOCK_STREAM) for _ in range(5)]
for listener in sockets:
    listener.bind(('127.0.0.1', 0))
print(*(listener.getsockname()[1] for listener in sockets))
for listener in sockets:
    listener.close()
PY
)
read -r api_port tcp_port quic_port ssh_port public_port <<< "$ports"
token=0123456789abcdef0123456789abcdef
AXE_RELAY_TOKEN="$token" "$relay" \
    --http "127.0.0.1:$api_port" \
    --tcp-control "127.0.0.1:$tcp_port" \
    --quic-control "127.0.0.1:$quic_port" \
    --public-bind 127.0.0.1 --public-host 127.0.0.1 \
    --min-port "$public_port" --max-port "$public_port" \
    --quic-key "$keys/quic_server_key.pem" \
    --quic-server-cert "$keys/quic_server_cert.pem" \
    --quic-client-cert "$keys/quic_client_cert.pem" \
    >"$scratch/relay.log" 2>&1 &
relay_pid=$!
api="http://127.0.0.1:$api_port"
ready=0
for ((attempt = 0; attempt < 100; attempt++)); do
    if "$relay" --api "$api" status >/dev/null 2>&1; then ready=1; break; fi
    if ! kill -0 "$relay_pid" 2>/dev/null; then echo 'relay exited before readiness' >&2; exit 1; fi
    sleep 0.1
done
if ((ready != 1)); then echo 'relay dashboard did not become ready' >&2; exit 1; fi

for transport in tcp quic; do
    mkdir "$scratch/work-$transport"
    client_id="live-$transport-$$"
    relay_id_args=(--relay-id "$client_id")
    if [[ "$transport" == tcp ]]; then relay_id_args=(); fi
    endpoint="127.0.0.1:$tcp_port"
    if [[ "$transport" == quic ]]; then endpoint="127.0.0.1:$quic_port"; fi
    AXE_RELAY_TOKEN="$token" HOSTNAME=spoofed USER=spoofed LOGNAME=spoofed "$axe" --applet sshd -- \
        --listen "127.0.0.1:$ssh_port" \
        --workdir "$scratch/work-$transport" --store-mode off \
        --relay "$endpoint" --relay-transport "$transport" "${relay_id_args[@]}" \
        --relay-quic-server-cert "$keys/quic_server_cert.pem" \
        --relay-quic-client-cert "$keys/quic_client_cert.pem" \
        --relay-quic-client-key "$keys/quic_client_key.pem" \
        >"$scratch/sshd-$transport.log" 2>&1 &
    sshd_pid=$!
    if [[ "$transport" == tcp ]]; then client_id=$(default_client_id); fi
    public_address=$("$relay" --api "$api" wait --client-id "$client_id" --transport "$transport" --timeout 20)
    if [[ "$transport" == tcp ]]; then
        "$relay" --api "$api" watch --json >"$scratch/watch.jsonl" 2>"$scratch/watch.err" &
        watch_pid=$!
        wait_watch_event present "$client_id" "$public_address"
    else
        wait_watch_event connected "$client_id" "$public_address"
    fi
    public_port=${public_address##*:}
    python3 - "$public_port" <<'PY'
import socket
import sys
with socket.create_connection(('127.0.0.1', int(sys.argv[1])), timeout=5) as peer:
    peer.settimeout(5)
    banner = peer.recv(128)
    if not banner.startswith(b'SSH-2.0-axe'):
        raise SystemExit(f'wrong SSH banner through relay: {banner!r}')
PY
    printf '%s: registered %s at %s; SSH banner forwarded\n' "$transport" "$client_id" "$public_address"
    kill "$sshd_pid"
    wait "$sshd_pid" 2>/dev/null || :
    sshd_pid=
    wait_watch_event disconnected "$client_id" "$public_address"
done

# A supervised daemon shares the launcher's PID namespace, so its default ID is
# identical and survives worker restarts; the launcher itself never registers.
mkdir "$scratch/work-daemon"
daemon_output=$(AXE_RELAY_TOKEN="$token" HOSTNAME=spoofed USER=spoofed LOGNAME=spoofed "$axe" --applet sshd -- \
    --listen "127.0.0.1:$ssh_port" --workdir "$scratch/work-daemon" --store-mode off \
    --relay "127.0.0.1:$tcp_port" --daemon --log-file "$scratch/sshd-daemon.log")
daemon_pid=${daemon_output##* }
client_id=$(default_client_id)
public_address=$("$relay" --api "$api" wait --client-id "$client_id" --transport tcp --timeout 20)
wait_watch_event connected "$client_id" "$public_address"
printf 'daemon: worker registered %s at %s\n' "$client_id" "$public_address"
kill "$daemon_pid"
wait_watch_event disconnected "$client_id" "$public_address"
daemon_pid=
