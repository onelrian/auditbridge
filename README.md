# Signal

[![Build Status](https://img.shields.io/github/actions/workflow/status/onelrian/signal/docker.yml?branch=main)](https://github.com/onelrian/signal/actions)
[![Docker Pulls](https://img.shields.io/docker/pulls/onelrian/signal)](https://hub.docker.com/r/onelrian/signal)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

Signal is a high-performance observability bridge for NetBird. It ingests audit events from the NetBird Management API and ships them to Grafana Loki, a Wazuh manager, and/or any generic HTTP endpoint, enabling real-time security monitoring, compliance auditing, and incident response.

## Features

- **Zero-Dependency Architecture**: Single binary or container; no local database or filesystem required.
- **Pluggable Sinks**: Ship the same event stream to Loki, Wazuh (syslog), and/or a generic HTTP endpoint at once, configured purely via environment variables.
- **Stateful Event Tracking**: Tracks an independent event cursor per sink, so one sink being down never blocks or duplicates delivery to the others.
- **Universal Compatibility**: Works seamlessly with both NetBird Cloud and Self-Hosted instances.
- **Production Hardened**: Written in Rust for minimal memory footprint and high reliability.

## Architecture

Signal acts as a stateless, highly available middleware between your NetBird control plane and your observability/SIEM stack.

```mermaid
flowchart LR
    NA[NetBird API] -->|JSON Stream| Signal[Signal Exporter]
    Signal -->|http transport, loki encoding| Loki[Grafana Loki]
    Signal -->|syslog transport, RFC3164/5424 encoding| Wazuh[Wazuh Manager]
    Signal -->|http transport, json/ndjson encoding| HTTP[Any HTTP Endpoint]
    Loki -->|LogQL| Grafana[Grafana Dashboards]
```

Sinks are built from two generic primitives, not one Rust type per vendor:

- **Transport**: how bytes are delivered. `http` (any URL, method, headers) or `syslog` (TCP/UDP).
- **Encoding**: what the bytes look like. `json` (flat array), `ndjson`, `loki` (Loki's push API stream/label shape), `syslog3164`, or `syslog5424`.

Loki and Wazuh ship as named presets (`SINKS=loki,wazuh` works with zero further config), but any other backend that speaks plain HTTP or syslog, Datadog, Splunk HEC, a generic webhook, another SIEM, needs no new code, just a `SINK_<NAME>_*` block of environment variables. See [Adding a Custom Sink](#adding-a-custom-sink) below.

## Prerequisites

### Required

- **NetBird Personal Access Token (PAT)**: Admin-level token with audit log read permissions.
- **Grafana Loki**: A reachable Loki instance (configured for ingestion).
- **Network Access**: Outbound HTTPS to NetBird API and Loki endpoints.

### Recommended for Production

- **Secrets Management**: Store `NETBIRD_API_TOKEN` in Kubernetes Secrets or Docker Secrets.
- **TLS**: Ensure `LOKI_URL` uses HTTPS if traversing public networks.

## Quick Start

### 1. Obtain NetBird PAT

1. Log in to your NetBird Dashboard.
2. Go to Users > Access Tokens.
3. Create a token and copy it (it is only shown once).

### 2. Deploy Signal

```bash
docker run -d --name signal \
  --restart unless-stopped \
  -e NETBIRD_API_URL="https://api.netbird.io/api" \
  -e NETBIRD_API_TOKEN="nbp_your_token_here" \
  -e LOKI_URL="http://<LOKI_URL>" \ # Replace with your Loki URL
  ghcr.io/onelrian/signal:latest
```
It will start a container named `signal` and run it in the background.

![Example usage](docs/images/example.png)

## Production Deployment

### Configuration Reference

Signal is configured entirely via environment variables.

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `NETBIRD_API_TOKEN` | NetBird PAT with audit permissions | - | **Yes** |
| `SINKS` | Comma-separated list of sink names to fan out to | `loki` | No |
| `NETBIRD_API_URL` | NetBird API base URL (for Self-Hosted) | `https://api.netbird.io` | No |
| `CHECK_INTERVAL` | Event polling interval (seconds) | `10` | No |
| `RUST_LOG` | Log level (`error`, `warn`, `info`, `debug`) | `info` | No |

#### Built-in sink presets

| Sink name | Variable | Description | Default |
|---|---|---|---|
| `loki` | `LOKI_URL` (or `SINK_LOKI_URL`) | Loki base URL (legacy var) or exact push URL | `http://loki:3100` |
| `wazuh` | `SINK_WAZUH_ADDR` (or `WAZUH_ADDR`) | Wazuh manager syslog address (`host:port`) | - (required if `wazuh` is in `SINKS`) |

#### Adding a Custom Sink

Any name in `SINKS` besides `loki`/`wazuh` is a fully generic sink, no code changes needed. Sink names become part of an environment variable name, so use only letters, digits, and underscores (no hyphens or spaces).

| Variable | Description |
|---|---|
| `SINK_<NAME>_TRANSPORT` | `http` or `syslog` |
| `SINK_<NAME>_ENCODING` | `json`, `ndjson`, `loki`, `syslog3164`, or `syslog5424` |
| `SINK_<NAME>_URL` | Destination URL (`http` transport) |
| `SINK_<NAME>_METHOD` | HTTP method, defaults to `POST` (`http` transport) |
| `SINK_<NAME>_HEADERS` | `Key1:Value1,Key2:Value2` (`http` transport, optional) |
| `SINK_<NAME>_ADDR` | Destination `host:port` (`syslog` transport) |
| `SINK_<NAME>_PROTOCOL` | `tcp` or `udp`, defaults to `tcp` (`syslog` transport) |

Example, shipping to Loki and a generic JSON webhook at once:

```bash
docker run -d --name signal \
  -e NETBIRD_API_TOKEN="nbp_your_token_here" \
  -e SINKS="loki,my_webhook" \
  -e SINK_MY_WEBHOOK_TRANSPORT="http" \
  -e SINK_MY_WEBHOOK_URL="https://collector.example.com/ingest" \
  -e SINK_MY_WEBHOOK_ENCODING="json" \
  -e SINK_MY_WEBHOOK_HEADERS="Authorization:Bearer secret" \
  ghcr.io/onelrian/auditbridge:latest
```

### Docker Compose (Production)

```yaml
version: '3.8'

services:
  signal:
    image: ghcr.io/onelrian/signal:latest
    container_name: signal
    restart: unless-stopped
    
    # Security
    read_only: true
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
    
    # Environment
    environment:
      - NETBIRD_API_TOKEN=${NETBIRD_PAT}
      - LOKI_URL=http://loki:3100
      - CHECK_INTERVAL=30
      - RUST_LOG=info
    
    # Dependencies
    depends_on:
      - loki
    
    # Network
    networks:
      - monitoring

networks:
  monitoring:
    driver: bridge
```

### Kubernetes Deployment

Use a simple Deployment and Secret.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: signal
spec:
  replicas: 1
  selector:
    matchLabels:
      app: signal
  template:
    metadata:
      labels:
        app: signal
    spec:
      containers:
      - name: signal
        image: ghcr.io/onelrian/signal:latest
        env:
        - name: LOKI_URL
          value: "http://loki.monitoring.svc:3100"
        - name: NETBIRD_API_TOKEN
          valueFrom:
            secretKeyRef:
              name: netbird-secrets
              key: api-token
```


## Monitoring & Observability

Signal enriches every event with structured metadata for querying.

### Log Labels

| Label | Description | Example |
|---|---|---|
| `job` | Component identifier | `netbird-events` |
| `activity` | Human-readable event name | `Group created` |
| `activity_code` | Machine-readable event code | `group.add` |
| `account_id` | Tenant/Account ID | `w89s7...` |
| `initiator_email` | Actor who triggered the event | `admin@example.com` |

![Grafana Dashboard](docs/images/grafana_dashboard_example.png)

## License

Distributed under the MIT License. See [LICENSE](LICENSE) for more information.