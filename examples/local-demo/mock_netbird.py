#!/usr/bin/env python3
"""Stand-in for the real NetBird Management API's /api/events/audit endpoint.
Serves a fixed set of realistic (but fake) audit events so the live test
exercises the real fetch/encode/deliver pipeline end to end. No real
NetBird account or credentials are involved."""
import http.server
import json
from datetime import datetime, timedelta, timezone

now = datetime.now(timezone.utc)


def ts(seconds_ago):
    return (now - timedelta(seconds=seconds_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")


EVENTS = [
    {
        "id": "evt-1001",
        "timestamp": ts(9),
        "activity": "Peer added",
        "activity_code": "peer.add",
        "initiator_id": "user-alice",
        "initiator_email": "alice@example.com",
        "initiator_name": "Alice Example",
        "target_id": "peer-7f3a",
        "account_id": "acc-demo-01",
        "meta": {"peer_name": "laptop-alice"},
    },
    {
        "id": "evt-1002",
        "timestamp": ts(5),
        "activity": "Group created",
        "activity_code": "group.add",
        "initiator_id": "user-bob",
        "initiator_email": "bob@example.com",
        "initiator_name": "Bob Example",
        "target_id": "group-eng",
        "account_id": "acc-demo-01",
        "meta": {"group_name": "engineering"},
    },
    {
        "id": "evt-1003",
        "timestamp": ts(1),
        "activity": "User login",
        "activity_code": "user.login",
        "initiator_id": "user-alice",
        "initiator_email": "alice@example.com",
        "initiator_name": "Alice Example",
        "target_id": None,
        "account_id": "acc-demo-01",
        "meta": None,
    },
]


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/events/audit":
            body = json.dumps(EVENTS).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, fmt, *args):
        print(f"[mock-netbird] {self.address_string()} {fmt % args}")


if __name__ == "__main__":
    http.server.HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
