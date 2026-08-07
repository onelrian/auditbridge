# AuditBridge Helm Chart

Deploy AuditBridge beside a NetBird installation with a values layout aligned to
`netbirdio/helms`: `image`, `service`, `resources`, probes, `serviceAccount`,
`envFromSecret`, and `metrics.serviceMonitor`.

## Install

This chart does not create credentials. Create the namespace and the Secret
it references first:

```bash
kubectl create namespace netbird
kubectl create secret generic netbird-audit \
  --namespace netbird \
  --from-literal=audit-token='nbp_your_token_here'
```

Use a distinct release name from NetBird's `signal` component. AuditBridge was
renamed from Signal because the two components serve different purposes; using
the same release name or manually overridden resource names in one namespace
can make their Kubernetes resources collide.

```bash
helm install auditbridge oci://ghcr.io/onelrian/charts/auditbridge \
  --version 0.1.0 \
  --namespace netbird \
  --set envFromSecret.NETBIRD_API_TOKEN=netbird-audit/audit-token
```

Use `envFromSecret` for every credential. It maps each environment-variable
name to an existing Kubernetes Secret reference in `secretName/secretKey`
format. `SINK_WEBHOOK_HEADERS` below is only relevant if you've added a
`webhook` sink, see [docs/SINKS.md](../../docs/SINKS.md), and needs its own
Secret created the same way:

```bash
kubectl create secret generic auditbridge-webhook \
  --namespace netbird \
  --from-literal=headers='Authorization:Bearer your-webhook-token'
```

```yaml
envFromSecret:
  NETBIRD_API_TOKEN: netbird-audit/audit-token
  SINK_WEBHOOK_HEADERS: auditbridge-webhook/headers
```

## Metrics

The Service exposes `/healthz`, `/readyz`, and `/metrics`. The application port
is `metrics.port`; `service.port` only controls the Service's client-facing
port. Enable the ServiceMonitor only when the Prometheus Operator CRD is installed:

```yaml
metrics:
  serviceMonitor:
    enabled: true
    labels:
      release: kube-prometheus-stack
```
