"""End-to-end bootstrap tests: every generated runtime against a local server.

Run inside the development shell (zig, python3, node, perl, curl, wget, bash):

    python3 -m unittest discover -s bootstrap
"""

import hashlib
import os
import platform
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import generate  # noqa: E402
from ascon import hexdigest as ascon_hexdigest  # noqa: E402

ROOT = Path(__file__).resolve().parent
ARCH = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}[platform.machine().lower()]
TARGET = f"{ARCH}-linux"
URL_PATH = "/bucket/axe/releases/v0.0.0/axe"
PAYLOAD_SOURCE = r"""
#include <stdio.h>

int main(int argc, char **argv) {
    printf("payload");
    for (int index = 1; index < argc; ++index) {
        printf(" %s", argv[index]);
    }
    printf("\n");
    return 7;
}
"""
# Tools every shell run needs besides the downloader under test.
SHELL_BASE_TOOLS = ("sh", "cat", "uname", "mktemp", "chmod", "mv", "rm", "mkdir", "dirname", "wc", "tr", "sha256sum")

# Embedded into shell, Python, JavaScript, Perl, and C literals.
DEFAULT_ARGS = ["default-arg", "it's \"quoted\" \\ $HOME ??="]


class Server:
    """Dual-stack HTTP server on one port; the response shape is switchable."""

    def __init__(self, payload: bytes) -> None:
        self.payload = payload
        self.mode = "normal"
        self.listener = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
        self.listener.bind(("::", 0))
        self.listener.listen(32)
        self.port = self.listener.getsockname()[1]
        threading.Thread(target=self._serve, daemon=True).start()

    def _serve(self) -> None:
        while True:
            try:
                connection, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self._handle, args=(connection,), daemon=True).start()

    def _handle(self, connection: socket.socket) -> None:
        with connection:
            request = b""
            while b"\r\n\r\n" not in request:
                chunk = connection.recv(4096)
                if not chunk:
                    return
                request += chunk

            if request.split(b" ")[1] != URL_PATH.encode():
                connection.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                return

            body = bytearray(self.payload)
            if self.mode == "corrupt":
                body[len(body) // 2] ^= 1
            head = f"HTTP/1.1 200 OK\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode()

            if self.mode == "split":
                # Status line, headers, and body in separate segments.
                connection.sendall(head[:17])
                time.sleep(0.1)
                connection.sendall(head[17:])
                time.sleep(0.1)
                connection.sendall(bytes(body))
            else:
                connection.sendall(head + bytes(body))

    def close(self) -> None:
        self.listener.close()


def closed_port() -> int:
    """A port with nothing listening, so HTTPS attempts fail fast."""

    with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as probe:
        probe.bind(("::1", 0))
        return probe.getsockname()[1]


class BootstrapRuntimeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.work = Path(tempfile.mkdtemp(prefix="axe-bootstrap-test."))

        source = cls.work / "payload.c"
        source.write_text(PAYLOAD_SOURCE)
        payload = cls.work / "payload"
        subprocess.run(
            ["zig", "cc", "-target", f"{ARCH}-linux-musl", "-static", "-Os", "-o", str(payload), str(source)],
            check=True,
        )
        data = payload.read_bytes()

        cls.server = Server(data)
        bundle = {
            "version": "0.0.0",
            "default_args": DEFAULT_ARGS,
            "targets": {
                TARGET: {
                    "host": "storage.test",
                    "path": URL_PATH,
                    "ports": {"https": closed_port(), "http": cls.server.port},
                    "addresses": ["127.0.0.1", "::1"],
                    "size": len(data),
                    "sha256": hashlib.sha256(data).hexdigest(),
                    "ascon_hash256": ascon_hexdigest(data),
                }
            },
        }
        # The Native loader has no port override in tests: its HTTP port comes
        # from the bundle, like every script runtime.
        cls.generated = cls.work / "generated"
        generate.generate(bundle, cls.generated)
        subprocess.run(
            [str(ROOT / "native/build.sh"), ARCH, str(cls.generated / f"{TARGET}.h"), str(cls.generated / "native")],
            check=True,
        )

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.close()
        shutil.rmtree(cls.work, ignore_errors=True)

    def setUp(self) -> None:
        self.server.mode = "normal"
        self.temporary = Path(tempfile.mkdtemp(dir=self.work))

    def tool_path(self, *tools: str) -> str:
        """A PATH directory holding only the shell basics plus *tools*."""

        directory = Path(tempfile.mkdtemp(dir=self.work))
        for tool in (*SHELL_BASE_TOOLS, *tools):
            os.symlink(shutil.which(tool), directory / tool)
        return str(directory)

    def runtimes(self) -> dict[str, tuple[list[str], dict[str, str]]]:
        generated = self.generated
        return {
            "shell/curl": (["sh", str(generated / "sh")], {"PATH": self.tool_path("curl")}),
            "shell/wget": (["sh", str(generated / "sh")], {"PATH": self.tool_path("wget")}),
            "shell/python": (["sh", str(generated / "sh")], {"PATH": self.tool_path("python3")}),
            "shell/perl": (["sh", str(generated / "sh")], {"PATH": self.tool_path("perl")}),
            "shell/node": (["sh", str(generated / "sh")], {"PATH": self.tool_path("node")}),
            "shell/bash": (["sh", str(generated / "sh")], {"PATH": self.tool_path("bash")}),
            "python": ([sys.executable, str(generated / "py")], {}),
            "node": (["node", str(generated / "js")], {}),
            "perl": (["perl", str(generated / "pl")], {}),
            "native": ([str(generated / "native")], {}),
        }

    def run_runtime(self, command: list[str], environment: dict[str, str], *args: str):
        env = {"PATH": os.environ["PATH"], "HOME": str(self.temporary), "TMPDIR": str(self.temporary), "LC_ALL": "C"}
        env.update(environment)
        return subprocess.run(
            [*command, *args], env=env, cwd=self.temporary, capture_output=True, text=True, timeout=120
        )

    def test_every_runtime_runs_the_verified_payload(self) -> None:
        for name, (command, environment) in self.runtimes().items():
            with self.subTest(name):
                result = self.run_runtime(command, environment, "one", "two")
                self.assertEqual(result.stdout, "payload one two\n", result.stderr)
                self.assertEqual(result.returncode, 7, result.stderr)

    def test_default_args_apply_only_without_arguments(self) -> None:
        for name, (command, environment) in self.runtimes().items():
            with self.subTest(name):
                result = self.run_runtime(command, environment)
                self.assertEqual(result.stdout, f"payload {' '.join(DEFAULT_ARGS)}\n", result.stderr)

    def test_fragmented_response_head(self) -> None:
        self.server.mode = "split"
        for name, (command, environment) in self.runtimes().items():
            with self.subTest(name):
                result = self.run_runtime(command, environment, "split")
                self.assertEqual(result.stdout, "payload split\n", result.stderr)

    def test_corrupted_payload_is_never_executed(self) -> None:
        self.server.mode = "corrupt"
        for name, (command, environment) in self.runtimes().items():
            with self.subTest(name):
                result = self.run_runtime(command, environment, "corrupt")
                self.assertNotIn("payload", result.stdout)
                self.assertEqual(result.returncode, 1, result.stderr)

    def test_output_keeps_the_download_without_running_it(self) -> None:
        for name in ("shell/curl", "python", "node", "perl"):
            command, environment = self.runtimes()[name]
            output = self.temporary / name.replace("/", "-") / "axe"
            with self.subTest(name):
                result = self.run_runtime(command, environment, "--output", str(output), "--no-exec")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.replace(f"{output}\n", ""), "")
                self.assertEqual(output.read_bytes(), self.server.payload)


if __name__ == "__main__":
    unittest.main()
