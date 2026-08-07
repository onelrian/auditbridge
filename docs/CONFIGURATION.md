# Configuration

AuditBridge uses environment variables. A direct secret variable and its
`_FILE` equivalent are mutually exclusive. File-backed values are trimmed.

| Variable | Default | Purpose |
|---|---|---|
| `NETBIRD_API_TOKEN` | none | NetBird token with audit-log access |
| `NETBIRD_API_TOKEN_FILE` | none | File containing the NetBird token |
| `NETBIRD_API_URL` | `https://api.netbird.io` | NetBird API base URL |
| `SINKS` | `loki` | Comma-separated sink names |
| `CHECK_INTERVAL` | `10` | Poll interval in seconds |
| `RETRY_MAX_ATTEMPTS` | `5` | Attempts per fetch or delivery cycle |
| `RETRY_BASE_DELAY_MS` | `500` | Full-jitter retry backoff base in milliseconds |
| `RETRY_MAX_DELAY_MS` | `30000` | Retry backoff maximum in milliseconds |
| `CURSOR_FILE` | unset | Persistent per-sink delivery watermark path |
| `METRICS_PORT` | `9090` | Health and Prometheus HTTP server port |
| `RUST_LOG` | `info` | Rust log filter |

`CURSOR_FILE` must be on durable storage to survive a container or pod
replacement. Without it, AuditBridge reprocesses the account audit history
after each restart. NetBird’s audit endpoint returns the full history and does
not expose server-side paging or cursor parameters.

See [Sinks](SINKS.md) for sink-specific variables.
