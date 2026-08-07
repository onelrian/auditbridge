# Local demo

Runs AuditBridge end to end against a real Loki, a real syslog receiver, and
a stubbed NetBird API (`mock_netbird.py` serves fixed sample audit events, no
real NetBird account or credentials are used). This reproduces exactly what
the screenshots in [docs/OPERATIONS.md](../../docs/OPERATIONS.md) show.

```bash
docker compose up -d --build
```

Then verify delivery yourself:

```bash
# Startup and delivery logs
docker compose logs auditbridge

# Events actually stored in Loki
curl -s 'http://localhost:3100/loki/api/v1/query_range?query={job="netbird-events"}' | jq

# Frames received by the Wazuh-equivalent syslog receiver
docker compose logs wazuh-receiver

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
