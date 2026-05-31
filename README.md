<p align="center">
  <img src="docs/logo.svg" width="80" height="80" alt="DOL logo">
</p>

<h1 align="center">DOL — Docker Observability Language</h1>

<p align="center">A domain-specific language and CLI tool for querying, observing, and analyzing Docker infrastructure.</p>

```bash
dol "observe containers | where cpu > 80% | select name, image, cpu | sort cpu desc"
dol "events containers | where action = die | limit 10"
dol "observe containers | group by state"
```

## Features

- **Live observation** — query containers, images, networks, volumes with filtering, sorting, and aggregation
- **Real-time events** — stream Docker events with pipeline filters and group-by aggregation
- **Historical inspection** — inspect container state at any point in the past (requires `--store`)
- **Historical observe** — query past container states with `observe containers last 5m`
- **Alerting** — continuously evaluate conditions with duration guards; actions include print, webhook (real HTTP POST), and container restart
- **Alert history** — all fired alerts are persisted in the SQLite store for audit/review
- **Analysis** — deterministic anomaly detection (CPU, memory, restart loops, deployment errors)
- **Telemetry store** — persistent SQLite-backed storage for metrics, events, and snapshots
- **Schema introspection** — discover available fields with `fields containers`
- **Static Semantic Analysis** — static type checking and schema validation of queries prior to execution to catch errors early
- **Control flow** — `if/then/else` pipeline branching, `case/when` expressions, `set` field assignment
- **Arithmetic expressions** — compute fields with `+`, `-`, `*`, `/`, `%` (e.g., `set mem_gb = memory / 1073741824`)
- **Aggregate functions** — `group by ... with sum/count/avg/min/max(field) as alias`
- **String functions** — `upper()`, `lower()`, `length()`, `trim()`, `concat()`, `substring()`, `coalesce()`
- **Range check** — `between ... and ...`, `is null`, `is not null`
- **Pattern matching** — `matches` (regex) and `in` operators for flexible filtering
- **Multi-field sort** — `sort by state desc, cpu desc`
- **Pipeline nodes** — `having`, `distinct`, `offset`
- **Diff mode** — compare current container state with the last store snapshot (`--diff`)
- **Multiple output formats** — table (default), CSV, JSONL, JSON, ANSI-colored table
- **External integrations** — push results directly to InfluxDB, Loki, or Prometheus Pushgateway
- **Config file** — YAML/TOML configuration from standard paths; manage with `dol config init|set|view`
- **Interactive REPL** — `dol repl` with tab completion, history, `.watch`, `.export` commands
- **Terminal Dashboard** — `dol top` live container monitor with auto-refresh, color-coded states, keyboard controls
- **Multi-panel Dashboard** — `dol dashboard` with container list + live Docker events panel
- **Shell completion** — generate completion scripts with `--completion <shell>`
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

### Output Formats

```bash
# JSON output
dol --output json "observe containers"

# CSV output
dol --output csv "observe containers | select name, state, cpu"

# JSONL (JSON Lines) output
dol --output jsonl "events containers | limit 5"

# ANSI-colored table (default when output is a terminal)
dol "observe containers"
```

### Background Data Collection

```bash
dol --store telemetry.db --collect --metrics-interval 30 --snapshot-interval 300
```

### Historical Queries

```bash
# Inspect a container's state at a specific time
dol --store telemetry.db 'inspect container my-app at "2025-01-01 12:00:00"'

# Observe containers as they were 5 minutes ago
dol --store telemetry.db 'observe containers last 5m'

# Replay events from a time window
dol --store telemetry.db 'events containers from "2025-05-30T10:00:00Z" to "2025-05-30T11:00:00Z"'

# Show store statistics
dol --store telemetry.db --store-stats

# Apply retention policies
dol --store telemetry.db --apply-retention
```

### Diff Mode

```bash
# Compare current containers with the last stored snapshot
dol --store telemetry.db "observe containers" --diff
```

### Export to File

```bash
dol --output csv --export results.csv "observe containers | select name, state"
dol --output json --export results.json "observe containers"
```

### Explain Mode

```bash
# Show the query plan without executing
dol --explain "observe containers | where cpu > 50% | select name, cpu"
```

### Watch Mode

```bash
# Re-run query every 5 seconds
dol --watch 5 "observe containers | where state = running"
```

### Shell Completion

```bash
# Generate bash completion script
dol --completion bash > /etc/bash_completion.d/dol

# Generate PowerShell completion
dol --completion powershell >> $PROFILE
```

### Config File

```bash
# Create a default config file
dol config init

# Set config values
dol config set store ~/telemetry.db
dol config set host tcp://192.168.1.100:2375

# View current configuration
dol config view
```

DOL loads settings from `~/.config/dol/config.yaml`, `~/.config/dol/config.toml`, `.dolrc`, or `dol.yaml`:

```yaml
store: /path/to/telemetry.db
output: table
host: tcp://192.168.1.100:2375
metrics_interval: 30
snapshot_interval: 300
```

### External Integrations

```bash
# Push results to InfluxDB (v1 write API)
dol --export-influx "http://localhost:8086/write?db=dol" "observe containers"

# Push to Grafana Loki
dol --export-grafana-loki "http://localhost:3100" "observe containers"

# Push to Prometheus Pushgateway
dol --export-prometheus "http://localhost:9091" "observe containers"

# Export to file in InfluxDB line protocol format
dol --export metrics.txt --export-format influx "observe containers"

# Export to file in Prometheus exposition format
dol --export metrics.prom --export-format prometheus "observe containers"
```

### Remote Host

```bash
# Connect to a remote Docker daemon
dol --host tcp://192.168.1.100:2375 "observe containers"
```

### Interactive REPL

```bash
# Start an interactive shell with tab completion and history
dol repl

dol> observe containers | where cpu > 50% | select name, cpu
dol> events containers | where action = die
dol> .help
dol> .watch 3
```

REPL commands:
- `.help` — show available commands
- `.exit` / `.quit` — exit the REPL
- `.history` — show command history
- `.watch <secs>` — re-run the last query every N seconds
- `.export <path>` — set export file path
- `.output <fmt>` — set output format (table, json, csv, jsonl)

### Terminal Dashboard

```bash
# Live-updating container monitor (top-like) with CPU/MEM gauge bars
dol top

# Multi-panel dashboard with containers, stats, and live events
dol dashboard
```

`dol top` keyboard controls:
- `↑`/`↓` or `j`/`k` — navigate rows
- `s` — cycle sort column (name, image, state, status)
- `d` — toggle sort direction
- `r` — force refresh
- `/` — enter filter mode (filter containers by name)
- `h` — toggle help overlay
- `q` / Esc — quit

`dol top` displays columns: NAME, IMAGE, CPU (gauge bar), MEM (gauge bar), MEMORY (usage %), STATE, STATUS, RST (restart count). CPU and MEM bars are color-coded: green (<50%), yellow (50–80%), red (>80%).

`dol dashboard` keyboard controls:
- `Tab` — switch panel focus (containers / stats)
- `r` — force refresh
- `c` — clear events panel
- `h` — toggle help overlay
- `q` / Esc — quit

`dol dashboard` layout:
- **Left panel**: Container list with name, CPU%, memory usage, state
- **Right panel**: State distribution histogram + top images
- **Bottom panel**: Live Docker events stream (polled every 5s)

Both modes auto-refresh every 2 seconds. Container states are color-coded: running (green), exited/dead (red), paused (yellow), restarting (cyan).

### Real Alert Actions

When an alert fires, actions are executed immediately:

```bash
# Webhook: sends an actual HTTP POST
dol 'alert when cpu > 85% for 2m then webhook "https://hooks.example.com/alert"'

# Restart: executes docker restart
dol 'alert when restart_count > 5 for 3m then restart container api'

# Alert history is saved to the telemetry store (requires --store)
dol --store telemetry.db 'alert when cpu > 85% for 2m then print "High CPU"'
```

## Query Language

### Targets

- `observe containers` / `images` / `networks` / `volumes`
- `events containers` / `images` / `networks` / `volumes`
- `inspect container <name>` / `image <name>` (with optional `at "<time>"`)
- `fields containers` / `images` / `networks` / `volumes` (schema introspection)
- `analyze [containers|container <name>] find [anomalies|restart_loops|deployment_errors|...]`
- `alert when <condition> [for <duration>] then <action>`

### Pipeline Nodes

| Node | Description |
|------|-------------|
| `where <expr>` | Filter rows |
| `select <fields>` | Choose columns |
| `group by <fields>` | Aggregate with count |
| `having <expr>` | Filter groups after aggregation |
| `sort by <field> [asc\|desc] [, ...]` | Order rows (multi-field) |
| `limit <n>` | Take first N rows |
| `offset <n>` | Skip first N rows |
| `distinct` | Remove duplicate rows |
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

Comparison: `=`, `!=`, `>`, `>=`, `<`, `<=`, `contains`, `matches` (regex), `in`
Arithmetic: `+`, `-`, `*`, `/`, `%`
Range: `between ... and ...`, `is null`, `is not null`
Logical: `and`, `or`, `not`
Functions: `upper()`, `lower()`, `length()`, `trim()`, `concat()`, `substring()`, `coalesce()`
Grouping: `(`, `)`

### Label Access

Access individual labels using dot notation:

```bash
dol "observe containers | where label.env = production | select name, label.version"
```

### Container Fields

`id`, `name`, `image`, `status`, `state`, `ports`, `labels`, `created_at`, `started_at`, `finished_at`, `cpu`, `memory`, `memory_limit`, `restart_count`, `network_rx`, `network_tx`, `disk_read`, `disk_write`, `compose_project`

### CLI Flags

| Flag / Subcommand | Description |
|------|-------------|
| `--store <path>` | Path to SQLite telemetry store |
| `--collect` | Start background data collection |
| `--metrics-interval <s>` | Metrics collection interval in seconds |
| `--snapshot-interval <s>` | Snapshot collection interval in seconds |
| `--store-stats` | Show telemetry store statistics |
| `--apply-retention` | Apply retention policies to the store |
| `--output <fmt>` | Output format: `table`, `json`, `csv`, `jsonl` |
| `--export <path>` | Write output to file instead of stdout |
| `--host <addr>` | Docker daemon host address |
| `--watch <s>` | Re-run query every N seconds |
| `--explain` | Show query plan without executing |
| `--diff` | Compare results with last store snapshot |
| `--completion <shell>` | Generate shell completion script |
| `--export-format <fmt>` | Export format for external systems: `influx`, `loki`, `prometheus` (used with `--export`)
| `--export-influx <url>` | Push results to InfluxDB v1/v2 HTTP write API |
| `--export-grafana-loki <url>` | Push results to Grafana Loki HTTP push API |
| `--export-prometheus <url>` | Push results to Prometheus Pushgateway |
| `repl` | Start interactive REPL shell with tab completion and history |
| `top` | Live-updating TUI container monitor with CPU/MEM gauge bars and filter mode |
| `dashboard` | Multi-panel TUI with container list, stats, and live events |
| `config init` | Create a default config file |
| `config set <key> <value>` | Update a config value |
| `config view` | Display current configuration |

## Examples

54 example queries are available in [`examples/`](examples/):

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
Docker API → Entity/Metrics/Event Sources → Parser → Planner → Executor → Table/CSV/JSON/JSONL Output
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
- **`config`** — YAML/TOML configuration file loader and `config init|set|view` subcommand
- **`repl`** — interactive REPL with tab completion and command history
- **`cli`** — CLI entry point (clap)

## Documentation

- [**Tutorial**](docs/tutorial.md) — step-by-step guide from installation to advanced pipelines
- [**Language Specification**](docs/spec.md) — full DOL language reference
- [**Examples**](docs/examples.md) — categorized query reference with 55+ examples
- [**Architecture**](docs/architecture.md) — pipeline architecture and module overview
- [**Analysis**](docs/analyze.md) — anomaly detection and automated analysis
- [**Storage**](docs/storage.md) — telemetry store, retention, and historical queries
