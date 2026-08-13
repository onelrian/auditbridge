#!/usr/bin/env python3
"""Minimal HTTP webhook receiver standing in for a generic HTTP sink
destination, to prove the generic HTTP transport actually delivers framed
events over the wire, not just that it compiles."""
import http.server
import json


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length).decode()
        print(f"[webhook-receiver] {self.path} ct={self.headers.get('Content-Type', '')}")
        print(body)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    http.server.HTTPServer(("0.0.0.0", 8081), Handler).serve_forever()
