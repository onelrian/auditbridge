# Local demo

Runs AuditBridge end to end against a real Loki, real TCP and UDP syslog
receivers, a real HTTP webhook receiver, and a stubbed NetBird API
(`mock_netbird.py` serves fixed sample audit events, no real NetBird account
or credentials are used). Two AuditBridge instances cover every sink type:

- `auditbridge` — the `loki` and `wazuh` sink presets
- `auditbridge-generic` — a generic `http` sink (`webhook`, ndjson encoding,
  custom header) and a generic `syslog` sink (`sysloggen`, RFC 5424 over UDP)

All four destinations are verified in the logs below; every command works
against this stack.

```bash
docker compose up -d --build
```

## Verify delivery

```bash
# Startup and delivery logs
docker compose logs auditbridge
docker compose logs auditbridge-generic

# Events actually stored in Loki
docker compose exec loki wget -qO- \
  'http://localhost:3100/loki/api/v1/query_range?query=%7Bjob%3D%22netbird-events%22%7D'

# Frames received by the Wazuh-equivalent TCP syslog listener (RFC 3164)
docker compose logs wazuh-receiver | grep syslog-tcp

# Frames received by the UDP syslog listener (RFC 5424)
docker compose logs wazuh-receiver | grep syslog-udp

# NDJSON batches received by the generic HTTP webhook
docker compose logs webhook-receiver

# Live health and metrics (main instance on 19090, generic on 19091)
curl http://localhost:19090/healthz
curl http://localhost:19090/readyz
curl http://localhost:19090/metrics
curl http://localhost:19091/readyz
curl http://localhost:19091/metrics
```

`auditbridge_events_delivered_total{sink="loki"}` and friends show the
per-sink delivered counts after the first poll cycle.

Loki's API isn't exposed to the host by default in this compose file; run
the query from inside the `loki` container instead if you don't want to add
a port mapping:

```bash
docker compose exec loki wget -qO- 'http://localhost:3100/loki/api/v1/query_range?query={job="netbird-events"}'
```

```bash
docker compose down -v
```
