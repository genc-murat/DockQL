# Docker Observability Language v0.1 Specification

Status: draft v0.1

DOL is a domain-specific language for querying, observing, and analyzing Docker infrastructure. Version 0.1 intentionally defines a small, implementable core that can be parsed into a typed AST and executed by a Rust CLI.

## 1. Design Goals

DOL treats Docker as a live data system:

- Docker entities are query targets.
- Container metrics are time-oriented records.
- Docker events are streams.
- Alerts are continuously evaluated queries.
- Historical inspection is a query over stored telemetry.

DOL improves the day-to-day workflow where users otherwise combine `docker ps`, `docker stats`, `docker events`, shell scripts, Prometheus queries, and dashboard tools. The language does not replace Prometheus or Grafana; it provides a Docker-native query surface that can later export to those systems.

Version 0.1 prioritizes:

- Simple syntax.
- Strong parser boundaries.
- Streaming support.
- A realistic Rust implementation path.
- Deterministic behavior before AI-assisted insights.

## 2. Query Families

DOL v0.1 has five top-level query families.

### 2.1 `observe`

`observe` reads the current state of Docker entities and optionally enriches containers with current metrics.

Execution mode: batch snapshot by default, hybrid when metric windows are requested.

Examples:

```dol
observe containers
observe containers where status = running
observe containers | where image contains "postgres" | select name, status, ports
```

### 2.2 `events`

`events` subscribes to Docker event streams or reads historical events when a time range is present.

Execution mode: stream by default, historical when `last`, `from`, or `to` is present and a telemetry store is configured.

Examples:

```dol
events containers
events containers where action = "die"
events containers | where action = "restart" | select time, container, image
```

### 2.3 `inspect`

`inspect` reads a detailed entity snapshot. With `at`, it reads a historical snapshot from storage.

Execution mode: batch for current inspection, historical for `at`.

Examples:

```dol
inspect container api-service
inspect image postgres:16
inspect container api-service at "2026-01-01 12:00:00"
```

### 2.4 `analyze`

`analyze` runs deterministic analysis over current or stored Docker telemetry.

Execution mode: batch or historical depending on time qualifiers.

Examples:

```dol
analyze containers find anomalies
analyze containers find restart_loops last 10m
analyze container api-service correlate events last 1h
```

### 2.5 `fields`

`fields` returns the schema (field names and types) for a given entity type. This is useful for discovery and tooling integration.

Execution mode: batch.

Examples:

```dol
fields containers
fields images
fields networks
fields volumes
```

`alert` defines a continuously evaluated condition and an action.

Execution mode: stream or scheduled evaluation loop.

Alert actions are executed in real time:
- **print**: Outputs a formatted message to stdout.
- **webhook**: Sends an HTTP POST to the specified URL. Requires network access.
- **restart**: Runs `docker restart <container>` to restart the target container.

When `--store` is active, all fired alerts are persisted to the `alert_history` table for audit and review.

Examples:

```dol
alert when cpu > 85% for 2m then print "High CPU"
alert when restart_count > 3 for 5m then print "Restart loop detected"
alert when memory > 90% for 1m then webhook "http://localhost:9000/hooks/docker"
alert when restart_count > 5 for 3m then restart container api-service
```

## 3. Targets

Top-level collection targets:

- `containers`
- `images`
- `networks`
- `volumes`

Singular inspection targets:

- `container <name-or-id>`
- `image <name-or-id>`
- `network <name-or-id>`
- `volume <name-or-id>`

Target names are case-sensitive when they refer to Docker names or IDs. DOL keywords are lowercase in v0.1.

## 4. Fields

### 4.1 Container Fields

Common container fields:

- `id`
- `name`
- `image`
- `status`
- `state`
- `ports`
- `labels`
- `created_at`
- `started_at`
- `finished_at`
- `restart_count`
- `cpu`
- `memory`
- `memory_limit`
- `network_rx`
- `network_tx`
- `disk_read`
- `disk_write`
- `compose_project`

Individual labels can be accessed with dot notation. For example, given a container
with label `com.docker.compose.project=myapp`:

```dol
observe containers where label.com.docker.compose.project = "myapp"
observe containers | where label.env = "production" | select name, label.version
```

### 4.2 Image Fields

Common image fields:

- `id`
- `repository`
- `tag`
- `digest`
- `size`
- `created_at`
- `labels`

### 4.3 Network Fields

Common network fields:

- `id`
- `name`
- `driver`
- `scope`
- `containers`
- `labels`

### 4.4 Volume Fields

Common volume fields:

- `name`
- `driver`
- `mountpoint`
- `scope`
- `labels`

### 4.5 Event Fields

Common event fields:

- `time`
- `type`
- `action`
- `actor_id`
- `container`
- `image`
- `attributes`

## 4.6 Dynamic Fields via `set`

The `set` pipeline node adds or overrides a field on each row. The value can be:

- A **literal**: `set tier = "prod"`
- An **expression**: `set mem_gb = memory / 1073741824` (arithmetic, function calls, field references)
- An **if/else expression**: `set health = if state = running then "up" else "down"`
- A **case/when expression**: `set severity = case when cpu > 80% then "critical" else "ok" end`

Examples:

```dol
observe containers | set tier = "prod"
observe containers | set mem_gb = memory / 1073741824 | select name, mem_gb
observe containers | set label = upper(name) | select name, label
observe containers | set health = if state = running then "up" else "down"
observe containers | set severity = case
    when cpu > 80% then "critical"
    when cpu > 50% then "warning"
    else "ok"
  end | select name, severity
```

## 5. Literals and Types

DOL v0.1 supports these literal types:

- String: `"api-service"`
- Bare identifier: `running`, `api-service`, `postgres`
- Integer: `42`
- Float: `0.75`
- Percentage: `85%`
- Duration: `30s`, `5m`, `1h`, `2d`
- Timestamp string: `"2026-01-01 12:00:00"`
- Boolean: `true`, `false`

Bare identifiers are accepted for simple values in filters, but strings are recommended when a value contains punctuation, spaces, or mixed case.

### 5.1 Static Semantic Analysis & Type Safety

DOL implements a static semantic analyzer and type safety validation pass before executing any query. This phase runs immediately after parsing and prevents execution of queries that contain structural or type errors:

- **Field Existence Validation**: All referenced fields in filters, projections (`select`), and aggregations (`group by`) are checked against the static schema of the collection target (e.g., containers, images). Dynamically added fields via `set` are added to the active schema context and allowed in downstream pipeline nodes.
- **Label Prefix Support**: Dynamically resolved label lookups using the `label.` prefix (e.g., `label.env`) are recognized and statically validated as long as the base `labels` field exists on the target.
- **Type Compatibility Checking**: High-level comparison and arithmetic operations validate that their operands are compatible. For example:
  - Comparing a String field (e.g., `state`) to an Integer literal (e.g., `50`) with binary comparison operators like `>` will be rejected at compilation/validation time.
  - Performing arithmetic operations on non-numeric types is statically rejected.

## 6. Operators

### 6.1 Comparison Operators

- `=`
- `!=`
- `>`
- `<`
- `>=`
- `<=`

### 6.2 String and Pattern Operators

- `contains` — substring match
- `matches` — regex match (Rust regex syntax)
- `in` — set membership: `expr in ("a", "b", "c")`

### 6.3 Range and Null Operators

- `between ... and ...` — numeric range check: `cpu between 50 and 80`
- `is null` — null check: `finished_at is null`
- `is not null` — not-null check: `finished_at is not null`

### 6.4 Arithmetic Operators

- `+` — addition
- `-` — subtraction (also unary minus: `-5`)
- `*` — multiplication
- `/` — division
- `%` — modulo

Arithmetic expressions can be used in `set` assignments, `where` filters, `having` filters, and anywhere an expression is expected.

### 6.5 Boolean Operators

- `and`
- `or`
- `not`

### 6.6 Function Calls

- `upper(s)` — uppercase
- `lower(s)` — lowercase
- `length(s)` — string length
- `trim(s)` — trim whitespace
- `concat(a, b, ...)` — string concatenation
- `substring(s, start, len)` — substring extraction
- `coalesce(a, b, ...)` — first non-null value

### 6.7 Coalesce Function

The `coalesce()` function returns the first non-null, non-empty value from its arguments:

```dol
observe containers | set name = coalesce(label.name, name, "unknown") | select name
```

If all arguments are null or empty strings, `coalesce()` returns `null`.

### 6.8 Operator Precedence

Precedence, highest to lowest:

1. Parentheses: `( ... )`
2. Function calls, unary `-`
3. Arithmetic: `*`, `/`, `%`
4. Arithmetic: `+`, `-`
5. Comparison, `contains`, `matches`, `between`, `is null`, `is not null`
6. `not`
7. `and`
8. `or`

Example:

```dol
observe containers where status = running and (cpu > 80% or memory > 90%)
observe containers where image in ("postgres", "mysql", "redis")
observe containers where name matches "^api-"
observe containers where cpu between 50 and 80
observe containers | where finished_at is not null | select name
observe containers | set mem_gb = memory / 1073741824 | select name, mem_gb
observe containers | where upper(name) contains "API" | select name
```

## 7. Time Syntax

DOL v0.1 supports relative windows and point-in-time inspection.

Relative windows:

```dol
last 5m
last 1h
last 2d
```

Point-in-time:

```dol
at "2026-01-01 12:00:00"
```

Historical ranges are reserved for v0.2 but may be accepted by the parser in v0.1 as experimental:

```dol
from "2026-01-01 12:00:00" to "2026-01-01 13:00:00"
```

Time semantics:

- Without a time clause, `observe` reads the current Docker snapshot.
- Without a time clause, `events` opens a live stream.
- `last <duration>` means the interval ending at query evaluation time.
- `at <timestamp>` means the nearest stored snapshot at or before that timestamp.
- Timestamps are interpreted in the local runtime timezone unless a timezone suffix is provided in a future version.

## 8. Pipeline Syntax

Pipelines transform query output from left to right.

Supported pipeline nodes:

- `where <expression>`
- `select <field-list>`
- `group by <field-list> [with <agg>(<field>) as <alias> [, ...]]`
- `having <expression>`
- `sort by <field> [asc|desc] [, <field> [asc|desc] ...]`
- `limit <integer>`
- `offset <integer>`
- `distinct`
- `alert <string>`
- `set <field> = <value-expr>`
- `if <condition> then <pipeline-node> [else if <condition> then <pipeline-node>] [else <pipeline-node>]`

Pipeline rules:

- The first expression must be a query family such as `observe containers`.
- Each pipe receives rows or events from the previous stage.
- `where` filters records.
- `select` changes output shape.
- `group by` aggregates records by field values. When `with` is given, the specified aggregate functions (sum, count, avg, min, max) are computed per group with optional `as` aliases.
- `having` filters groups after aggregation (similar to `where` but operates on aggregate values).
- `sort by` supports multiple sort fields with independent direction per field.
- `limit` stops after N records.
- `offset` skips the first N records.
- `distinct` removes duplicate rows (same values across all fields).
- `alert` inside a pipeline emits an alert when a record reaches that stage.
- `set <field> = <value>` adds or overrides a field on each row. The value can include arithmetic expressions, function calls, and field references.
- `if <condition> then <nodes> [else <nodes>]` conditionally applies nested pipeline nodes.

Examples:

```dol
observe containers | where cpu > 80% | select name, cpu, memory
observe images | sort by size desc | limit 10
events containers | where action = "die" | group by image
observe containers | set health = if state = running then "healthy" else "unhealthy"
observe containers | if cpu > 90% then alert "Critical CPU" else alert "OK"
observe containers | set severity = case when cpu > 80% then "critical" else "ok" end
observe containers | set mem_gb = memory / 1073741824 | select name, mem_gb
observe containers | sort by state desc, cpu desc | select name, state, cpu
observe containers | distinct | select image
observe containers | sort by name asc | offset 10 | limit 5
observe containers | group by image with count(id) as cnt | having cnt > 3
observe containers | group by image with avg(cpu) as avg_cpu | sort by avg_cpu desc
```

## 10. Execution Semantics

### 10.1 Batch Queries

Batch queries consume finite input and return finite output.

Batch examples:

```dol
observe containers
observe images | sort by size desc | limit 5
inspect container api-service
```

Batch execution steps:

1. Parse query into AST.
2. Build logical plan.
3. Read Docker snapshot or stored snapshot.
4. Apply filters and pipeline stages.
5. Render output as table or JSON.

### 10.2 Stream Queries

Stream queries consume unbounded input and keep running until cancelled.

Stream examples:

```dol
events containers
events containers | where action = "die"
alert when cpu > 85% for 2m then print "High CPU"
```

Stream execution rules:

- Stream output is record-by-record.
- `limit` may terminate a stream after N matching records.
- `sort by` is invalid on unbounded streams unless a finite window is present.
- Alerts keep state per entity when evaluating `for <duration>`.
- Ctrl+C should cancel stream execution cleanly.

### 10.3 Hybrid Queries

Hybrid queries combine a current Docker snapshot with recent metric samples.

Examples:

```dol
observe containers last 5m
observe containers | where cpu > 80%
analyze containers find anomalies last 1h
```

Hybrid execution rules:

- Entity metadata comes from Docker or the most recent stored snapshot.
- Metric fields may come from the metrics collector or telemetry store.
- Windowed metrics use samples whose timestamps are inside the requested interval.

### 10.4 Historical Queries

Historical queries require a telemetry store.

Examples:

```dol
inspect container api-service at "2026-01-01 12:00:00"
events containers last 1h
analyze container api-service correlate events last 1h
```

Historical execution rules:

- If no telemetry store is configured, return a clear unsupported error.
- `at` selects the nearest snapshot at or before the timestamp.
- `last` selects all matching records in the window ending at execution time.
- Historical results must include source timestamps in JSON output.

## 11. Consistency Model

DOL v0.1 uses best-effort consistency:

- Docker snapshots are point-in-time approximations.
- Metrics may lag entity metadata.
- Events are ordered by Docker event timestamp when available.
- For live streams, delivery is at-least-once from the DOL process perspective.
- Exact-once alert delivery is not guaranteed in v0.1.

The implementation should preserve timestamps and raw Docker IDs so users can reconcile output with Docker itself.

## 12. Error Semantics

Parser errors should include:

- Query text position when available.
- Expected token or construct.
- A short repair hint.

Runtime errors should distinguish:

- Docker connection failure.
- Unsupported target.
- Unsupported field.
- Unsupported pipeline node for execution mode.
- Missing telemetry store.
- Alert action failure.

Examples:

```text
parse error at column 20: expected expression after `where`
runtime error: `sort by` requires finite input; add `last 5m` or `limit`
runtime error: historical query requires a telemetry store
```

## 13. Reserved Keywords

Reserved keywords in v0.1:

```text
alert analyze and asc at between by case contains count desc distinct else end events
false find for from group having if in inspect is last limit matches max min not null
observe or offset restart select set sort sum then to true webhook when where
```

Docker names that conflict with reserved keywords must be quoted as strings when used as values.

## 14. MVP Acceptance Query Set

The parser and executor should prioritize these queries first:

```dol
observe containers
observe containers where status = running
observe containers | where image contains "postgres" | select name, status
observe images | sort by size desc | limit 10
events containers where action = "die"
events containers | where action = "restart" | select time, container, image
inspect container api-service
inspect container api-service at "2026-01-01 12:00:00"
analyze containers find anomalies
analyze containers find restart_loops last 10m
alert when cpu > 85% for 2m then print "High CPU"
observe containers | where cpu > 80% | alert "High CPU detected"
```

## 15. Out of Scope for v0.1

These features are intentionally deferred:

- Joins between targets.
- User-defined functions.
- Full SQL aggregation functions.
- Distributed Docker host querying.
- Kubernetes targets.
- Automatic remediation without explicit user opt-in.
- AI-generated decisions that are not backed by deterministic signals.

## 16. Implementation Notes for Faz 2

Parser phase should start with:

1. `observe containers`
2. Inline `where`
3. Pipe `where`
4. `select`, `sort by`, `limit`
5. `events containers`
6. `inspect container <value>`
7. `alert when ... then print ...`

Recommended AST split:

- `Query`
- `Target`
- `Expression`
- `Value`
- `PipelineNode`
- `TimeSelector`
- `AlertRule`

## 17. CLI Reference

The DOL CLI supports the following flags:

| Flag | Description |
|------|-------------|
| `--store <path>` | Path to SQLite telemetry store |
| `--collect` | Start background data collection daemon |
| `--metrics-interval <s>` | Metrics polling interval in seconds (default: 30) |
| `--snapshot-interval <s>` | Snapshot interval in seconds (default: 300) |
| `--store-stats` | Display telemetry store statistics |
| `--apply-retention` | Apply retention policies to clean old data |
| `--output <fmt>` | Output format: `table`, `json`, `csv`, `jsonl` (default: table) |
| `--export <path>` | Write output to file (format inferred from extension: .csv, .json, .jsonl, .table) |
| `--export-format <fmt>` | Export file format: `influx`, `loki`, `prometheus` (used with `--export`) |
| `--host <addr>` | Docker daemon address (e.g., `tcp://192.168.1.100:2375`) |
| `--watch <s>` | Re-run query every N seconds (batch queries only) |
| `--explain` | Show the logical query plan without executing |
| `--diff` | Compare query results with the last store snapshot (requires `--store`) |
| `--completion <shell>` | Generate shell completion script (`bash`, `zsh`, `fish`, `powershell`, `elvish`) |
| `--export-influx <url>` | Push results to InfluxDB v1/v2 HTTP write API |
| `--export-grafana-loki <url>` | Push results to Grafana Loki HTTP push API |
| `--export-prometheus <url>` | Push results to Prometheus Pushgateway |
| `repl` | Start interactive REPL shell with tab completion and history |
| `top` | Live-updating TUI container monitor (top-like) with auto-refresh, keyboard controls, CPU/MEM gauge bars, and filter mode |
| `dashboard` | Multi-panel TUI dashboard with container list, state distribution stats, and live Docker events |
| `config init` | Create a default config file at the standard config path |
| `config set <key> <value>` | Update a configuration value (`store`, `output`, `host`, `metrics-interval`, `snapshot-interval`) |
| `config view` | Display the current merged configuration |

Additional REPL commands (within `dol repl`):

| REPL Command | Description |
|--------------|-------------|
| `.help` | Show available REPL commands |
| `.exit` / `.quit` | Exit the REPL |
| `.history` | Show command history |
| `.watch <secs>` | Re-run the last query every N seconds |
| `.export <path>` | Write subsequent results to a file |
| `.output <fmt>` | Set output format (`table`, `json`, `csv`, `jsonl`) |

Config file support (YAML/TOML):

- `~/.config/dol/config.yaml`
- `~/.config/dol/config.toml`
- `~/.dolrc`
- `.dolrc`
- `dol.yaml`
- `dol.toml`

The v0.1 grammar should be treated as the source of truth for parser tests.
