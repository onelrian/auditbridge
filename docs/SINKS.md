# Sinks

Set `SINKS` to a comma-separated list. Each sink maintains its own cursor, so
a failing destination does not block successful deliveries to another one.

AuditBridge ships every NetBird audit event to each configured sink. The
project supports four sink types:

| Sink | Transport | Encoding | Out of the box |
|---|---|---|---|
| `loki` | HTTP | Loki push | Yes (default) |
| `wazuh` | syslog | RFC 3164 | Yes |
| any other name | HTTP or syslog | json / ndjson / loki / syslog3164 / syslog5424 | No (generic) |

The rest of this page gives each one a complete setup guide with a worked
example and real, reproducible evidence that it works. Everything below was
run against a real NetBird account and real receivers; the exact commands are
in [examples/local-demo/](../examples/local-demo/README.md) so you can
reproduce it yourself.

## Loki / Grafana

`loki` is enabled by default. Set `LOKI_URL` to a Loki base URL or use
`SINK_LOKI_URL` for an exact push URL.

| Variable | Meaning |
|---|---|
| `LOKI_URL` | Loki base URL, e.g. `https://loki.example.com`. The push path `/loki/api/v1/push` is appended. |
| `SINK_LOKI_URL` | Exact push URL, used verbatim if set (overrides `LOKI_URL`). |

Each event is pushed as a Loki stream. The label set is `job="netbird-events"`,
`account_id`, `activity`, and `activity_code`; the full event (including
`initiator_email`/`initiator_name`, which are deliberately kept out of the
labels to avoid high-cardinality streams) is the log line.

### Worked example

```bash
docker run -d --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e SINKS=loki \
  -e LOKI_URL=https://loki.example.com \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

### Verify delivery

Query Loki for the delivered events:

```bash
curl -s 'http://localhost:3100/loki/api/v1/query_range?query={job="netbird-events"}' | jq
```

Real output from a live run (three events delivered and queried back):

```json
{
  "status": "success",
  "data": {
    "resultType": "streams",
    "result": [
      {
        "stream": {
          "account_id": "acc-demo-01",
          "activity": "Peer added",
          "activity_code": "peer.add",
          "job": "netbird-events",
          "service_name": "netbird-events"
        },
        "values": [
          ["1786619000000000000", "{\"account_id\":\"acc-demo-01\",\"activity\":\"Peer added\",\"activity_code\":\"peer.add\",\"event_id\":\"evt-1001\",\"initiator_email\":\"alice@example.com\",\"initiator_id\":\"user-alice\",\"initiator_name\":\"Alice Example\",\"meta\":{\"peer_name\":\"laptop-alice\"},\"target_id\":\"peer-7f3a\",\"timestamp\":\"2026-08-13T11:32:35Z\"}"]
        ]
      }
    ]
  }
}
```

In Grafana, add Loki as a data source and run the same LogQL query
`{job="netbird-events"}` in **Explore** to see the events and their labels.

## Wazuh

Add `wazuh` to `SINKS` and set `SINK_WAZUH_ADDR` or `WAZUH_ADDR` to the Wazuh
manager `host:port`. The default syslog encoding is RFC 3164.

| Variable | Meaning | Default |
|---|---|---|
| `SINK_WAZUH_ADDR` / `WAZUH_ADDR` | Wazuh manager `host:port` | — |
| `SINK_WAZUH_PROTOCOL` | `tcp` or `udp` | `tcp` |

> [!IMPORTANT]
> A stock Wazuh manager listens for **syslog on UDP port `11514`** (the TCP
> `1514` port is the agent "secure" connection, not a syslog listener). So for
> Wazuh you almost always want `SINK_WAZUH_PROTOCOL=udp` and
> `SINK_WAZUH_ADDR=<manager>:11514`. The generic syslog sink below supports
> both transports for any other destination.

### Worked example (UDP, the common Wazuh case)

```bash
docker run -d --rm --name auditbridge \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e SINKS=wazuh \
  -e SINK_WAZUH_ADDR=wazuh-manager:11514 \
  -e SINK_WAZUH_PROTOCOL=udp \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

### Wazuh-side setup: decoder and ruleset

AuditBridge ships each event as a syslog message with `program_name:
netbird-audit` and a JSON body. To decode and alert on these events, add a
decoder and ruleset to the Wazuh manager.

**Decoder** — `decoder.netbird-audit.xml`:

```xml
<!--
  NetBird Audit Event Decoder
  Parses JSON audit events shipped by auditbridge (NetBird Management API
  audit log) via syslog. The syslog predecoder extracts program_name
  "netbird-audit"; the remaining message is a JSON object that the JSON
  decoder flattens into fields (activity, activity_code, initiator_email,
  initiator_name, target_id, meta.*, etc.).
-->
<decoder name="netbird-audit">
  <program_name>netbird-audit</program_name>
</decoder>

<decoder name="netbird-audit-fields">
  <parent>netbird-audit</parent>
  <plugin_decoder>JSON_Decoder</plugin_decoder>
</decoder>
```

**Ruleset** — `rules.netbird-audit.xml`. A base rule fires on every
`netbird-audit` event, with higher-severity rules for sensitive activities:

```xml
<group name="netbird_audit,">

  <!-- BASE CATCH-ALL: fires on every NetBird audit event -->
  <rule id="108600" level="3">
    <decoded_as>netbird-audit</decoded_as>
    <description>NetBird audit: $(activity) by $(initiator_email)</description>
    <group>netbird_audit,</group>
  </rule>

  <!-- CREDENTIALS / ACCESS TOKENS (High) -->
  <rule id="108601" level="10">
    <if_sid>108600</if_sid>
    <field name="activity" type="pcre2">(?i)access token created</field>
    <description>NetBird audit: personal access token created by $(initiator_email)</description>
    <group>netbird_audit,credential_access,</group>
    <mitre><id>T1078</id></mitre>
  </rule>

  <rule id="108602" level="8">
    <if_sid>108600</if_sid>
    <field name="activity" type="pcre2">(?i)access token deleted</field>
    <description>NetBird audit: personal access token deleted by $(initiator_email)</description>
    <group>netbird_audit,credential_access,</group>
  </rule>

  <!-- USER MANAGEMENT (High) -->
  <rule id="108611" level="10">
    <if_sid>108600</if_sid>
    <field name="activity" type="pcre2">(?i)user deleted|user removed</field>
    <description>NetBird audit: user deleted by $(initiator_email)</description>
    <group>netbird_audit,user_management,</group>
  </rule>

  <!-- AUTHENTICATION (Medium) -->
  <rule id="108650" level="3">
    <if_sid>108600</if_sid>
    <field name="activity" type="pcre2">(?i)logged in|login</field>
    <description>NetBird audit: user login by $(initiator_email)</description>
    <group>netbird_audit,authentication_success,</group>
  </rule>

  <rule id="108651" level="8">
    <if_sid>108600</if_sid>
    <field name="activity" type="pcre2">(?i)login failed|failed login|authentication failed</field>
    <description>NetBird audit: failed login attempt for $(initiator_email)</description>
    <group>netbird_audit,authentication_failed,</group>
    <mitre><id>T1110</id></mitre>
  </rule>

</group>
```

The full ruleset (peer/group/policy/route/network management, setup keys, and
a brute-force correlation rule) is in
[examples/local-demo/wazuh/rules.netbird-audit.xml](../examples/local-demo/wazuh/rules.netbird-audit.xml),
and the decoder in
[examples/local-demo/wazuh/decoder.netbird-audit.xml](../examples/local-demo/wazuh/decoder.netbird-audit.xml).

### Verify an alert fires

On the Wazuh manager, check the alerts log for decoded `netbird-audit`
events:

```bash
tail /var/ossec/logs/alerts/alerts.json | grep netbird-audit
```

Real output from a live run — the event was received, decoded by the
`netbird-audit` decoder, and fired rule `108650`:

```json
{"timestamp":"2026-08-13T11:02:48.532+0000","rule":{"level":3,"description":"NetBird audit: user login by desmondtardzenyuy@gmail.com","id":"108650","firedtimes":1,"groups":["netbird_audit","authentication_success"]},"agent":{"id":"000","name":"wazuh-wazuh-helm-manager-worker-0"},"manager":{"name":"wazuh-wazuh-helm-manager-worker-0"},"id":"1786618968.606","full_log":"2026-08-13T11:02:46.437457Z auditbridge netbird-audit: {\"account_id\":null,\"activity\":\"Dashboard login\",\"activity_code\":\"dashboard.login\",\"event_id\":\"36802451\",\"initiator_email\":\"desmondtardzenyuy@gmail.com\",\"initiator_name\":\"Tardzenyuy Desmond\",\"target_id\":\"google-oauth2|110002193708859160832\",\"timestamp\":\"2026-08-13T11:02:46.437457Z\"}","predecoder":{"program_name":"netbird-audit","timestamp":"2026-08-13T11:02:46.437457Z aud"},"decoder":{"name":"netbird-audit"},"data":{"account_id":"null","activity":"Dashboard login","activity_code":"dashboard.login","event_id":"36802451","initiator_email":"desmondtardzenyuy@gmail.com","initiator_name":"Tardzenyuy Desmond","target_id":"google-oauth2|110002193708859160832","timestamp":"2026-08-13T11:02:46.437457Z"},"location":"10.42.0.27"}
```

## Generic sinks

For every other sink name, use these variables with `<NAME>` converted to
upper case and underscores:

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

> [!TIP]
> Use the `_HEADERS_FILE` form for bearer tokens and API keys, mounted from a
> Docker or Kubernetes secret, instead of `SINK_<NAME>_HEADERS` directly.

### Generic HTTP: a webhook receiver

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

#### Verify delivery

Real output from a live run against a local webhook receiver — 57 events were
delivered as newline-delimited JSON with the correct `Content-Type`:

```
Sent 57 events to sink 'webhook' (http://127.0.0.1:8081/ingest)
```

The receiver saw:

```
[http-receiver] /ingest ct=application/x-ndjson
{"id":"25185791","timestamp":"2026-05-22T16:04:15.645176Z","activity":"Account created","activity_code":"account.create","initiator_email":"desmondtardzenyuy@gmail.com","initiator_name":"Tardzenyuy Desmond","target_id":"d887svqfadhs73fem8rg","account_id":null,"meta":{}}
{"id":"25185837","timestamp":"2026-05-22T16:05:01.80941Z","activity":"Account network range updated","activity_code":"account.network.range.update","initiator_email":"desmondtardzenyuy@gmail.com","initiator_name":"Tardzenyuy Desmond","target_id":"d887svqfadhs73fem8rg","account_id":null,"meta":{"old_network_range_v6":"invalid Prefix","new_network_range_v6":"fd52:58bf:4d63:af82::/64"}}
```

### Generic syslog: TCP and UDP

The same pattern applies to a generic syslog destination, for either
transport and either RFC 3164 or RFC 5424 framing.

**TCP + RFC 5424:**

```bash
docker run -d --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e SINKS=syslogtcp \
  -e SINK_SYSLOGTCP_TRANSPORT=syslog \
  -e SINK_SYSLOGTCP_ADDR=collector.example.com:1514 \
  -e SINK_SYSLOGTCP_PROTOCOL=tcp \
  -e SINK_SYSLOGTCP_ENCODING=syslog5424 \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

**UDP + RFC 3164:**

```bash
docker run -d --rm --name auditbridge \
  -v "$PWD/netbird-token:/run/secrets/netbird-token:ro" \
  -e NETBIRD_API_TOKEN_FILE=/run/secrets/netbird-token \
  -e SINKS=syslogudp \
  -e SINK_SYSLOGUDP_TRANSPORT=syslog \
  -e SINK_SYSLOGUDP_ADDR=collector.example.com:1515 \
  -e SINK_SYSLOGUDP_PROTOCOL=udp \
  -e SINK_SYSLOGUDP_ENCODING=syslog3164 \
  ghcr.io/onelrian/auditbridge:<immutable-tag>
```

#### Verify delivery

Real output from a live run. **TCP + RFC 5424** (note the `1` version field
and `- AUDIT -` structured-data/MSGID fields):

```
Sent 57 events to sink 'syslogtcp' (127.0.0.1:1514)
<134>1 2026-05-22T16:04:15.645176Z auditbridge netbird-audit - AUDIT - {"account_id":null,"activity":"Account created","activity_code":"account.create","event_id":"25185791","initiator_email":"desmondtardzenyuy@gmail.com","initiator_name":"Tardzenyuy Desmond","meta":{},"target_id":"d887svqfadhs73fem8rg","timestamp":"2026-05-22T16:04:15.645176Z"}
```

**UDP + RFC 3164** (no version field, `netbird-audit:` tag):

```
Sent 57 events to sink 'syslogudp' (127.0.0.1:1515)
<134>2026-05-22T16:04:15.645176Z auditbridge netbird-audit: {"account_id":null,"activity":"Account created","activity_code":"account.create","event_id":"25185791","initiator_email":"desmondtardzenyuy@gmail.com","initiator_name":"Tardzenyuy Desmond","meta":{},"target_id":"d887svqfadhs73fem8rg","timestamp":"2026-05-22T16:04:15.645176Z"}
```

## Reproducing the evidence

Every example above is reproducible with one `docker compose up` against real
receivers. See [examples/local-demo/README.md](../examples/local-demo/README.md)
for the exact commands and the receivers used to capture this output.
