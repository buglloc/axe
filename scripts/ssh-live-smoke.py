#!/usr/bin/env python3
"""Local OpenSSH/AXE interoperability smoke; builds are deliberately external.

Prepare an isolated build root:
  python3 scripts/ssh-live-smoke.py prepare-edition /tmp/axe-ssh-edition
Run a binary built with AXE_EDITION_ROOT pointing there:
  python3 scripts/ssh-live-smoke.py run --axe /tmp/axe-ssh-bin \
      --identity-config /tmp/axe-ssh-edition/smoke-identity.json

Requires Linux /proc, Python 3.11+, OpenSSH ssh/sftp/ssh-add/ssh-keygen and an
existing populated SSH_AUTH_SOCK. Only public agent identities are read. The
edition contains a freshly generated disposable host key, not a user key.
Prepare output is caller-owned; run output and processes are temporary-owned.
SSH signal requests and env requests after exec require the russh unit tests:
OpenSSH's CLI cannot issue these requests. This script checks actual terminal
modes/resize, signal exit status, process-group cleanup and SFTP wire replies.
"""

from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from collections.abc import Callable


class Failure(RuntimeError):
    pass


def require(condition: object, message: str) -> None:
    if not condition:
        raise Failure(message)


def wait_for(predicate: Callable[[], object], label: str, timeout: float = 10) -> object:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.025)
    raise Failure(f"deadline waiting for {label}")


def command(argv: list[str], *, data: bytes | None = None,
            env: dict[str, str] | None = None, timeout: float = 20,
            expected: int = 0) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(argv, input=data, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, env=env, timeout=timeout)
    require(result.returncode == expected,
            f"{shlex.join(argv)}: exit {result.returncode}, expected {expected}\n"
            + result.stderr.decode(errors="replace"))
    return result


def string(value: bytes | str) -> bytes:
    if isinstance(value, str):
        value = value.encode()
    return struct.pack(">I", len(value)) + value


class Wire:
    def __init__(self, data: bytes):
        self.data = data
        self.offset = 0

    def take(self, count: int) -> bytes:
        require(0 <= count <= len(self.data) - self.offset, "truncated SSH/SFTP packet")
        value = self.data[self.offset:self.offset + count]
        self.offset += count
        return value

    def u32(self) -> int:
        return struct.unpack(">I", self.take(4))[0]

    def u64(self) -> int:
        return struct.unpack(">Q", self.take(8))[0]

    def string(self) -> bytes:
        return self.take(self.u32())

    def done(self) -> None:
        require(self.offset == len(self.data), "trailing SSH/SFTP packet data")


def certificate(line: str) -> dict[str, object]:
    fields = line.split()
    require(len(fields) >= 2, "invalid public identity")
    raw = base64.b64decode(fields[1], validate=True)
    wire = Wire(raw)
    kind = wire.string().decode()
    require(kind.endswith("-cert-v01@openssh.com"), "identity is not an OpenSSH certificate")
    wire.string()  # nonce
    bare_kind = kind.removesuffix("-cert-v01@openssh.com")
    if bare_kind == "ssh-ed25519":
        key_fields = [wire.string()]
    elif bare_kind.startswith("ecdsa-sha2-"):
        key_fields = [wire.string(), wire.string()]
    elif bare_kind == "ssh-rsa":
        key_fields = [wire.string(), wire.string()]
    else:
        raise Failure(f"unsupported agent certificate type: {kind}")
    bare = string(bare_kind) + b"".join(string(field) for field in key_fields)
    wire.u64()  # serial
    require(wire.u32() == 1, "agent certificate is not a user certificate")
    wire.string()  # key id
    principal_wire = Wire(wire.string())
    principals = []
    while principal_wire.offset < len(principal_wire.data):
        principals.append(principal_wire.string().decode())
    valid_after, valid_before = wire.u64(), wire.u64()
    require(valid_after <= time.time() < valid_before, "agent certificate is expired or not yet valid")
    require(not wire.string(), "certificate has critical options not supported by AXE")
    extensions = Wire(wire.string())
    extension_names = []
    while extensions.offset < len(extensions.data):
        extension_names.append(extensions.string().decode())
        extensions.string()
    wire.string()  # reserved
    ca = wire.string()
    wire.string()  # signature
    wire.done()
    ca_kind = Wire(ca).string().decode()
    return {"certificate": f"{kind} {base64.b64encode(raw).decode()}",
            "public_key": f"{bare_kind} {base64.b64encode(bare).decode()}",
            "ca": f"{ca_kind} {base64.b64encode(ca).decode()}",
            "principals": principals, "extensions": extension_names}


def agent_lines() -> list[str]:
    sock = os.environ.get("SSH_AUTH_SOCK", "")
    require(sock and stat.S_ISSOCK(os.stat(sock).st_mode), "SSH_AUTH_SOCK must name the current agent")
    lines = command(["ssh-add", "-L"]).stdout.decode().splitlines()
    require(lines, "the current agent contains no identities")
    return lines


def prepare(args: argparse.Namespace) -> None:
    identities = agent_lines()
    candidates = []
    for line in identities:
        if "-cert-v01@openssh.com " not in line:
            continue
        try:
            parsed = certificate(line)
        except Failure as error:
            print(f"Ignoring unusable certificate: {error}", file=sys.stderr)
            continue
        if args.principal is None or args.principal in parsed["principals"]:
            candidates.append(parsed)
    require(candidates, "no current agent certificate matches the requested principal")
    identity = candidates[args.certificate_index] if args.certificate_index < len(candidates) else None
    require(identity is not None, "certificate index is out of range")
    principals = identity["principals"]
    require(isinstance(principals, list) and principals, "certificate must contain an explicit principal")
    principal = args.principal or principals[0]
    root = args.directory.absolute()
    require(not root.exists(), f"refusing to replace existing edition: {root}")
    root.mkdir(mode=0o700, parents=True)
    try:
        source = args.source.resolve()
        for name in ("config", "keys/ssh", "store"):
            (root / name).mkdir(parents=True, exist_ok=True)
        edition = json.loads((source / "config/edition.json").read_text())
        edition["id"] = "ssh-live-smoke"
        edition["sshd"] = {"principals": [principal]}
        edition["relay"] = {"enabled_by_default": False, "tcp_endpoint": None, "quic_endpoint": None}
        (root / "config/edition.json").write_text(json.dumps(edition, indent=2) + "\n")
        shutil.copyfile(source / "config/store.json", root / "config/store.json")
        shutil.copyfile(source / "config/bootstrap.json", root / "config/bootstrap.json")
        for name in ("store/trusted", "store/bootstrap.json", "store/bootstrap-index.cbor.zst"):
            if (source / name).exists():
                (root / name).symlink_to(source / name, target_is_directory=(source / name).is_dir())
        (root / "keys/ssh/user_ca_keys").write_text(str(identity["ca"]) + "\n")
        (root / "identity-cert.pub").write_text(str(identity["certificate"]) + "\n")
        (root / "identity.pub").write_text(str(identity["public_key"]) + "\n")
        command(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "ssh-live-smoke-host",
                 "-f", str(root / "keys/ssh/host_ed25519")])
        config = {"principal": principal, "certificate": identity["certificate"],
                  "public_key": identity["public_key"], "extensions": identity["extensions"]}
        (root / "smoke-identity.json").write_text(json.dumps(config, indent=2) + "\n")
    except BaseException:
        shutil.rmtree(root)
        raise
    print(f"AXE_EDITION_ROOT={shlex.quote(str(root))}")
    print(f"identity-config={root / 'smoke-identity.json'}")
    print("Build/copy the edition binary externally, then restore the public build; this command builds nothing.")


class Harness:
    def __init__(self, root: Path, args: argparse.Namespace):
        self.root = root
        self.args = args
        self.processes: list[subprocess.Popen[bytes]] = []
        self.logs: list[object] = []
        self.children: list[tuple[int, str]] = []
        self.port = self.free_port()
        self.identity = json.loads(args.identity_config.read_text())
        self.cert = root / "identity-cert.pub"
        self.cert.write_text(self.identity["certificate"] + "\n")
        self.public = root / "identity.pub"
        self.public.write_text(self.identity["public_key"] + "\n")
        self.python = str(Path(sys.executable).resolve())
        self.config = root / "ssh.conf"
        self.config.write_text("Host smoke\n"
            " HostName 127.0.0.1\n"
            f" Port {self.port}\n"
            f" User {self.identity['principal']}\n"
            " BatchMode yes\n IdentityAgent SSH_AUTH_SOCK\n"
            f' IdentityFile "{self.cert}"\n'
            " IdentitiesOnly yes\n PreferredAuthentications publickey\n"
            " StrictHostKeyChecking no\n UserKnownHostsFile /dev/null\n"
            " GlobalKnownHostsFile /dev/null\n CheckHostIP no\n UpdateHostKeys no\n"
            " VerifyHostKeyDNS no\n LogLevel ERROR\n ForwardAgent no\n ForwardX11 no\n"
            " ControlMaster no\n ControlPath none\n PermitLocalCommand no\n"
            " ConnectTimeout 5\n ConnectionAttempts 1\n ServerAliveInterval 2\n"
            " ServerAliveCountMax 3\n ExitOnForwardFailure yes\n StreamLocalBindUnlink no\n")
        self.work = root / "work"
        self.work.mkdir()

    @staticmethod
    def free_port() -> int:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            return sock.getsockname()[1]

    def spawn(self, argv: list[str], *, name: str, env: dict[str, str] | None = None,
              pipes: bool = False, **kwargs: object) -> subprocess.Popen[bytes]:
        log = (self.root / f"{name}.log").open("wb")
        self.logs.append(log)
        process = subprocess.Popen(argv, stdin=subprocess.PIPE if pipes else subprocess.DEVNULL,
                                   stdout=subprocess.PIPE if pipes else log, stderr=log,
                                   env=env, start_new_session=True, **kwargs)
        self.processes.append(process)
        return process

    @staticmethod
    def stop(process: subprocess.Popen[bytes]) -> None:
        if process.poll() is None:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=3)
        for pipe in (process.stdin, process.stdout, process.stderr):
            if pipe is not None:
                pipe.close()

    def close(self) -> None:
        pending = [process.pid for process in self.processes if process.poll() is None]
        seen: set[int] = set()
        while pending:
            pid = pending.pop()
            if pid in seen:
                continue
            seen.add(pid)
            start = self.pid_start(pid)
            if start is not None:
                self.children.append((pid, start))
            for children in Path(f"/proc/{pid}/task").glob("*/children"):
                try:
                    pending.extend(int(value) for value in children.read_text().split())
                except FileNotFoundError:
                    continue
        for process in reversed(self.processes):
            self.stop(process)
        for pid, start in self.children:
            if self.pid_start(pid) == start:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(pid, signal.SIGKILL)
        for log in self.logs:
            log.close()

    def ssh(self, remote: str | None = None, *options: str) -> list[str]:
        argv = ["ssh", "-F", str(self.config), *options, "smoke"]
        return argv if remote is None else argv + [remote]

    def py(self, code: str, *argv: object) -> str:
        return shlex.join([self.python, "-c", code, *(str(arg) for arg in argv)])

    def start(self, *, accept_env: str | None = None) -> None:
        env = os.environ.copy()
        for key in list(env):
            if key.startswith("AXE_") or key.startswith("LC_") or key in ("LD_PRELOAD", "LANG"):
                env.pop(key)
        env["LANG"] = "C"
        options = ["--listen", f"127.0.0.1:{self.port}", "--workdir", str(self.work),
                   "--store-mode", "off", "--no-relay"]
        if accept_env is not None:
            options += ["--accept-env", accept_env]
        self.server = self.spawn([str(self.args.axe.resolve()), "--applet", "sshd", "--", *options],
                                 name=f"sshd-{accept_env or 'default'}", env=env)
        def ready() -> bool:
            require(self.server.poll() is None, "sshd exited during startup (see sshd.log)")
            try:
                with socket.create_connection(("127.0.0.1", self.port), timeout=0.2) as sock:
                    return sock.recv(100).startswith(b"SSH-2.0-")
            except (ConnectionError, TimeoutError):
                return False
        wait_for(ready, "sshd banner")
        require(command(self.ssh("printf smoke-ready")).stdout == b"smoke-ready", "certificate exec mismatch")
        print("PASS certificate-only agent login", flush=True)

    def plain_key_rejection(self) -> None:
        if not any("-cert-v01@openssh.com" not in line.split()[0] for line in agent_lines()):
            print("SKIP plain-key rejection: current agent exposes certificates only", flush=True)
            return
        algorithms = "ssh-ed25519,ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521,rsa-sha2-512,rsa-sha2-256"
        result = command(self.ssh("printf forbidden-plain-key-login", "-o", "CertificateFile=none",
            "-o", "IdentitiesOnly=no", "-o", f"PubkeyAcceptedAlgorithms={algorithms}"), expected=255)
        require(not result.stdout and b"Permission denied" in result.stderr,
                "plain-key test failed for a reason other than authentication rejection")
        print("PASS current agent's bare identities rejected", flush=True)

    def forwarding(self) -> None:
        payload = bytes(range(256)) * 1281
        response = hashlib.sha256(payload).digest() + payload[::-1]
        for direction in ("L", "R"):
            for source_kind in ("tcp", "unix"):
                for destination_kind in ("tcp", "unix"):
                    label = f"{direction}-{source_kind}-{destination_kind}"
                    with socket.socket(socket.AF_INET if destination_kind == "tcp" else socket.AF_UNIX) as endpoint:
                        if destination_kind == "tcp":
                            endpoint.bind(("127.0.0.1", 0))
                            destination = f"127.0.0.1:{endpoint.getsockname()[1]}"
                        else:
                            destination = str(self.root / f"destination-{label}.sock")
                            endpoint.bind(destination)
                        endpoint.listen(1)
                        endpoint.settimeout(15)
                        errors: list[BaseException] = []
                        def serve() -> None:
                            try:
                                with endpoint.accept()[0] as stream:
                                    stream.settimeout(15)
                                    received = bytearray()
                                    while chunk := stream.recv(65536):
                                        received.extend(chunk)
                                        require(len(received) <= len(payload), f"{label}: excess destination bytes")
                                    require(received == payload, f"{label}: destination transfer mismatch")
                                    stream.sendall(response)
                                    stream.shutdown(socket.SHUT_WR)
                            except BaseException as error:
                                errors.append(error)
                        thread = threading.Thread(target=serve, daemon=True)
                        thread.start()
                        source = (f"127.0.0.1:{self.free_port()}" if source_kind == "tcp"
                                  else str(self.root / f"source-{label}.sock"))
                        tunnel = self.spawn(self.ssh(None, "-N", f"-{direction}", f"{source}:{destination}"), name=label)
                        def connect() -> socket.socket | None:
                            require(tunnel.poll() is None, f"{label}: SSH tunnel exited")
                            stream = socket.socket(socket.AF_INET if source_kind == "tcp" else socket.AF_UNIX)
                            stream.settimeout(15)
                            try:
                                stream.connect(("127.0.0.1", int(source.rsplit(":", 1)[1]))
                                               if source_kind == "tcp" else source)
                                return stream
                            except (ConnectionError, FileNotFoundError):
                                stream.close()
                                return None
                        stream = wait_for(connect, f"{label} listener")
                        with stream:
                            stream.sendall(payload)
                            stream.shutdown(socket.SHUT_WR)
                            received = bytearray()
                            while chunk := stream.recv(65536):
                                received.extend(chunk)
                                require(len(received) <= len(response), f"{label}: excess response bytes")
                        thread.join(timeout=15)
                        require(not thread.is_alive(), f"{label}: destination did not finish")
                        if errors:
                            raise errors[0]
                        require(received == response, f"{label}: response/half-close mismatch")
                        self.stop(tunnel)
                        if source_kind == "tcp":
                            self.wait_tcp_closed(int(source.rsplit(":", 1)[1]))
                        elif direction == "R":
                            wait_for(lambda: not Path(source).exists(), f"{label} server socket removal")
                    print(f"PASS forwarding {label}: {len(payload)} bytes + half-close", flush=True)
        self.socks(payload)

    def wait_tcp_closed(self, port: int) -> None:
        def closed() -> bool:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                    return False
            except ConnectionError:
                return True
        wait_for(closed, f"port {port} removal")

    def socks(self, payload: bytes) -> None:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            listener.settimeout(15)
            target = listener.getsockname()[1]
            errors: list[BaseException] = []
            def serve() -> None:
                try:
                    with listener.accept()[0] as peer:
                        peer.settimeout(15)
                        data = bytearray()
                        while chunk := peer.recv(65536):
                            data.extend(chunk)
                            require(len(data) <= len(payload), "SOCKS excess destination bytes")
                        require(data == payload, "SOCKS destination payload mismatch")
                        peer.sendall(hashlib.sha256(data).digest())
                except BaseException as error:
                    errors.append(error)
            thread = threading.Thread(target=serve, daemon=True)
            thread.start()
            port = self.free_port()
            tunnel = self.spawn(self.ssh(None, "-N", "-D", f"127.0.0.1:{port}"), name="socks")
            def connect() -> socket.socket | None:
                require(tunnel.poll() is None, "SOCKS tunnel exited")
                try:
                    return socket.create_connection(("127.0.0.1", port), timeout=0.3)
                except ConnectionError:
                    return None
            with wait_for(connect, "SOCKS listener") as peer:
                peer.settimeout(15)
                peer.sendall(b"\x05\x01\x00")
                require(self.recv_exact(peer, 2) == b"\x05\x00", "SOCKS5 method mismatch")
                peer.sendall(b"\x05\x01\x00\x01\x7f\x00\x00\x01" + struct.pack(">H", target))
                head = self.recv_exact(peer, 4)
                require(head[:3] == b"\x05\x00\x00", f"SOCKS5 CONNECT failed: {head!r}")
                size = {1: 4, 4: 16}.get(head[3])
                require(size is not None, "unexpected SOCKS5 address type")
                self.recv_exact(peer, size + 2)
                peer.sendall(payload)
                peer.shutdown(socket.SHUT_WR)
                require(self.recv_exact(peer, 32) == hashlib.sha256(payload).digest(), "SOCKS transfer mismatch")
                require(not peer.recv(1), "SOCKS missing EOF")
            thread.join(timeout=15)
            require(not thread.is_alive(), "SOCKS destination did not finish")
            if errors:
                raise errors[0]
            self.stop(tunnel)
            self.wait_tcp_closed(port)
        print("PASS SOCKS5 TCP binary transfer + half-close", flush=True)

    @staticmethod
    def recv_exact(peer: socket.socket, count: int) -> bytes:
        result = bytearray()
        while len(result) < count:
            chunk = peer.recv(count - len(result))
            require(chunk, "unexpected stream EOF")
            result.extend(chunk)
        return bytes(result)

    def environment(self, *, policy: str) -> None:
        names = ["LANG", "LC_AXE_SMOKE", "TEST_ALLOWED", "TEST_AWED", "LD_PRELOAD",
                 "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "PATH", "SSH_AUTH_SOCK",
                 "AXE_CLIENT_VALUE", "ENV.WITH.DOTS", "HOME", "AXE_STORE_MODE"]
        env = os.environ.copy()
        env.update(LANG="C", LC_AXE_SMOKE="smoke-locale", TEST_ALLOWED="accepted-value",
                   TEST_AWED="must-not-match-question-mark")
        code = "import json,os; print(json.dumps({k:os.environ.get(k) for k in " + repr(names) + "}))"
        baseline = json.loads(command(self.ssh(self.py(code))).stdout)
        sent = ("LD_PRELOAD= LD_LIBRARY_PATH=/smoke-client-libs DYLD_LIBRARY_PATH=/smoke-client-libs "
                "PATH=/smoke-client-path SSH_AUTH_SOCK=/smoke-client-agent "
                "AXE_CLIENT_VALUE=client ENV.WITH.DOTS=literal HOME=/smoke-client-home AXE_STORE_MODE=auto")
        result = command(self.ssh(self.py(code), "-o", "SendEnv=LANG LC_AXE_SMOKE TEST_ALLOWED TEST_AWED",
                                 "-o", "SetEnv=" + sent), env=env)
        values = json.loads(result.stdout)
        require(values["LANG"] == "C" and values["LC_AXE_SMOKE"] == "smoke-locale", "locale allowlist mismatch")
        require(values["TEST_ALLOWED"] == (baseline["TEST_ALLOWED"] if policy == "default" else "accepted-value"),
                "configured/default env allowlist mismatch")
        require(values["TEST_AWED"] == ("must-not-match-question-mark" if policy == "all" else baseline["TEST_AWED"]),
                "question-mark pattern accepted the wrong name")
        requested = {"LD_PRELOAD": "", "LD_LIBRARY_PATH": "/smoke-client-libs",
                     "DYLD_LIBRARY_PATH": "/smoke-client-libs", "SSH_AUTH_SOCK": "/smoke-client-agent",
                     "AXE_CLIENT_VALUE": "client", "ENV.WITH.DOTS": "literal"}
        for name, value in requested.items():
            require(values[name] == (value if policy == "all" else baseline[name]),
                    f"{policy} environment mismatch for {name}: {values[name]!r}")
        if policy == "all":
            require("/smoke-client-path" in values["PATH"].split(os.pathsep), "PATH wildcard acceptance mismatch")
        else:
            require(values["PATH"] == baseline["PATH"], "rejected PATH request changed the environment")
        expected_home = str((self.work / ".axe-home").resolve())
        require(values["HOME"] == expected_home and values["AXE_STORE_MODE"] == "off",
                f"server-owned environment mismatch: HOME={values['HOME']!r}, "
                f"expected {expected_home!r}; AXE_STORE_MODE={values['AXE_STORE_MODE']!r}")
        print(f"PASS environment {policy}: wildcard matching and server-owned values", flush=True)

    def agent(self) -> None:
        directory = Path(tempfile.mkdtemp(prefix="agent-", dir=self.root))
        master_path = directory / "master.sock"
        master = self.spawn(self.ssh(None, "-M", "-S", str(master_path), "-N", "-A"), name=f"{directory.name}-master")
        wait_for(master_path.exists, "private control master")
        sessions = []
        socket_paths = []
        expected = sorted(" ".join(line.split()[:2]) for line in agent_lines())
        for index in range(2):
            record = directory / f"agent-{index}.json"
            code = ("import json,os,stat,subprocess,sys; "
                "p=os.environ['SSH_AUTH_SOCK']; s=os.stat(p); d=os.stat(os.path.dirname(p)); "
                "r=subprocess.run([sys.argv[2],'-L'],capture_output=True,check=True); "
                "subprocess.run([sys.argv[2],'-T',sys.argv[3]],check=True); "
                "open(sys.argv[1],'w').write(json.dumps({'socket':p,'mode':stat.S_IMODE(s.st_mode),"
                "'directory_mode':stat.S_IMODE(d.st_mode),'uid':s.st_uid,'identities':r.stdout.decode().splitlines()})); "
                "sys.stdin.buffer.read()")
            process = self.spawn(self.ssh(self.py(code, record, shutil.which("ssh-add"), self.cert),
                "-S", str(master_path), "-A", "-o", "SetEnv=SSH_AUTH_SOCK=/smoke-client-agent"), name=f"agent-{index}", pipes=True)
            sessions.append(process)
            def published() -> object:
                require(process.poll() is None, "agent session exited before publishing evidence")
                if not record.exists():
                    return None
                try:
                    return json.loads(record.read_text())
                except json.JSONDecodeError:
                    return None
            evidence = wait_for(published, f"agent session {index}")
            require(evidence["uid"] == os.getuid(), "forwarded socket owner mismatch")
            require(evidence["mode"] & 0o077 == 0 and evidence["directory_mode"] & 0o077 == 0,
                    "forwarded agent socket/directory is not private")
            require(sorted(" ".join(line.split()[:2]) for line in evidence["identities"]) == expected,
                    "forwarded agent identities differ from the current agent")
            socket_paths.append(Path(evidence["socket"]))
        require(socket_paths[0] != socket_paths[1], "agent sockets are shared between session channels")
        disabled = command(self.ssh(self.py("import os; print(os.environ.get('SSH_AUTH_SOCK','ABSENT'))"),
                                   "-S", str(master_path), "-a")).stdout.strip()
        require(disabled == b"ABSENT", "non-forwarded channel inherited another channel's agent")
        sessions[0].communicate(timeout=15)
        require(sessions[0].returncode == 0, "first agent channel did not close normally")
        wait_for(lambda: not socket_paths[0].exists(), "first agent socket cleanup")
        require(socket_paths[1].is_socket(), "closing one channel removed another channel's agent")
        sessions[1].communicate(timeout=15)
        require(sessions[1].returncode == 0, "second agent channel did not close normally")
        wait_for(lambda: not socket_paths[1].exists(), "second agent socket cleanup")
        self.stop(master)
        print("PASS forwarded identities, real signature verification, per-channel isolation and cleanup", flush=True)

    def pty(self) -> None:
        master, slave = os.openpty()
        attrs = termios.tcgetattr(slave)
        attrs[0] &= ~termios.IXON
        attrs[3] = (attrs[3] | termios.ISIG) & ~(termios.ECHO | termios.ICANON)
        attrs[4] = attrs[5] = termios.B9600
        attrs[6][termios.VINTR] = b"\x1a"
        attrs[6][termios.VERASE] = b"\x09"
        termios.tcsetattr(slave, termios.TCSANOW, attrs)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 31, 97, 0, 0))
        code = ("import fcntl,json,os,struct,termios; a=termios.tcgetattr(0); "
            "print(json.dumps({'echo':bool(a[3]&termios.ECHO),'icanon':bool(a[3]&termios.ICANON),"
            "'isig':bool(a[3]&termios.ISIG),'ixon':bool(a[0]&termios.IXON),'ispeed':a[4],'ospeed':a[5],"
            "'intr':ord(a[6][termios.VINTR]),'erase':ord(a[6][termios.VERASE]),"
            "'size':list(struct.unpack('HHHH',fcntl.ioctl(0,termios.TIOCGWINSZ,b'\\0'*8))[:2])}),flush=True); "
            "os.read(0,1); print(json.dumps({'resized':list(struct.unpack('HHHH',fcntl.ioctl(0,termios.TIOCGWINSZ,b'\\0'*8))[:2])}),flush=True)")
        def controlling_terminal() -> None:
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        process = subprocess.Popen(self.ssh(self.py(code), "-tt"), stdin=slave, stdout=slave,
                                   stderr=slave, preexec_fn=controlling_terminal)
        self.processes.append(process)
        os.close(slave)
        pending = bytearray()
        def read_json() -> dict[str, object]:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if b"\n" in pending:
                    line, _, rest = pending.partition(b"\n")
                    pending[:] = rest
                    try:
                        return json.loads(line.strip())
                    except json.JSONDecodeError as error:
                        raise Failure(f"unexpected PTY output: {line!r}") from error
                ready, _, _ = select.select([master], [], [], max(0, deadline - time.monotonic()))
                require(ready, "PTY output deadline")
                data = os.read(master, 65536)
                require(data, "PTY closed before evidence")
                pending.extend(data)
            raise Failure("PTY JSON deadline")
        try:
            initial = read_json()
            expected = {"echo": False, "icanon": False, "isig": True, "ixon": False,
                        "ispeed": termios.B9600, "ospeed": termios.B9600,
                        "intr": 26, "erase": 9, "size": [31, 97]}
            require(initial == expected, f"actual remote PTY modes differ: {initial!r} != {expected!r}")
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 43, 123, 0, 0))
            os.kill(process.pid, signal.SIGWINCH)
            time.sleep(0.2)
            os.write(master, b"x")
            require(read_json() == {"resized": [43, 123]}, "PTY window-change did not reach the actual terminal")
            require(process.wait(timeout=15) == 0, "PTY SSH command failed")
        finally:
            os.close(master)
            self.stop(process)
        print("PASS actual terminal modes and resize", flush=True)

    @staticmethod
    def pid_start(pid: int) -> str | None:
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
            return None if fields[0] == "Z" else fields[19]
        except FileNotFoundError:
            return None

    def remember_pids(self, path: Path) -> list[tuple[int, str]]:
        def published() -> object:
            if not path.exists():
                return None
            try:
                return json.loads(path.read_text())
            except json.JSONDecodeError:
                return None
        pids = wait_for(published, f"PID evidence {path.name}")
        tracked = []
        for pid in pids:
            start = self.pid_start(pid)
            require(start is not None, f"fixture PID {pid} exited before observation")
            tracked.append((pid, start))
        self.children.extend(tracked)
        return tracked

    def gone(self, tracked: list[tuple[int, str]]) -> bool:
        return all(self.pid_start(pid) != start for pid, start in tracked)

    def lifecycle(self) -> None:
        command(self.ssh(self.py("import os,signal; os.kill(os.getpid(),signal.SIGTERM)")), expected=143)
        record = self.root / "signal-pids.json"
        code = ("import json,os,signal,subprocess,sys,time; "
                "p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(120)']); "
                "open(sys.argv[1],'w').write(json.dumps([os.getpid(),p.pid])); "
                "time.sleep(1); os.kill(os.getpid(),signal.SIGTERM)")
        session = self.spawn(self.ssh(self.py(code, record)), name="signal-exit", pipes=True)
        tracked = self.remember_pids(record)
        session.communicate(timeout=15)
        require(session.returncode == 143, f"signal exit status is {session.returncode}, not 143")
        wait_for(lambda: self.gone(tracked), "signal exit descendant cleanup")
        record = self.root / "disconnect-pids.json"
        socket_record = self.root / "disconnect-agent.txt"
        code = ("import json,os,subprocess,sys,time; "
                "p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(120)']); "
                "open(sys.argv[2],'w').write(os.environ['SSH_AUTH_SOCK']); "
                "open(sys.argv[1],'w').write(json.dumps([os.getpid(),p.pid])); time.sleep(120)")
        session = self.spawn(self.ssh(self.py(code, record, socket_record), "-A"), name="abrupt-disconnect", pipes=True)
        tracked = self.remember_pids(record)
        agent_record = command(self.ssh(self.py("import os; print(os.environ.get('SSH_AUTH_SOCK','ABSENT'))"))).stdout
        require(agent_record.strip() == b"ABSENT", "simultaneous plain session inherited forwarded agent")
        os.killpg(session.pid, signal.SIGKILL)
        session.wait(timeout=5)
        wait_for(lambda: self.gone(tracked), "abrupt disconnect descendant cleanup")
        abandoned_socket = Path(socket_record.read_text())
        wait_for(lambda: not abandoned_socket.exists(), "abrupt disconnect agent socket cleanup")
        def concurrent(index: int) -> bytes:
            token = f"concurrent-{index}"
            return command(self.ssh(self.py("import sys,time; time.sleep(.15); print(sys.argv[1])", token))).stdout.strip()
        with ThreadPoolExecutor(max_workers=4) as executor:
            values = list(executor.map(concurrent, range(4)))
        require(values == [f"concurrent-{i}".encode() for i in range(4)], "simultaneous session output crossed channels")
        print("PASS signal exits, descendant cleanup, abrupt disconnect and four concurrent sessions", flush=True)

    def sftp(self) -> None:
        client = Sftp(self)
        try:
            required = {"fsync@openssh.com": "1", "statvfs@openssh.com": "2",
                        "fstatvfs@openssh.com": "2", "posix-rename@openssh.com": "1",
                        "hardlink@openssh.com": "1", "limits@openssh.com": "1"}
            for name, version in required.items():
                require(client.extensions.get(name) == version, f"missing/incorrect advertised SFTP extension {name}")
            reply = client.ext("limits@openssh.com", b"", 201)
            limits = [reply.u64() for _ in range(4)]
            reply.done()
            require(all(limits) and limits[1] <= limits[0] and limits[2] <= limits[0],
                    f"invalid SFTP limits: {limits}")
            payload = bytes(range(256)) * 1281
            source = self.work / "wire-source.bin"
            renamed = self.work / "wire-renamed.bin"
            linked = self.work / "wire-linked.bin"
            old = b"old contents replaced atomically"
            renamed.write_bytes(old)
            handle = client.open(source, 0x1b)
            for offset in range(0, len(payload), 65536):
                client.request(6, string(handle) + struct.pack(">Q", offset) + string(payload[offset:offset + 65536]), 101).status(0)
            client.ext("fsync@openssh.com", string(handle), 101).status(0)
            require(source.read_bytes() == payload, "SFTP fsync/write bytes mismatch")
            for name, body in (("statvfs@openssh.com", string(str(self.work))),
                               ("fstatvfs@openssh.com", string(handle))):
                result = client.ext(name, body, 201)
                values = [result.u64() for _ in range(11)]
                result.done()
                actual = os.statvfs(self.work)
                require(values[0] == actual.f_bsize and values[1] == actual.f_frsize and values[2] == actual.f_blocks,
                        f"SFTP {name} filesystem geometry mismatch: {values}")
                require(values[3] >= values[4] and values[5] >= values[6] >= values[7] and values[10] > 0,
                        f"SFTP {name} invalid counters: {values}")
            client.request(4, string(handle), 101).status(0)
            client.ext("fsync@openssh.com", string(handle), 101).status(4, 9)
            client.ext("fstatvfs@openssh.com", string(handle), 101).status(4, 9)
            client.ext("posix-rename@openssh.com", string(str(source)) + string(str(renamed)), 101).status(0)
            require(not source.exists() and renamed.read_bytes() == payload, "SFTP overwrite rename mismatch")
            client.ext("hardlink@openssh.com", string(str(renamed)) + string(str(linked)), 101).status(0)
            require(os.path.samefile(renamed, linked) and linked.read_bytes() == payload, "SFTP hardlink mismatch")
            client.ext("hardlink@openssh.com", string(str(renamed)) + string(str(linked)), 101).status(4)
            client.ext("smoke-unknown@invalid", b"", 101).status(8)
            handle = client.open(linked, 1)
            received = bytearray()
            while len(received) < len(payload):
                result = client.request(5, string(handle) + struct.pack(">QI", len(received), 65536), 103)
                chunk = result.string()
                result.done()
                require(chunk and len(chunk) <= 65536, "SFTP READ returned invalid chunk length")
                received.extend(chunk)
            require(received == payload, "SFTP READ binary transfer mismatch")
            client.request(5, string(handle) + struct.pack(">QI", len(payload), 1), 101).status(1)
            client.request(4, string(handle), 101).status(0)
        finally:
            client.close()
        upload = self.root / "openssh-upload.bin"
        download = self.root / "openssh-download.bin"
        upload.write_bytes(bytes(range(256)) * 1281)
        remote = self.work / "openssh-upload.bin"
        renamed = self.work / "openssh-renamed.bin"
        link = self.work / "openssh-linked.bin"
        batch = (f'put -f "{upload}" "{remote}"\n'
                 f'df "{self.work}"\nrename "{remote}" "{renamed}"\n'
                 f'ln "{renamed}" "{link}"\nget "{link}" "{download}"\n'
                 f'rm "{renamed}"\nrm "{link}"\n')
        result = command(["sftp", "-vv", "-F", str(self.config), "-b", "-", "smoke"], data=batch.encode(), timeout=40)
        require(download.read_bytes() == upload.read_bytes(), "OpenSSH SFTP upload/download mismatch")
        require(not renamed.exists() and not link.exists(), "OpenSSH SFTP remove mismatch")
        (self.root / "sftp-negotiation.log").write_bytes(result.stderr)
        print("PASS SFTP negotiation, real extensions, error recovery and OpenSSH batch interoperability", flush=True)


class SftpReply(Wire):
    def status(self, *codes: int) -> None:
        code = self.u32()
        message, _language = self.string(), self.string()
        self.done()
        require(code in codes, f"SFTP status {code}, expected {codes}: {message!r}")


class Sftp:
    def __init__(self, harness: Harness):
        self.harness = harness
        self.process = harness.spawn(harness.ssh("sftp", "-s"), name="sftp-wire", pipes=True, bufsize=0)
        self.counter = 0
        self.send(b"\x01" + struct.pack(">I", 3))
        reply = Wire(self.receive())
        require(reply.take(1) == b"\x02" and reply.u32() == 3, "SFTP version negotiation mismatch")
        self.extensions: dict[str, str] = {}
        while reply.offset < len(reply.data):
            name, value = reply.string().decode(), reply.string().decode()
            require(name not in self.extensions, f"duplicate SFTP extension {name}")
            self.extensions[name] = value

    def send(self, payload: bytes) -> None:
        data = memoryview(struct.pack(">I", len(payload)) + payload)
        deadline = time.monotonic() + 15
        while data:
            _, ready, _ = select.select([], [self.process.stdin], [], max(0, deadline - time.monotonic()))
            require(ready, "SFTP write deadline")
            count = os.write(self.process.stdin.fileno(), data)
            require(count, "SFTP write EOF")
            data = data[count:]

    def read(self, count: int) -> bytes:
        data = bytearray()
        deadline = time.monotonic() + 15
        while len(data) < count:
            ready, _, _ = select.select([self.process.stdout], [], [], max(0, deadline - time.monotonic()))
            require(ready, "SFTP read deadline")
            chunk = os.read(self.process.stdout.fileno(), count - len(data))
            require(chunk, "SFTP unexpected EOF")
            data.extend(chunk)
        return bytes(data)

    def receive(self) -> bytes:
        size = struct.unpack(">I", self.read(4))[0]
        require(0 < size <= 1024 * 1024, f"invalid SFTP frame length {size}")
        return self.read(size)

    def request(self, kind: int, body: bytes, expected: int) -> SftpReply:
        self.counter += 1
        self.send(bytes([kind]) + struct.pack(">I", self.counter) + body)
        reply = SftpReply(self.receive())
        actual = reply.take(1)[0]
        require(reply.u32() == self.counter, "SFTP response request-id mismatch")
        if actual != expected:
            if actual == 101:
                code, message = reply.u32(), reply.string()
                raise Failure(f"SFTP request {kind}: unexpected status {code}: {message!r}")
            raise Failure(f"SFTP response type {actual}, expected {expected}")
        return reply

    def ext(self, name: str, body: bytes, expected: int) -> SftpReply:
        return self.request(200, string(name) + body, expected)

    def open(self, path: Path, flags: int) -> bytes:
        result = self.request(3, string(str(path)) + struct.pack(">II", flags, 0), 102)
        handle = result.string()
        result.done()
        require(handle, "SFTP OPEN returned empty handle")
        return handle

    def close(self) -> None:
        self.harness.stop(self.process)


def run(args: argparse.Namespace) -> None:
    require(sys.platform == "linux" and Path("/proc/self/stat").exists(), "run mode requires Linux /proc")
    require(args.axe.is_file() and os.access(args.axe, os.X_OK), "--axe must be an already-built executable")
    lines = agent_lines()
    config = json.loads(args.identity_config.read_text())
    require(config["certificate"] in [" ".join(line.split()[:2]) for line in lines],
            "the configured certificate is not present in the current SSH_AUTH_SOCK")
    certificate(config["certificate"])
    with tempfile.TemporaryDirectory(prefix="axe-ssh-", dir="/tmp") as directory:
        harness = Harness(Path(directory), args)
        try:
            harness.start()
            harness.plain_key_rejection()
            harness.environment(policy="default")
            harness.stop(harness.server)
            harness.wait_tcp_closed(harness.port)
            harness.start(accept_env="LANG,LC_*,TEST_?L*WED")
            harness.forwarding()
            harness.environment(policy="patterns")
            harness.agent()
            harness.pty()
            harness.lifecycle()
            harness.sftp()
            harness.stop(harness.server)
            harness.wait_tcp_closed(harness.port)
            harness.start(accept_env="*")
            harness.environment(policy="all")
            harness.agent()
        except BaseException:
            for path in sorted(Path(directory).glob("*.log")):
                print(f"\n--- {path.name} ---", file=sys.stderr)
                print(path.read_text(errors="replace")[-24000:], file=sys.stderr)
            raise
        finally:
            harness.close()
    print("PASS all local real-binary SSH smoke scenarios", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="mode", required=True)
    edition = sub.add_parser("prepare-edition", help="create caller-owned edition using current agent's public certificate")
    edition.add_argument("directory", type=Path)
    edition.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    edition.add_argument("--principal")
    edition.add_argument("--certificate-index", type=int, default=0)
    smoke = sub.add_parser("run", help="exercise an already-built edition binary; never builds")
    smoke.add_argument("--axe", required=True, type=Path)
    smoke.add_argument("--identity-config", required=True, type=Path)
    args = parser.parse_args()
    for name in ("ssh", "sftp", "ssh-add", "ssh-keygen"):
        require(shutil.which(name), f"missing prerequisite executable: {name}")
    os.umask(0o077)
    def interrupted(signum: int, _frame: object) -> None:
        raise Failure(f"interrupted by signal {signum}")
    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupted)
    if args.mode == "prepare-edition":
        require(args.certificate_index >= 0, "certificate index must be nonnegative")
        prepare(args)
    else:
        run(args)


if __name__ == "__main__":
    try:
        main()
    except (Failure, OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
        print(f"ssh-live-smoke: {error}", file=sys.stderr)
        sys.exit(1)
