# Docker Observability Language Specification

Status: draft

DOL is a domain-specific language for querying, observing, and analyzing Docker infrastructure. The language defines a rich, implementable core that can be parsed into a typed AST and executed by a Rust CLI.

## 1. Design Goals

DOL treats Docker as a live data system:

- Docker entities are query targets.
- Container metrics are time-oriented records.
- Docker events are streams.
- Alerts are continuously evaluated queries.
- Historical inspection is a query over stored telemetry.

DOL improves the day-to-day workflow where users otherwise combine `docker ps`, `docker stats`, `docker events`, shell scripts, Prometheus queries, and dashboard tools. The language does not replace Prometheus or Grafana; it provides a Docker-native query surface that can later export to those systems.

The language prioritizes:

- Simple, readable syntax.
- Strong parser boundaries with detailed error messages.
- Streaming and batch execution modes.
- A realistic Rust implementation with comprehensive test coverage.
- Deterministic behavior before AI-assisted insights.

## 2. Query Families

DOL has nine top-level query families.

### 2.1 `observe`

`observe` reads the current state of Docker entities and optionally enriches containers with current metrics.

Execution mode: batch snapshot by default, hybrid when metric windows are requested.

**Simple queries:**

```dol
observe containers
observe containers where status = running
observe containers | where image contains "postgres" | select name, status, ports
```

**Cross-target JOIN:**

A `JOIN` clause merges rows from two targets on a matching key:

```dol
observe containers join images on image = repository
observe containers join networks on name = name
observe containers join volumes on scope = scope
```

Syntax:

```dol
observe <left-target> join <right-target> on <left-key> = <right-key> [ | <pipeline> ]
```

- The left target is the primary query target.
- The right target is specified after `join`.
- Both key expressions are evaluated against their respective target's fields
  using bare field names (no prefix).
- Output rows contain all fields from both targets, prefixed with an
  auto-generated alias:
  - `c.` for containers
  - `i.` for images
  - `n.` for networks
  - `v.` for volumes
- All downstream pipeline nodes operate on the prefixed field names.

Execution: nested-loop join over all matching right-target rows for each left-target row; equality comparison via `=`.

Examples:

```dol
observe containers join images on image = repository + ":" + tag
observe containers join networks on id = id
observe containers join images on id = id | select c.name, i.repository
observe containers join images on id = id | where c.image = "nginx:latest"
observe containers join volumes on state = scope
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

### 2.4 `logs`

`logs` retrieves log output from a running container. Returns the last N lines
with line numbers, message content, and container name.

Execution mode: batch.

Examples:

```dol
logs container my-app
logs container my-app tail 50
logs container my-app | where message contains "error" | select line, message
```

### 2.5 `ping`

`ping` tests connectivity to the Docker daemon. Returns a status field (`ok` or
`error`) and a human-readable message.

Execution mode: batch.

Examples:

```dol
ping
```

### 2.6 `analyze`

`analyze` runs deterministic analysis over current or stored Docker telemetry.

Execution mode: batch or historical depending on time qualifiers.

Examples:

```dol
analyze containers find anomalies
analyze containers find restart_loops last 10m
analyze containers find dependencies
analyze containers find density
analyze containers find leaks
analyze containers find drift
analyze container api-service correlate events last 1h
```

Analysis types:
- **anomalies** (default) — detects high CPU, memory pressure, restart loops, and unhealthy states.
- **correlate** — finds containers sharing images and labels.
- **explain** — produces a diagnostic signal summary for one or all containers.
- **dependencies** — maps compose project, network, and volume relationships.
- **density** — container distribution across images, states, and compose projects.
- **leaks** — detects memory usage growth trends from historical metrics (requires `--store`).
- **drift** — compares two telemetry snapshots to detect image, state, label, or restart count changes (requires `--store`).

### 2.7 `fields`

`fields` returns the schema (field names and types) for a given entity type. This is useful for discovery and tooling integration.

Execution mode: batch.

Examples:

```dol
fields containers
fields images
fields networks
fields volumes
```

### 2.8 `compose`

`compose` queries containers, networks, volumes, and other resources grouped under a Docker
Compose project. It filters resources by the `com.docker.compose.project`
label.

Execution mode: batch.

Syntax:

```dol
compose ls
compose <project>
compose <project> containers
compose <project> services
compose <project> networks
compose <project> volumes
compose <project> health
compose <project> images
compose <project> stats
compose <project> ps
compose <project> logs <service> [tail <n>]
compose <project> port <service> <port>
compose <project> config [services|networks|volumes]
compose <project> events
observe compose <project>
```

- `compose ls` lists all Docker Compose projects with their container, network, and volume counts.
  Supported fields: `project`, `containers`, `running`, `stopped`, `networks`, `volumes`, `status`.
- Without a target keyword, defaults to `containers` — listing all containers
  in the compose project with their labels and metrics.
- Use `containers` explicitly to make the target clear: `compose myapp containers`.
- With `services`, each row additionally shows the `service` field extracted from the
  `com.docker.compose.service` label, allowing pipeline operations like
  `select service, name, state`.
- With `networks`, lists all Docker networks filtered by the compose project label.
  Supported fields: `id`, `name`, `driver`, `scope`, `containers`, `labels`.
- With `volumes`, lists all Docker volumes filtered by the compose project label.
  Supported fields: `name`, `driver`, `mountpoint`, `scope`, `labels`.
- With `health`, lists containers within the compose project with their service
  name and health status. The `health` field is extracted from the Docker
  inspect `/State/Health/Status` endpoint. Supported fields include all
  container fields plus `service` and `health`.
- With `images`, lists images used by containers in the compose project.
  Supported fields: `id`, `repository`, `name`, `tag`, `digest`, `size`, `service`.
- With `stats`, shows resource usage statistics for compose project containers.
  Supported fields: `name`, `service`, `cpu`, `memory`, `memory_limit`, `memory_pct`,
  `network_rx`, `network_tx`, `disk_read`, `disk_write`.
- With `ps`, shows enhanced container status with service names.
  Supported fields: `name`, `service`, `image`, `state`, `status`, `health`,
  `restart_count`, `ports`.
- With `logs <service>`, retrieves log output for a specific service.
  Supported fields: `line`, `message`, `service`, `container`.
- With `port <service> <port>`, shows port mappings for a service.
  Supported fields: `service`, `container`, `ports`.
- With `config`, inspects the Compose project configuration from running containers.
  Supported fields (services): `name`, `image`, `state`, `status`, `ports`, `restart_count`, `health`, `depends_on`.
  Supported fields (networks): `name`, `driver`, `scope`, `containers`.
  Supported fields (volumes): `name`, `driver`, `mountpoint`, `scope`.
- With `events`, streams real-time events for the compose project (requires streaming mode).
- The `observe compose <project>` syntax is an alternative form that reads
  identically to other `observe` sub-queries.

Examples:

```dol
compose ls
compose ls | where containers > 5 | sort by project asc
compose myapp
compose myapp services
compose myapp networks
compose myapp volumes
compose myapp health
compose myapp images
compose myapp stats | where cpu > 80% | select name, service, cpu
compose myapp ps | where state = "running" | select name, service, health
compose myapp logs api-service tail 50
compose myapp logs api-service tail 100 | where message contains "error"
compose myapp port api-service 8080
compose myapp config services
compose myapp config networks
compose myapp config volumes
compose myapp | where cpu > 80% | select name, cpu
compose myapp health | where health = "unhealthy" | select name, service, health
alert when compose_project = 'myapp' and cpu > 85% for 2m then print "High CPU"
```

### 2.9 `alert`

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
- `compose <project>` — dynamically scoped to containers within a Docker Compose project

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
- `health` — health check status from Docker inspect (`/State/Health/Status`); `null` if no health check is configured

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

- `time` — event timestamp (Unix seconds, nanoseconds, or ISO 8601)
- `type` — event type (`container`, `image`, `network`, `volume`)
- `action` — action name (`start`, `die`, `stop`, `restart`, `pull`, etc.)
- `actor_id` — Docker entity ID for the event actor
- `container` — container name (when applicable)
- `image` — image reference (when applicable)
- `attributes` — array of `key=value` strings from the event's Actor Attributes

### 4.6 Dynamic Fields via `set`

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

#### 5.1 Static Semantic Analysis & Type Safety

DOL implements a static semantic analyzer and type safety validation pass before executing any query. This phase runs immediately after parsing and prevents execution of queries that contain structural or type errors:

- **Field Existence Validation**: All referenced fields in filters, projections (`select`), and aggregations (`group by`) are checked against the static schema of the collection target (e.g., containers, images). Dynamically added fields via `set`, `fill`, and `let` are added to the active schema context and allowed in downstream pipeline nodes.
- **Label Prefix Support**: Dynamically resolved label lookups using the `label.` prefix (e.g., `label.env`) are recognized and statically validated as long as the base `labels` field exists on the target.
- **Type Compatibility Checking**: High-level comparison and arithmetic operations validate that their operands are compatible. For example:
  - Comparing a String field (e.g., `state`) to an Integer literal (e.g., `50`) with binary comparison operators like `>` will be rejected at compilation/validation time.
  - Performing arithmetic operations on non-numeric types is statically rejected.
  - `AND`, `OR`, and `NOT` operators require boolean operands.

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
- `starts_with` — prefix match: `name starts_with "api-"`
- `ends_with` — suffix match: `image ends_with ":latest"`
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

**String functions:**
- `upper(s)` — returns uppercase
- `lower(s)` — returns lowercase
- `length(s)` — returns Integer string length
- `trim(s)` — returns trimmed whitespace
- `concat(a, b, ...)` — returns string concatenation of all arguments
- `substring(s, start, len)` — returns substring extraction
- `coalesce(a, b, ...)` — returns first non-null, non-empty value
- `starts_with(s, prefix)` — returns Boolean, true if `s` starts with `prefix`
- `ends_with(s, suffix)` — returns Boolean, true if `s` ends with `suffix`
- `replace(s, from, to)` — returns string with all occurrences of `from` replaced with `to`
- `reverse(s)` — returns reversed string
- `repeat(s, n)` — returns string repeated `n` times
- `position(s, substr)` — returns Integer, 0-based index of first `substr` occurrence (0 if not found)
- `split_part(s, delim, n)` — returns split by `delim`, the `n`-th part (1-indexed)

**Date/time functions:**
- `now()` — returns current UTC timestamp as RFC 3339 string
- `date_format(ts, fmt)` — format a timestamp string according to `fmt` (strftime syntax, e.g., `%Y-%m-%d`)
- `date_diff(a, b, unit)` — returns Integer difference between two timestamps in the given unit (`seconds`, `minutes`, `hours`, `days`)
- `extract(ts, part)` — returns Integer part of a timestamp: `year`, `month`, `day`, `hour`, `minute`, `second`

### 6.7 Coalesce Function

The `coalesce()` function returns the first non-null, non-empty value from its arguments:

```dol
observe containers | set name = coalesce(label.name, name, "unknown") | select name
```

If all arguments are null or empty strings, `coalesce()` returns `null`.

### 6.8 `$var` Field References

Field names can be prefixed with `$` for explicit field access. This is useful
when a field name might otherwise be parsed as a literal value:

```dol
observe containers where $state = running
observe containers | set $cpu = cpu
```

`$state` is equivalent to `state` — the `$` prefix is stripped during parsing
and the result is treated as a field reference.

### 6.9 Operator Precedence

Precedence, highest to lowest:

1. Parentheses: `( ... )`
2. Function calls, unary `-`
3. Arithmetic: `*`, `/`, `%`
4. Arithmetic: `+`, `-`
5. Comparison, `contains`, `matches`, `starts_with`, `ends_with`, `between`, `is null`, `is not null`
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
- `fill <field> with <expr>` — Replace null/empty values in `<field>` with the result of `<expr>` (useful for supplying defaults for missing metrics or labels)
- `let $name = <expr>` — Declare a constant or parameter. The expression is evaluated and the result is added to each row as a field named `$name` (the `$` prefix is optional). Useful for declaring thresholds, labels, or reusable values: `let $threshold = 80`
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
observe containers | fill memory with 0 | where memory > 500
observe containers | where name starts_with "api-"
observe containers | where image ends_with ":latest"
observe containers | set mem_gb = memory / 1073741824
observe containers | set today = now()
observe containers | set day = extract(created_at, 'day')
observe containers where $state = running
observe containers | let $threshold = 80 | where cpu > $threshold
observe containers | let $app = "myapp" | where compose_project = $app
```

## 9. Execution Semantics

### 9.1 Batch Queries

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

### 9.2 Stream Queries

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

### 9.3 Hybrid Queries

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

### 9.4 Historical Queries

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

## 10. Consistency Model

DOL v0.1 uses best-effort consistency:

- Docker snapshots are point-in-time approximations.
- Metrics may lag entity metadata.
- Events are ordered by Docker event timestamp when available.
- For live streams, delivery is at-least-once from the DOL process perspective.
- Exact-once alert delivery is not guaranteed in v0.1.

The implementation should preserve timestamps and raw Docker IDs so users can reconcile output with Docker itself.

## 11. Error Semantics

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
parse error at column 28: expected field
  --> observe containers | where | sort by cpu
                            ^

parse error at column 20: expected expression after `where`
  --> observe containers | where | sort by cpu
                               ^

runtime error: `sort by` requires finite input; add `last 5m` or `limit`
runtime error: historical query requires a telemetry store
```

Parser errors show:

- The exact column position of the error.
- A source context line with `-->` pointing to the offending query.
- A `^` pointer under the exact column where parsing failed.
- A descriptive message explaining what was expected.

In the CLI and REPL, error messages are displayed in ANSI **red** for visual prominence.

## 12. Reserved Keywords

Reserved keywords:

```text
alert analyze and asc at between by case compose contains correlate count dashboard
config desc distinct else end events explain extract false fields fill find for from
group having health if in inspect is join last let limit logs matches max min networks
not null observe of offset or ping repl repeat restart select service services set sort
split_part starts_with ends_with sum then to top true upper lower length trim concat
substring coalesce replace reverse position now date_format date_diff volumes
webhook when where
```

Docker names that conflict with reserved keywords must be quoted as strings when used as values.

## 13. MVP Acceptance Query Set

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

## 14. Out of Scope

These features are intentionally deferred:

- User-defined functions.
- Full SQL aggregation functions.
- Distributed Docker host querying.
- Kubernetes targets.
- Automatic remediation without explicit user opt-in.
- AI-generated decisions that are not backed by deterministic signals.

## 15. Implementation Notes

The parser was implemented incrementally, starting with the core query families and gradually adding pipeline nodes. The current implementation supports all nine query families and all pipeline nodes described in this specification.

AST types (defined in `src/ast.rs`):

- `Query`
- `Target` (CollectionTarget, SingularTarget, ComposeTarget)
- `Expression`
- `Value`
- `PipelineNode`
- `TimeSelector`
- `AlertRule`
- `JoinClause`
- `SetValue`
- `AggregateExpr`

## 16. CLI Reference

The DOL CLI supports the following flags:

| Flag | Description |
|------|-------------|
| `--store <path>` | Path to SQLite telemetry store |
| `--collect` | Start background data collection daemon |
| `--metrics-interval <s>` | Metrics polling interval in seconds (default: 30) |
| `--snapshot-interval <s>` | Snapshot interval in seconds (default: 300) |
| `--store-stats` | Display telemetry store statistics |
| `--apply-retention` | Apply retention policies to clean old data |
| `--output <fmt>` | Output format: `table`, `json`, `json-compact`, `csv`, `jsonl` (default: table) |
| `--export <path>` | Write output to file (format inferred from extension: .csv, .json, .jsonl, .table) |
| `--export-format <fmt>` | Export file format: `influx`, `loki`, `prometheus` (used with `--export`) |
| `--file <path>` / `-f <path>` | Read the DOL query from a `.dol` file |
| `--host <addr>` | Docker daemon address (e.g., `tcp://192.168.1.100:2375`) |
| `--watch <s>` | Re-run query every N seconds (batch and alert queries) |
| `--timeout <s>` | Query execution timeout in seconds — if a query takes longer than this, it is aborted (applies to watch, alert, events, store, and single queries) |
| `--explain` | Show the logical query plan without executing |
| `--diff` | Compare query results with the last store snapshot (requires `--store`) |
| `--theme <dark\|light>` | Color theme for table output (`dark` or `light`); can also be set permanently in config via `theme: dark\|light` |
| `--completion <shell>` | Generate shell completion script (`bash`, `zsh`, `fish`, `powershell`, `elvish`) |
| `--export-influx <url>` | Push results to InfluxDB v1/v2 HTTP write API |
| `--export-grafana-loki <url>` | Push results to Grafana Loki HTTP push API |
| `--export-prometheus <url>` | Push results to Prometheus Pushgateway |
| `repl` | Start interactive REPL shell with tab completion, history, and REPL commands |
| `top` | Live-updating TUI container monitor (top-like) with auto-refresh, keyboard controls, CPU/MEM gauge bars, filter mode, and event-driven refresh |
| `dashboard` | Multi-panel TUI dashboard with container list, state distribution stats (histogram bars), top images, and live Docker events stream |
| `config init` | Create a default config file at the standard config path |
| `config set <key> <value>` | Update a configuration value (`store`, `output`, `host`, `metrics-interval`, `snapshot-interval`, `theme`) |
| `config view` | Display the current merged configuration (from CLI flags + config file + defaults) |

Additional REPL commands (within `dol repl`):

| REPL Command | Description |
|--------------|-------------|
| `.help` | Show available REPL commands |
| `.exit` / `.quit` | Exit the REPL |
| `.history` | Show command history |
| `.watch <secs>` | Re-run the last query every N seconds |
| `.export <path>` | Write subsequent results to a file |
| `.output <fmt>` | Set output format (`table`, `json`, `json-compact`, `csv`, `jsonl`) |
| `.host [<addr>]` | Show or set Docker host address within the REPL session |

Config file support (YAML/TOML):

- `~/.config/dol/config.yaml`
- `~/.config/dol/config.toml`
- `~/.dolrc`
- `.dolrc`
- `dol.yaml`
- `dol.toml`
- `~/.dolrc.yaml`
- `~/.dolrc.toml`

The grammar defined in this specification is the source of truth for parser tests.
