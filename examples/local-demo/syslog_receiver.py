#!/usr/bin/env python3
"""Minimal syslog receivers standing in for a Wazuh manager's listener (TCP,
port 1514, exercised by the `wazuh` sink preset) and a generic syslog intake
(UDP, port 1515, exercised by a generic `syslog` sink), to prove the syslog
sinks actually deliver framed events over the wire, not just that they
compile."""
import socketserver
import threading


class TcpHandler(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            print(f"[syslog-tcp] {line.decode(errors='replace').rstrip()}")


class UdpHandler(socketserver.BaseRequestHandler):
    def handle(self):
        data = self.request[0].decode(errors="replace")
        for line in data.splitlines():
            print(f"[syslog-udp] {line}")


if __name__ == "__main__":
    tcp = socketserver.ThreadingTCPServer(("0.0.0.0", 1514), TcpHandler)
    udp = socketserver.ThreadingUDPServer(("0.0.0.0", 1515), UdpHandler)
    threading.Thread(target=tcp.serve_forever, daemon=True).start()
    udp.serve_forever()
