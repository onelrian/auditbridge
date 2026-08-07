# Installation

AuditBridge reads NetBird audit events and delivers them to one or more sinks.
Create a NetBird personal access token with audit-log read access before
deploying it.

## Docker

Mount credentials as files rather than exposing them in environment variables:

```bash
docker run --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e LOKI_URL=https://loki.example.com \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

Replace `<immutable-tag>` with a released application version. Do not use
`latest` in a production deployment.

## Docker Compose

Use a read-only filesystem and a persistent volume only when `CURSOR_FILE` is
configured. See [Configuration](CONFIGURATION.md) for the cursor behavior.

```yaml
services:
  auditbridge:
    image: ghcr.io/onelrian/auditbridge:<immutable-tag>
    read_only: true
    security_opt: [no-new-privileges:true]
    cap_drop: [ALL]
    environment:
      NETBIRD_API_TOKEN_FILE: /run/secrets/netbird-token
      LOKI_URL: https://loki.example.com
      CURSOR_FILE: /data/cursor.json
    volumes:
      - ./netbird-token:/run/secrets/netbird-token:ro
      - auditbridge-cursor:/data
volumes:
  auditbridge-cursor: {}
```

## Kubernetes and Helm

Use the chart after the corresponding application and chart releases exist.
It expects an existing Secret and does not create credentials:

```bash
helm install auditbridge oci://ghcr.io/onelrian/charts/auditbridge \
  --version <chart-version> \
  --namespace netbird --create-namespace \
  --set envFromSecret.NETBIRD_API_TOKEN=netbird-audit/audit-token
```

Use a release name distinct from NetBird's `signal` component. See the
[chart README](../charts/auditbridge/README.md) for values and ServiceMonitor
configuration.
