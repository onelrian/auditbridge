# Local demo

Runs AuditBridge end to end against real receivers for **every sink type** the
project supports, with a stubbed NetBird API (`mock_netbird.py` serves fixed
sample audit events, no real NetBird account or credentials are used):

| Sink | Receiver | What it proves |
|---|---|---|
| `loki` | real Grafana Loki | events stored and queryable via LogQL |
| `wazuh` | UDP syslog receiver (RFC 3164) | the common Wazuh case (UDP `11514`) |
| `webhook` | HTTP receiver | generic HTTP transport delivers NDJSON |
| `syslogtcp` | TCP syslog receiver (RFC 5424) | generic syslog over TCP |
| `syslogudp` | UDP syslog receiver (RFC 3164) | generic syslog over UDP |

The Wazuh decoder and ruleset that decode and alert on these events are in
[`wazuh/`](wazuh/): see [docs/SINKS.md](../../docs/SINKS.md#wazuh) for how to
install them on a real manager.

```bash
docker compose up -d --build
```

Then verify delivery yourself:

```bash
# Startup and delivery logs (one "Sent N events" line per sink)
docker compose logs auditbridge

# Events actually stored in Loki
curl -s 'http://localhost:3100/loki/api/v1/query_range?query={job="netbird-events"}' | jq

# Frames received by the Wazuh-equivalent UDP syslog receiver
docker compose logs wazuh-receiver

# Frames received by the generic HTTP webhook receiver
docker compose logs webhook-receiver

# Frames received by the generic syslog receiver (TCP and UDP)
docker compose logs syslog-receiver

# Live health and metrics
curl http://localhost:19090/healthz
curl http://localhost:19090/readyz
curl http://localhost:19090/metrics
```

Loki's API isn't exposed to the host by default in this compose file; run
the query from inside the `loki` container instead if you don't want to add
a port mapping:

```bash
docker compose exec loki wget -qO- 'http://localhost:3100/loki/api/v1/query_range?query={job="netbird-events"}'
```

```bash
docker compose down -v
```

## What the output looks like

The `auditbridge` service logs one delivery line per sink:

```
Sent 3 events to sink 'loki' (http://loki:3100/loki/api/v1/push)
Sent 3 events to sink 'wazuh' (wazuh-receiver:1515)
Sent 3 events to sink 'webhook' (http://webhook-receiver:8081/ingest)
Sent 3 events to sink 'syslogtcp' (syslog-receiver:1514)
Sent 3 events to sink 'syslogudp' (syslog-receiver:1515)
```

The `wazuh-receiver` and `syslog-receiver` logs show the framed events, e.g.
an RFC 3164 frame:

```
[syslog-udp-receiver] <134>2026-08-13T11:32:35Z auditbridge netbird-audit: {"account_id":"acc-demo-01","activity":"Peer added","activity_code":"peer.add","event_id":"evt-1001","initiator_email":"alice@example.com","initiator_name":"Alice Example","meta":{"peer_name":"laptop-alice"},"target_id":"peer-7f3a","timestamp":"2026-08-13T11:32:35Z"}
```

The `webhook-receiver` log shows the NDJSON body and its `Content-Type`:

```
[webhook-receiver] /ingest ct=application/x-ndjson
{"account_id":"acc-demo-01","activity":"Peer added","activity_code":"peer.add","event_id":"evt-1001","initiator_email":"alice@example.com","initiator_name":"Alice Example","meta":{"peer_name":"laptop-alice"},"target_id":"peer-7f3a","timestamp":"2026-08-13T11:32:35Z"}
```
