# Operations

See the [Verified section of the root README](../README.md#verified) for a
real, reproducible end-to-end run of everything on this page.

## Health and metrics

The HTTP server exposes these endpoints on `METRICS_PORT`:

| Endpoint | Meaning |
|---|---|
| `/healthz` | Process is running |
| `/readyz` | NetBird has been reached and at least one sink has delivered an event |
| `/metrics` | Prometheus metrics |

Prometheus metrics include fetched and delivered event totals, fetch and
delivery error totals, and the last successful poll timestamp. Delivery metrics
are labelled by sink.

## Troubleshooting

| Symptom | Check | Resolution |
|---|---|---|
| `/readyz` is `503` | NetBird token, API URL, and sink reachability | Correct credentials or connectivity, then wait for a successful poll and delivery |
| Events repeat after restart | `CURSOR_FILE` durability | Mount persistent storage or accept replay behavior |
| One sink falls behind | Sink errors in logs and metrics | Repair that sink; other sinks continue independently |
| Fetch latency grows | Account audit-history size | NetBird returns the full audit history each poll; reduce polling pressure or retain a cursor |

## Rollback

Roll back to the last known-good immutable image or Helm chart version. Preserve
the cursor volume when rolling back to avoid replaying already delivered events.
