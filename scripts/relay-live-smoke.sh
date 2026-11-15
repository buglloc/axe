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
scratch=$(umask 077; mktemp -d)
relay_pid=
sshd_pid=
daemon_pid=
watch_pid=
echo_pid=
hold_pid=
watchdog_pid=
cleanup() {
    result=$?
    trap - EXIT
    for pid in "$watch_pid" "$daemon_pid" "$sshd_pid" "$relay_pid" "$echo_pid" "$hold_pid" "$watchdog_pid"; do
        if [[ -n "$pid" ]]; then kill -KILL "$pid" 2>/dev/null || :; wait "$pid" 2>/dev/null || :; fi
    done
    if ((result != 0)); then
        if [[ -f "$scratch/relay.log" ]]; then
            echo 'relay logs:' >&2
            cat "$scratch/relay.log" >&2
        fi
        for log in "$scratch"/sshd-*.log "$scratch/echo.log"; do
            if [[ -f "$log" ]]; then echo "$log:" >&2; cat "$log" >&2; fi
        done
        if [[ -f "$scratch/watch.jsonl" ]]; then cat "$scratch/watch.jsonl" >&2; fi
        if [[ -f "$scratch/watch.err" ]]; then cat "$scratch/watch.err" >&2; fi
    fi
    rm -rf -- "$scratch"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 124' TERM
trap 'exit 130' INT
python3 - "$$" <<'PY' &
import os
import signal
import sys
import time
time.sleep(240)
print("relay live smoke exceeded its 240s deadline", file=sys.stderr, flush=True)
os.kill(int(sys.argv[1]), signal.SIGTERM)
PY
watchdog_pid=$!

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
token=relay-live-smoke-token-012345678901234567
printf '%s' "$token" >"$scratch/token"
export AXE_RELAY_API_URL="http://127.0.0.1:$api_port"
export AXE_RELAY_API_TOKEN="$token"
python3 - "$scratch/config.json" "$scratch/token" "$keys" "$api_port" "$tcp_port" "$quic_port" "$public_port" <<'PY'
import json
import pathlib
import sys

config_path, token_file, keys, api_port, tcp_port, quic_port, public_port = sys.argv[1:]
config = {
    "tcp_control": f"127.0.0.1:{tcp_port}",
    "quic_control": f"127.0.0.1:{quic_port}",
    "quic_key": str(pathlib.Path(keys) / "quic_server_key.pem"),
    "quic_server_cert": str(pathlib.Path(keys) / "quic_server_cert.pem"),
    "quic_client_cert": str(pathlib.Path(keys) / "quic_client_cert.pem"),
    "public_bind": "127.0.0.1",
    "public_host": "127.0.0.1",
    "min_port": int(public_port),
    "max_port": int(public_port),
    "http": f"127.0.0.1:{api_port}",
    "token_file": token_file,
}
pathlib.Path(config_path).write_text(json.dumps(config) + "\n")
PY
"$relay" --config "$scratch/config.json" >"$scratch/relay.log" 2>&1 &
relay_pid=$!
api="http://127.0.0.1:$api_port"
ready=0
for ((attempt = 0; attempt < 100; attempt++)); do
    if "$relay" status >/dev/null 2>&1; then ready=1; break; fi
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
    public_address=$("$relay" wait --client-id "$client_id" --transport "$transport" --timeout 20)
    if [[ "$transport" == tcp ]]; then
        "$relay" watch --json >"$scratch/watch.jsonl" 2>"$scratch/watch.err" &
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
public_address=$("$relay" wait --client-id "$client_id" --transport tcp --timeout 20)
wait_watch_event connected "$client_id" "$public_address"
printf 'daemon: worker registered %s at %s\n' "$client_id" "$public_address"
kill "$daemon_pid"
wait_watch_event disconnected "$client_id" "$public_address"
daemon_pid=

kill "$watch_pid"
wait "$watch_pid" 2>/dev/null || :
watch_pid=

# A byte-for-byte local target exercises forwarding independently of SSH auth.
python3 - "$scratch" >"$scratch/echo.log" 2>&1 <<'PY' &
import asyncio
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
active = 0

def publish():
    temporary = root / "echo.stats.tmp"
    temporary.write_text(str(active))
    temporary.replace(root / "echo.stats")

async def echo(reader, writer):
    global active
    active += 1
    publish()
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, OSError):
        pass
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except (ConnectionError, OSError):
            pass
        active -= 1
        publish()

async def main():
    publish()
    server = await asyncio.start_server(echo, "127.0.0.1", 0)
    (root / "echo.ready").write_text(str(server.sockets[0].getsockname()[1]))
    async with server:
        await server.serve_forever()

asyncio.run(main())
PY
echo_pid=$!
for ((attempt = 0; attempt < 100; attempt++)); do
    if [[ -s "$scratch/echo.ready" ]]; then break; fi
    if ! kill -0 "$echo_pid" 2>/dev/null; then echo 'echo target exited' >&2; exit 1; fi
    sleep .05
done
if [[ ! -s "$scratch/echo.ready" ]]; then echo 'echo target did not become ready' >&2; exit 1; fi
echo_port=$(<"$scratch/echo.ready")

concurrent_echo() {
    python3 - "$1" <<'PY'
import asyncio
import sys

port = int(sys.argv[1])

async def exchange(sequence):
    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    payload = bytes([sequence]) * (256 * 1024)
    try:
        async def send():
            writer.write(payload)
            await writer.drain()
            writer.write_eof()
        async def receive():
            received = await reader.readexactly(len(payload))
            if received != payload or await reader.read(1) != b"":
                raise RuntimeError("relay corrupted data or lost half-close")
        await asyncio.gather(send(), receive())
    finally:
        writer.close()
        await writer.wait_closed()

async def main():
    # An idle tunnel must not block unrelated streams.
    idle_reader, idle_writer = await asyncio.open_connection("127.0.0.1", port)
    try:
        await asyncio.gather(*(exchange(sequence) for sequence in range(16)))
        idle_writer.write(b"still-alive")
        await idle_writer.drain()
        if await idle_reader.readexactly(11) != b"still-alive":
            raise RuntimeError("idle tunnel did not survive concurrent traffic")
    finally:
        idle_writer.close()
        await idle_writer.wait_closed()

asyncio.run(asyncio.wait_for(main(), timeout=20))
PY
}

wait_recovery_cleanup() {
    python3 - "$scratch" "$public_port" <<'PY'
import json
import os
import pathlib
import socket
import sys
import time
import urllib.request

root, port = pathlib.Path(sys.argv[1]), int(sys.argv[2])
deadline = time.monotonic() + 10
request = urllib.request.Request(
    os.environ["AXE_RELAY_API_URL"] + "/api/v1/status",
    headers={"Authorization": "Bearer " + os.environ["AXE_RELAY_API_TOKEN"]},
)
while time.monotonic() < deadline:
    with urllib.request.urlopen(request, timeout=2) as response:
        clients = json.load(response)["clients"]
    if not clients and (root / "echo.stats").read_text() == "0":
        try:
            with socket.socket() as listener:
                listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                listener.bind(("127.0.0.1", port))
            raise SystemExit(0)
        except OSError:
            pass
    time.sleep(.05)
raise SystemExit("registration, target tunnel, or public listener leaked after shutdown")
PY
}

wait_recovery_cleanup

for transport in tcp quic; do
    endpoint="127.0.0.1:$tcp_port"
    if [[ "$transport" == quic ]]; then endpoint="127.0.0.1:$quic_port"; fi
    client_id="recovery-$transport-$$"
    AXE_RELAY_TOKEN="$token" "$axe" --applet sshd -- \
        --listen "127.0.0.1:$ssh_port" --workdir "$scratch/work-$transport" --store-mode off \
        --relay "$endpoint" --relay-transport "$transport" --relay-id "$client_id" \
        --relay-target "127.0.0.1:$echo_port" \
        --relay-quic-server-cert "$keys/quic_server_cert.pem" \
        --relay-quic-client-cert "$keys/quic_client_cert.pem" \
        --relay-quic-client-key "$keys/quic_client_key.pem" \
        >>"$scratch/sshd-$transport.log" 2>&1 &
    sshd_pid=$!
    public_address=$("$relay" wait --client-id "$client_id" --transport "$transport" --timeout 20)
    public_port=${public_address##*:}
    concurrent_echo "$public_port"
    rm -f "$scratch/hold.ready"
    python3 - "$public_port" "$scratch/hold.ready" <<'PY' &
import pathlib
import socket
import sys
with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=5) as peer:
    peer.settimeout(45)
    peer.sendall(b"hold")
    received = b""
    while len(received) < 4:
        chunk = peer.recv(4 - len(received))
        if not chunk:
            raise SystemExit("held tunnel closed before relay crash")
        received += chunk
    if received != b"hold":
        raise SystemExit("held tunnel corrupted data")
    pathlib.Path(sys.argv[2]).write_text("ready")
    try:
        if peer.recv(1):
            raise SystemExit("unexpected data on held tunnel")
    except ConnectionResetError:
        pass
PY
    hold_pid=$!
    for ((attempt = 0; attempt < 100; attempt++)); do
        if [[ -s "$scratch/hold.ready" ]]; then break; fi
        if ! kill -0 "$hold_pid" 2>/dev/null; then echo 'held tunnel exited early' >&2; exit 1; fi
        sleep .05
    done
    if [[ ! -s "$scratch/hold.ready" ]]; then echo 'held tunnel did not become ready' >&2; exit 1; fi
    kill -KILL "$relay_pid"
    wait "$relay_pid" 2>/dev/null || :
    relay_pid=
    "$relay" --config "$scratch/config.json" >>"$scratch/relay.log" 2>&1 &
    relay_pid=$!
    ready=0
    for ((attempt = 0; attempt < 100; attempt++)); do
        if "$relay" status >/dev/null 2>&1; then ready=1; break; fi
        if ! kill -0 "$relay_pid" 2>/dev/null; then echo 'restarted relay exited' >&2; exit 1; fi
        sleep .1
    done
    if ((ready != 1)); then echo 'restarted relay did not become ready' >&2; exit 1; fi
    recovered=$("$relay" wait --client-id "$client_id" --transport "$transport" --timeout 45)
    wait "$hold_pid"
    hold_pid=
    concurrent_echo "${recovered##*:}"
    kill "$sshd_pid"
    wait "$sshd_pid" 2>/dev/null || :
    sshd_pid=
    wait_recovery_cleanup
    printf '%s: recovered after relay crash; concurrent data, half-close and cleanup passed\n' "$transport"
done
