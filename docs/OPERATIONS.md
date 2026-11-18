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

AuditBridge's own errors are already shown at the default `info` level;
`RUST_LOG=debug` adds the underlying HTTP client's request/response detail
(useful for TLS or connection-level issues), not additional application
detail. Every failure retries with backoff before it's logged as an error, a
single warning is not itself a problem.

| Symptom | Check | Resolution |
|---|---|---|
| `/readyz` is `503`, log shows `Failed to fetch events from Netbird: ... 401` | `NETBIRD_API_TOKEN`/`NETBIRD_API_TOKEN_FILE` | Token is wrong, expired, or revoked. Create a new one, see [Installation](INSTALLATION.md#get-a-netbird-access-token) |
| `/readyz` is `503`, log shows a connection error (timeout, refused, DNS) | `NETBIRD_API_URL` and network/firewall rules to that host | Fix the URL or allow outbound access to it, self-hosted NetBird in particular often needs an explicit `NETBIRD_API_URL` |
| `/readyz` is `503` but fetch succeeds in the logs | Every configured sink's own connectivity (see the next row) | At least one sink must deliver successfully; fix the nearest one |
| `Failed to send N events to <sink>: ...` for one sink only | That sink's URL/address, credentials, and `auditbridge_delivery_errors_total{sink="<name>"}` | Other sinks are unaffected, each has its own cursor. Fix that sink's config and it resumes on the next cycle |
| Events repeat after a restart | Whether `CURSOR_FILE` is set and its path is on a volume that survives restart, not just the container's writable layer | Set `CURSOR_FILE` and mount a real volume or PVC, see [Configuration](CONFIGURATION.md). Repeated events can also be plain at-least-once delivery at the watermark: a crash between a successful send and the cursor save, or the first poll after upgrading from a version that stored only a timestamp, re-delivers the events that share the newest delivered timestamp |
| `auditbridge_events_fetched_total` grows every poll but `..._delivered_total` barely moves | Cursor is working as intended, NetBird returns the full account history every poll, only new-since-cursor events get delivered | Not a bug, see [Configuration](CONFIGURATION.md) for why NetBird's API works this way |
| Loki shows an event at the Unix epoch (1970-01-01) and the log warns `has an unparseable timestamp ... encoding it at the Unix epoch` | That event's `timestamp` in the NetBird response is not a parseable RFC 3339 value, so it cannot be placed in audit time | Not data loss: the event is still delivered, at the epoch, with its original timestamp preserved in the log line. Report the bad value upstream to NetBird; the fallback is deterministic by design, so the same event always lands in the same place |
| A burst of duplicate events immediately after upgrading from a version without boundary IDs | The cursor file still holds a bare timestamp per sink | Expected once, by design: the entry loads with no boundary IDs, so the events at that watermark are re-sent one final time. It does not recur on later polls or restarts |

## Rollback

Roll back to the last known-good immutable image or Helm chart version. Preserve
the cursor volume when rolling back to avoid replaying already delivered events.

A version older than the boundary-ID cursor format cannot read the newer cursor
file: it warns that the file is unreadable and starts from an empty cursor, which
replays the account's full audit history into every sink. Keep the volume so you
can roll forward again rather than expecting the older binary to resume from it,
and expect that replay for as long as you stay on it.
