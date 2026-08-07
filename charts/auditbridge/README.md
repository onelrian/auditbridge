# AuditBridge Helm Chart

Deploy AuditBridge beside a NetBird installation with a values layout aligned to
`netbirdio/helms`: `image`, `service`, `resources`, probes, `serviceAccount`,
`envFromSecret`, and `metrics.serviceMonitor`.

## Install

Use a distinct release name from NetBird's `signal` component. AuditBridge was
renamed from Signal because the two components serve different purposes; using
the same release name or manually overridden resource names in one namespace
can make their Kubernetes resources collide.

```bash
helm install auditbridge oci://ghcr.io/onelrian/charts/auditbridge \
  --version 0.1.0 \
  --namespace netbird \
  --create-namespace \
  --set envFromSecret.NETBIRD_API_TOKEN=netbird-audit/audit-token
```

Use `envFromSecret` for every credential. It maps each environment-variable
name to an existing Kubernetes Secret reference in `secretName/secretKey`
format.

```yaml
envFromSecret:
  NETBIRD_API_TOKEN: netbird-audit/audit-token
  SINK_WEBHOOK_HEADERS: auditbridge-webhook/headers
```

## Metrics

The Service exposes `/healthz`, `/readyz`, and `/metrics` on port 9090. Enable
the ServiceMonitor only when the Prometheus Operator CRD is installed:

```yaml
metrics:
  serviceMonitor:
    enabled: true
    labels:
      release: kube-prometheus-stack
```
