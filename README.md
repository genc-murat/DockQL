# DOL — Docker Observability Language

A domain-specific language and CLI tool for querying, observing, and analyzing Docker infrastructure.

```bash
dol "observe containers | where cpu > 80% | select name, image, cpu | sort cpu desc"
dol "events containers | where action = die | limit 10"
dol "observe containers | group by state"
```

## Features

- **Live observation** — query containers, images, networks, volumes with filtering, sorting, and aggregation
- **Real-time events** — stream Docker events with pipeline filters
- **Historical inspection** — inspect container state at any point in the past (requires `--store`)
- **Alerting** — continuously evaluate conditions with duration guards and actions (print, webhook, restart)
- **Analysis** — deterministic anomaly detection (CPU, memory, restart loops, deployment errors)
- **Telemetry store** — persistent SQLite-backed storage for metrics, events, and snapshots
- **Control flow** — `if/then/else` pipeline branching, `case/when` expressions, `set` field assignment
- **Batch and stream modes** — snapshots for `observe`/`inspect`, streaming for `events`/`alert`

## Installation

```bash
git clone <repo>
cd DockQL
cargo build --release
./target/release/dol --help
```

## Usage

### Quick Start

```bash
# List all running containers
dol "observe containers"

# Find containers with high memory usage
dol "observe containers | where memory > 500MB and state = running | select name, memory"

# Stream container crash events
dol "events containers where action = die"

# Top 10 largest images
dol "observe images | sort by size desc | limit 10"

# Alert on high CPU
dol "alert when cpu > 85% for 2m then print High CPU"
```

### Pipeline Chaining

Multiple operations can be chained with `|`:

```bash
dol "observe containers \
  | where cpu > 50% \
  | group by image \
  | sort by count desc \
  | limit 5"
```

### Classification with `if`/`case`

```bash
# Set a severity field based on CPU thresholds
dol 'observe containers \
  | set severity = case \
      when cpu > 80% then "critical" \
      when cpu > 50% then "warning" \
      else "ok" \
    end \
  | select name, cpu, severity'

# Conditional pipeline branching
dol 'observe containers \
  | if cpu > 90% then alert "Critical CPU" \
    else if cpu > 70% then alert "Warning: High CPU"'
```

### Background Data Collection

```bash
dol --store telemetry.db --collect --metrics-interval 30 --snapshot-interval 300
```

### Historical Queries

```bash
# Inspect a container's state at a specific time
dol --store telemetry.db 'inspect container my-app at "2025-01-01 12:00:00"'

# Replay events from a time window
dol --store telemetry.db 'events containers from "2025-05-30T10:00:00Z" to "2025-05-30T11:00:00Z"'

# Show store statistics
dol --store telemetry.db --store-stats

# Apply retention policies
dol --store telemetry.db --apply-retention
```

### JSON Output

```bash
dol --output json "observe containers"
```

## Query Language

### Targets

- `observe containers` / `images` / `networks` / `volumes`
- `events containers` / `images` / `networks` / `volumes`
- `inspect container <name>` / `image <name>` (with optional `at "<time>"`)
- `analyze [containers|container <name>] find [anomalies|restart_loops|deployment_errors|...]`
- `alert when <condition> [for <duration>] then <action>`

### Pipeline Nodes

| Node | Description |
|------|-------------|
| `where <expr>` | Filter rows |
| `select <fields>` | Choose columns |
| `group by <fields>` | Aggregate with count |
| `sort by <field> [asc\|desc]` | Order rows |
| `limit <n>` | Take first N rows |
| `alert "message"` | Emit inline alert |
| `set <field> = <value>` | Add or override a field |
| `if <cond> then <node>` | Conditional pipeline branching |
| `[else if <cond> then <node>]` | Chained else-if |
| `[else <node>]` | Fallback branch |

### Set Values

- Literal: `set tier = "prod"`
- If/else: `set health = if state = running then "up" else "down"`
- Case/when: `set severity = case when cpu > 80% then "critical" else "ok" end`

### Expression Operators

Comparison: `=`, `!=`, `>`, `>=`, `<`, `<=`, `contains`, `matches`
Logical: `and`, `or`, `not`
Grouping: `(`, `)`

### Container Fields

`id`, `name`, `image`, `status`, `state`, `ports`, `labels`, `created_at`, `cpu`, `memory`, `memory_limit`, `restart_count`, `network_rx`, `network_tx`, `disk_read`, `disk_write`

## Examples

36 example queries are available in [`examples/`](examples/):

```
observe containers
observe containers where state = running
observe containers | where cpu > 80% | select name, image, cpu | sort cpu desc
observe containers | where memory > 500MB and state = running | select name, memory
observe containers | group by state
observe containers | group by image | sort by count desc | limit 5
observe images | sort by size desc | limit 10
observe networks | select name, driver, scope
observe volumes | sort by name asc
events containers | where action = die | limit 10
events images | where action = pull
inspect container api-service
inspect container db-master at "2025-05-30 04:59:59Z"
analyze containers find anomalies
alert when cpu > 85% for 2m then print High CPU
observe containers | set severity = case when cpu > 80% then "critical" else "ok" end
observe containers | if cpu > 90% then alert "Critical"
```

## Architecture

The project follows a pipeline architecture:

```
Docker API → Entity/Metrics/Event Sources → Parser → Planner → Executor → Table/JSON Output
                                              ↑                              ↑
                                           AST nodes                    Telemetry Store
                                                                        (SQLite)
```

Key modules:

- **`parser`** — tokenizes and parses DOL into AST (`ast.rs`)
- **`planner`** — optimizes queries (filter pushdown, reordering)
- **`executor`** — executes batch queries against Docker entities
- **`events`** — streams and filters Docker events
- **`alerts`** — continuously evaluates alert rules with duration guards
- **`eval`** — shared expression evaluation engine
- **`metrics`** — collects and normalizes Docker stats
- **`docker`** — Docker CLI client abstraction
- **`collector`** — background daemon for telemetry collection
- **`sqlite_store`** — persistent storage (metrics, events, snapshots, retention)
- **`analyze`** — deterministic anomaly detection engine
- **`cli`** — CLI entry point (clap)

## Specification

See [`docs/spec.md`](docs/spec.md) for the full language specification and [`docs/examples.md`](docs/examples.md) for a categorized query reference.

