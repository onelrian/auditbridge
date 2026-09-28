#!/usr/bin/env python3
"""Minimal HTTP receiver standing in for any generic webhook/HTTP intake
endpoint, to prove the generic HTTP sink delivers encoded batches over the
wire, not just that it compiles. Logs each request's method, path, content
type, and body so delivery is visible in `docker compose logs`."""
import http.server


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length).decode(errors="replace")
        print(
            f"[webhook-receiver] {self.command} {self.path} "
            f"content-type={self.headers.get('Content-Type')}"
        )
        for line in body.splitlines():
            print(f"[webhook-receiver] {line}")
        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, fmt, *args):
        # Silence the default per-request access log line; the prints above
        # are the evidence we want in the compose logs.
        pass


if __name__ == "__main__":
    http.server.HTTPServer(("0.0.0.0", 9099), Handler).serve_forever()
