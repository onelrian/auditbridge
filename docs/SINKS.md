# Sinks

Set `SINKS` to a comma-separated list. Each sink maintains its own cursor, so
a failing destination does not block successful deliveries to another one.
Every scenario below is verified end to end in
[examples/local-demo](../examples/local-demo/): the evidence blocks are real
output captured from that compose stack against a real Loki, real TCP and UDP
syslog receivers, and a real HTTP webhook receiver. Only the NetBird API is
stubbed to fixed sample data, no live account is involved.

## Grafana Loki

`loki` is enabled by default. Events are pushed to Loki's
`/loki/api/v1/push` endpoint as per-label-set streams with nanosecond
timestamps, which is what Loki requires to accept a batch at all.

| Variable | Default | Meaning |
|---|---|---|
| `LOKI_URL` | `http://loki:3100` | Loki base URL; the push path is appended |
| `SINK_LOKI_URL` | unset | Exact push URL, used verbatim when set (overrides `LOKI_URL`) |
| `SINK_LOKI_ENCODING` | `loki` | Keep the `loki` encoding unless you have a reason not to |

The Loki label set is fixed: `job="netbird-events"`, `account_id` (defaults to
`unknown` when missing), `activity`, and `activity_code`. The higher-cardinality
`initiator_*` fields stay in the JSON log line rather than as labels, so streams
fragment per activity, not per user.

### Worked example

```bash
docker run -d --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e SINKS=loki \
  -e LOKI_URL=https://loki.example.com \
  -p 9090:9090 \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

### Verify

Query Loki for the delivered stream:

```bash
docker compose exec loki wget -qO- \
  'http://localhost:3100/loki/api/v1/query_range?query=%7Bjob%3D%22netbird-events%22%7D'
```

Real response from the demo (three events, one stream per activity):

```json
{"status":"success","data":{"resultType":"streams","result":[
  {"stream":{"account_id":"acc-demo-01","activity":"User login","activity_code":"user.login","job":"netbird-events"},
   "values":[["1786538589000000000","{\"account_id\":\"acc-demo-01\",\"activity\":\"User login\",\"activity_code\":\"user.login\",\"event_id\":\"evt-1003\",\"initiator_email\":\"alice@example.com\",\"initiator_id\":\"user-alice\",\"initiator_name\":\"Alice Example\",\"meta\":null,\"target_id\":null,\"timestamp\":\"2026-08-12T12:43:09Z\"}"]]},
  {"stream":{"account_id":"acc-demo-01","activity":"Peer added","activity_code":"peer.add","job":"netbird-events"},
   "values":[["1786538581000000000","{\"account_id\":\"acc-demo-01\",\"activity\":\"Peer added\",\"activity_code\":\"peer.add\",\"event_id\":\"evt-1001\",\"initiator_email\":\"alice@example.com\",\"initiator_id\":\"user-alice\",\"initiator_name\":\"Alice Example\",\"meta\":{\"peer_name\":\"laptop-alice\"},\"target_id\":\"peer-7f3a\",\"timestamp\":\"2026-08-12T12:43:01Z\"}"]]},
  {"stream":{"account_id":"acc-demo-01","activity":"Group created","activity_code":"group.add","job":"netbird-events"},
   "values":[["1786538585000000000","{\"account_id\":\"acc-demo-01\",\"activity\":\"Group created\",\"activity_code\":\"group.add\",\"event_id\":\"evt-1002\",\"initiator_email\":\"bob@example.com\",\"initiator_id\":\"user-bob\",\"initiator_name\":\"Bob Example\",\"meta\":{\"group_name\":\"engineering\"},\"target_id\":\"group-eng\",\"timestamp\":\"2026-08-12T12:43:05Z\"}"]]}
]}}
```

In Grafana, add a Loki data source and query with LogQL, e.g. all audit events,
then filter to logins:

```logql
{job="netbird-events"}
{job="netbird-events"} |= "user.login"
```

## Wazuh

Add `wazuh` to `SINKS` and point it at the Wazuh manager's syslog listener.
The preset uses RFC 3164 framing over TCP, the combination most Wazuh
deployments accept from an agentless sender.

| Variable | Default | Meaning |
|---|---|---|
| `SINK_WAZUH_ADDR` / `WAZUH_ADDR` | none | Manager `host:port` (required) |
| `SINK_WAZUH_PROTOCOL` | `tcp` | `tcp` or `udp` |
| `SINK_WAZUH_ENCODING` | `syslog3164` | `syslog3164` or `syslog5424` |

### Worked example (TCP, the default)

```bash
docker run -d --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e SINKS=wazuh \
  -e SINK_WAZUH_ADDR=wazuh-manager:1514 \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

For UDP, add `-e SINK_WAZUH_PROTOCOL=udp` and set the manager's UDP listener
port in `SINK_WAZUH_ADDR`.

### Wazuh-side configuration

On the Wazuh manager, register a decoder and ruleset so delivered frames
become alerts. AuditBridge emits the event JSON as the MSG part with keys in
stable alphabetical order, e.g.:

```
<134>2026-08-12T12:43:01Z auditbridge netbird-audit: {"account_id":"acc-demo-01","activity":"Peer added","activity_code":"peer.add","event_id":"evt-1001",...}
```

`/var/ossec/etc/decoders/local_decoder.xml`:

```xml
<decoder name="netbird-audit">
  <prematch>auditbridge netbird-audit</prematch>
  <regex>auditbridge netbird-audit: \{"activity_code":"(\S+)"</regex>
  <order>status</order>
</decoder>
```

`/var/ossec/etc/rules/local_rules.xml`:

```xml
<group name="netbird,">
  <rule id="100200" level="3">
    <decoded_as>netbird-audit</decoded_as>
    <description>NetBird audit event delivered</description>
  </rule>
  <rule id="100201" level="8">
    <decoded_as>netbird-audit</decoded_as>
    <match>peer.add</match>
    <description>NetBird peer added</description>
  </rule>
  <rule id="100202" level="12">
    <decoded_as>netbird-audit</decoded_as>
    <match>user.login</match>
    <description>NetBird user login</description>
  </rule>
</group>
```

Restart the manager, then confirm alerts land in
`/var/ossec/logs/alerts/alerts.json` (`grep netbird-audit ...`). Adapt the
decoder's `<regex>` to your Wazuh version's regex flavour; the wire frames
below are the proof the manager is being fed.

### Verify

The demo's `wazuh-receiver` is a TCP listener standing in for the manager.
Real frames received (RFC 3164, one per event):

```
[syslog-tcp] <134>2026-08-12T12:43:01Z auditbridge netbird-audit: {"account_id":"acc-demo-01","activity":"Peer added","activity_code":"peer.add","event_id":"evt-1001","initiator_email":"alice@example.com","initiator_id":"user-alice","initiator_name":"Alice Example","meta":{"peer_name":"laptop-alice"},"target_id":"peer-7f3a","timestamp":"2026-08-12T12:43:01Z"}
[syslog-tcp] <134>2026-08-12T12:43:05Z auditbridge netbird-audit: {"account_id":"acc-demo-01","activity":"Group created","activity_code":"group.add","event_id":"evt-1002","initiator_email":"bob@example.com","initiator_id":"user-bob","initiator_name":"Bob Example","meta":{"group_name":"engineering"},"target_id":"group-eng","timestamp":"2026-08-12T12:43:05Z"}
[syslog-tcp] <134>2026-08-12T12:43:09Z auditbridge netbird-audit: {"account_id":"acc-demo-01","activity":"User login","activity_code":"user.login","event_id":"evt-1003","initiator_email":"alice@example.com","initiator_id":"user-alice","initiator_name":"Alice Example","meta":null,"target_id":null,"timestamp":"2026-08-12T12:43:09Z"}
```

## Generic HTTP sinks

Every sink name other than `loki` and `wazuh` is fully configurable: pick the
transport, the encoding, and the destination. HTTP is the `http` transport.

| Variable | Required for | Values |
|---|---|---|
| `SINK_<NAME>_TRANSPORT` | every generic sink | `http` or `syslog` |
| `SINK_<NAME>_ENCODING` | every generic sink | `json`, `ndjson`, `loki`, `syslog3164`, or `syslog5424` |
| `SINK_<NAME>_URL` | HTTP | Destination URL |
| `SINK_<NAME>_METHOD` | HTTP | HTTP method, default `POST` |
| `SINK_<NAME>_HEADERS` | HTTP | Comma-separated `Name:Value` headers |
| `SINK_<NAME>_HEADERS_FILE` | HTTP | File containing headers instead of the direct variable |

> [!TIP]
> Use the `_HEADERS_FILE` form for bearer tokens and API keys, mounted from a
> Docker or Kubernetes secret, instead of `SINK_<NAME>_HEADERS` directly.

### Worked example: a generic HTTP webhook

Deliver to Loki and a custom webhook at the same time, naming the second sink
`webhook` (any name works, it becomes the `SINK_<NAME>_*` prefix and the
`sink` label in its own metrics):

```bash
docker run -d --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e SINKS=loki,webhook \
  -e LOKI_URL=https://loki.example.com \
  -e SINK_WEBHOOK_TRANSPORT=http \
  -e SINK_WEBHOOK_URL=https://collector.example.com/ingest \
  -e SINK_WEBHOOK_ENCODING=ndjson \
  -e SINK_WEBHOOK_HEADERS="Authorization:Bearer your-webhook-token" \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

A replay of the full account history (fresh install, lost cursor) is split
into `BATCH_SIZE`-event requests, default 500, so payloads stay under intake
limits; see [Configuration](CONFIGURATION.md).

### Verify

The demo's `webhook-receiver` logs each request. Real delivery with
`ndjson` encoding and an `X-Demo` header:

```
[webhook-receiver] POST /ingest content-type=application/x-ndjson
[webhook-receiver] {"id":"evt-1001","timestamp":"2026-08-12T12:43:01Z","activity":"Peer added","activity_code":"peer.add","initiator_id":"user-alice","initiator_email":"alice@example.com","initiator_name":"Alice Example","target_id":"peer-7f3a","account_id":"acc-demo-01","meta":{"peer_name":"laptop-alice"}}
[webhook-receiver] {"id":"evt-1002","timestamp":"2026-08-12T12:43:05Z","activity":"Group created","activity_code":"group.add","initiator_id":"user-bob","initiator_email":"bob@example.com","initiator_name":"Bob Example","target_id":"group-eng","account_id":"acc-demo-01","meta":{"group_name":"engineering"}}
[webhook-receiver] {"id":"evt-1003","timestamp":"2026-08-12T12:43:09Z","activity":"User login","activity_code":"user.login","initiator_id":"user-alice","initiator_email":"alice@example.com","initiator_name":"Alice Example","target_id":null,"account_id":"acc-demo-01","meta":null}
```

## Generic syslog sinks

Syslog is the `syslog` transport. Any address, either protocol, either
framing; RFC 5424 for consumers confirmed to support it, RFC 3164 otherwise.

| Variable | Required for | Values |
|---|---|---|
| `SINK_<NAME>_TRANSPORT` | every generic sink | `http` or `syslog` |
| `SINK_<NAME>_ENCODING` | every generic sink | `syslog3164` or `syslog5424` |
| `SINK_<NAME>_ADDR` | syslog | Destination `host:port` |
| `SINK_<NAME>_PROTOCOL` | syslog | `tcp` or `udp`, default `tcp` |

### Worked example: RFC 5424 over UDP

```bash
docker run -d --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e SINKS=sysloggen \
  -e SINK_SYSLOGGEN_TRANSPORT=syslog \
  -e SINK_SYSLOGGEN_ADDR=collector.example:1515 \
  -e SINK_SYSLOGGEN_PROTOCOL=udp \
  -e SINK_SYSLOGGEN_ENCODING=syslog5424 \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

For TCP, drop `SINK_SYSLOGGEN_PROTOCOL` (TCP is the default) and point
`SINK_SYSLOGGEN_ADDR` at the TCP listener; the Wazuh section above shows the
RFC 3164-over-TCP variant in action.

### Verify

The demo's `wazuh-receiver` also listens on UDP 1515. Real frames received
(RFC 5424: `<PRI>VERSION TIMESTAMP HOSTNAME APP-NAME PROCID MSGID
STRUCTURED-DATA MSG`):

```
[syslog-udp] <134>1 2026-08-12T12:43:01Z auditbridge netbird-audit - AUDIT - {"account_id":"acc-demo-01","activity":"Peer added","activity_code":"peer.add","event_id":"evt-1001","initiator_email":"alice@example.com","initiator_id":"user-alice","initiator_name":"Alice Example","meta":{"peer_name":"laptop-alice"},"target_id":"peer-7f3a","timestamp":"2026-08-12T12:43:01Z"}
```

## Reproduce everything

`examples/local-demo/` runs all four scenarios at once: one `auditbridge`
instance with `SINKS=loki,wazuh` and a second, `auditbridge-generic`, with
`SINKS=webhook,sysloggen` covering the generic HTTP and generic syslog paths.
See [examples/local-demo/README.md](../examples/local-demo/README.md) for the
one-command setup and the verification commands whose output appears above.
