#!/usr/bin/env python3
"""Minimal TCP syslog receiver standing in for a Wazuh manager's listener,
to prove the RFC3164 syslog sink actually delivers framed events over the
wire, not just that it compiles."""
import socketserver


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            print(f"[wazuh-receiver] {line.decode().rstrip()}")


if __name__ == "__main__":
    with socketserver.ThreadingTCPServer(("0.0.0.0", 1514), Handler) as server:
        server.serve_forever()
