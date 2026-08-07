# Installation

AuditBridge reads NetBird audit events and delivers them to one or more sinks.
Every method below needs a NetBird personal access token first.

## Get a NetBird access token

1. Sign in to your NetBird dashboard (Cloud or self-hosted).
2. Go to the **Team** section. Create a **Service User** for AuditBridge
   rather than using a personal user's token, so revoking someone's NetBird
   access later doesn't also break AuditBridge (see NetBird's
   [access token docs](https://docs.netbird.io/how-to/access-netbird-public-api)).
3. Under that service user, create a new access token.
4. Copy the token immediately and store it securely. NetBird only stores a
   hashed version and cannot show the plaintext again once you close the
   popup; if you lose it, revoke it and create a new one.

## Docker

Mount the token as a file rather than exposing it in an environment variable:

```bash
echo "nbp_your_token_here" > netbird-token
chmod 600 netbird-token

docker run -d --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e LOKI_URL=https://loki.example.com \
  -p 9090:9090 \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

> [!WARNING]
> Replace `<immutable-tag>` with a released application version. Do not use
> `latest` in a production deployment, it moves whenever a new release ships.

Confirm it started correctly:

```bash
docker logs auditbridge
curl http://localhost:9090/healthz   # "ok"
```

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

`./netbird-token` is the same file created in the Docker section above. Run
`docker compose up -d` then `docker compose logs -f auditbridge` to confirm it
started delivering.

## Kubernetes and Helm

Chart releases are versioned independently from application releases,
`<chart-version>` below is a `charts/auditbridge/Chart.yaml` version, not an
app image tag. The chart expects an existing Secret and does not create
credentials, create the namespace and Secret first:

```bash
kubectl create namespace netbird
kubectl create secret generic netbird-audit \
  --namespace netbird \
  --from-literal=audit-token='nbp_your_token_here'
```

Then install the chart:

```bash
helm install auditbridge oci://ghcr.io/onelrian/charts/auditbridge \
  --version <chart-version> \
  --namespace netbird \
  --set envFromSecret.NETBIRD_API_TOKEN=netbird-audit/audit-token
```

Use a release name distinct from NetBird's `signal` component. See the
[chart README](../charts/auditbridge/README.md) for values and ServiceMonitor
configuration.

Confirm it started correctly:

```bash
kubectl -n netbird rollout status deployment/auditbridge
kubectl -n netbird logs deployment/auditbridge
kubectl -n netbird port-forward svc/auditbridge 9090:9090 &
curl http://localhost:9090/healthz   # "ok"
```
