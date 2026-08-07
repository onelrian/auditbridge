# Sinks

Set `SINKS` to a comma-separated list. Each sink maintains its own cursor, so
a failing destination does not block successful deliveries to another one.

## Loki

`loki` is enabled by default. Set `LOKI_URL` to a Loki base URL or use
`SINK_LOKI_URL` for an exact push URL.

## Wazuh

Add `wazuh` to `SINKS` and set `SINK_WAZUH_ADDR` or `WAZUH_ADDR` to the Wazuh
manager `host:port`. The default syslog encoding is RFC3164.

## Generic sinks

For every other sink name, use these variables with `<NAME>` converted to upper
case and underscores:

| Variable | Required for | Values |
|---|---|---|
| `SINK_<NAME>_TRANSPORT` | every generic sink | `http` or `syslog` |
| `SINK_<NAME>_ENCODING` | every generic sink | `json`, `ndjson`, `loki`, `syslog3164`, or `syslog5424` |
| `SINK_<NAME>_URL` | HTTP | Destination URL |
| `SINK_<NAME>_METHOD` | HTTP | HTTP method, default `POST` |
| `SINK_<NAME>_HEADERS` | HTTP | Comma-separated `Name:Value` headers |
| `SINK_<NAME>_HEADERS_FILE` | HTTP | File containing headers instead of the direct variable |
| `SINK_<NAME>_ADDR` | syslog | Destination `host:port` |
| `SINK_<NAME>_PROTOCOL` | syslog | `tcp` or `udp`, default `tcp` |

Use the `_HEADERS_FILE` form for bearer tokens and API keys.
