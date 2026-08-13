#!/usr/bin/env python3
"""Minimal generic syslog receiver standing in for any syslog destination,
proving the generic syslog transport delivers RFC 3164 / RFC 5424 framed
events over both TCP and UDP."""
import socket
import socketserver


class TcpHandler(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            print(f"[syslog-tcp-receiver] {line.decode().rstrip()}")


class UdpHandler(socketserver.BaseRequestHandler):
    def handle(self):
        data = self.request[0]
        print(f"[syslog-udp-receiver] {data.decode().rstrip()}")


if __name__ == "__main__":
    tcp = socketserver.ThreadingTCPServer(("0.0.0.0", 1514), TcpHandler)
    udp = socketserver.ThreadingUDPServer(("0.0.0.0", 1515), UdpHandler)
    import threading

    threading.Thread(target=tcp.serve_forever, daemon=True).start()
    threading.Thread(target=udp.serve_forever, daemon=True).start()
    print("syslog receivers listening on tcp:1514 udp:1515")
    threading.Event().wait()
