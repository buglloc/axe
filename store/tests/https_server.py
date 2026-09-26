#!/usr/bin/env python3
import argparse
import hashlib
import http.server
import os
import ssl
from pathlib import Path


class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        if getattr(self, "_etag", None):
            self.send_header("ETag", self._etag)
        self.send_header("Cache-Control", "no-cache")
        super().end_headers()

    def send_head(self):
        path = Path(self.translate_path(self.path))
        self._etag = None
        if path.is_file():
            self._etag = '"' + hashlib.sha256(path.read_bytes()).hexdigest() + '"'
            if self.headers.get("If-None-Match") == self._etag:
                self.send_response(304)
                self.end_headers()
                return None
        return super().send_head()

    def log_message(self, message, *args):
        request_log = getattr(self.server, "request_log", None)
        if request_log:
            with open(request_log, "a", encoding="utf-8") as output:
                output.write((message % args) + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", required=True)
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--port-file", required=True)
    parser.add_argument("--request-log")
    parser.add_argument("--tls-cert")
    parser.add_argument("--tls-key")
    args = parser.parse_args()

    handler = lambda *values, **kwargs: Handler(
        *values, directory=args.directory, **kwargs
    )
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    server.request_log = args.request_log
    if args.tls_cert or args.tls_key:
        if not args.tls_cert or not args.tls_key:
            parser.error("--tls-cert and --tls-key must be supplied together")
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(args.tls_cert, args.tls_key)
        server.socket = context.wrap_socket(server.socket, server_side=True)

    port_file = Path(args.port_file)
    port_file.write_text(str(server.server_port), encoding="ascii")
    os.chmod(port_file, 0o600)
    server.serve_forever()


if __name__ == "__main__":
    main()
