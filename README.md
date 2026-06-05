<p align="center">
  <img src="docs/logo.svg" width="80" height="80" alt="DOL logo">
</p>

<h1 align="center">DOL — Docker Observability Language</h1>

<p align="center">A domain-specific language and CLI tool for querying, observing, and analyzing Docker infrastructure.</p>

<p align="center">
  <img src="https://img.shields.io/github/v/release/genc-murat/DockQL?color=3ecf8e&logo=github&style=for-the-badge" alt="Release">
  <img src="https://img.shields.io/github/actions/workflow/status/genc-murat/DockQL/ci.yml?branch=main&label=CI&color=57a6ff&style=for-the-badge" alt="CI">
  <img src="https://img.shields.io/github/deployments/genc-murat/DockQL/github-pages?label=Docs&logo=github&style=for-the-badge" alt="Docs">
  <img src="https://img.shields.io/badge/Rust-1.96%2B-8a95a5?logo=rust&style=for-the-badge" alt="Rust">
</p>

```bash
dol "observe containers | where cpu > 80% | select name, image, cpu | sort cpu desc"
dol "events containers | where action = die | limit 10"
dol "logs container my-app tail 50"
dol "ping"
dol "compose ls"
dol "compose myapp services"
dol "compose myapp images"
dol "compose myapp stats | where cpu > 80%"
dol "compose myapp ps | where state = running"
dol "compose myapp logs api-service tail 50"
dol "compose myapp port api-service 8080"
dol "compose myapp config services"
dol "observe compose myapp | where cpu > 80%"
dol "compose myapp health | where health = unhealthy"
dol "observe containers join images on id = id | select c.name, i.repository"
dol "observe containers | group by state"
```

## Features

- **Live observation** — query containers, images, networks, volumes with filtering, sorting, aggregation, and cross-target JOIN
- **Docker Compose** — full compose project introspection: containers, services, networks, volumes, health, images, stats, PS, logs, ports, config, and `compose ls`
- **Real-time events** — stream Docker events with pipeline filters and aggregation
- **Historical queries** — inspect containers at any past point, observe last N minutes, replay event windows (requires `--store`)
- **Alerting** — continuous evaluation with duration guards; actions: print, webhook (HTTP POST), container restart
- **Analysis engine** — deterministic anomaly detection (restart loops, high CPU/memory, deployment errors, resource leaks, config drift, dependencies, density)
- **Control flow** — `if/then/else` branching, `case/when` expressions, `set` field assignment, `fill` null defaults, `let` variables
- **Pipeline chaining** — `where`, `select`, `group by`, `sort`, `limit`, `offset`, `distinct`, `having`, `fill`, `let`, `if`/`else`
- **Rich expressions** — arithmetic (`+`, `-`, `*`, `/`), comparison, `between`, `in`, `matches` (regex), string functions, date/time functions, `$var` field references
- **Telemetry store** — persistent SQLite-backed storage for metrics, events, snapshots; retention policies
- **Multiple output formats** — table, CSV, JSON, JSONL, JSON-compact, ANSI-colored; export to file or push to InfluxDB/Loki/Prometheus
- **Interactive REPL** — `dol repl` with tab completion, history, `.watch`, `.export`, `.output`
- **Terminal dashboards** — `dol top` (live container monitor) and `dol dashboard` (multi-panel with events stream)
- **Configurable timeouts** — all Docker API, stats, events, and alert timeouts adjustable via config file
- **Smart error messages** — coloured parse errors with source pointers and "did you mean?" keyword suggestions

## Installation

### Quick install (Linux / macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/genc-murat/DockQL/main/install.sh | bash
```

Or pin a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/genc-murat/DockQL/main/install.sh | bash -s -- 0.6.0
```

### macOS — Homebrew

```bash
brew tap genc-murat/dockql https://github.com/genc-murat/DockQL
brew install dol
```

### Cargo (if you have Rust installed)

```bash
cargo install dol
```

### Build from source

```bash
git clone https://github.com/genc-murat/DockQL.git
cd DockQL
cargo build --release
./target/release/dol --help

# Optionally install to ~/.cargo/bin
make install
```

### Download a pre-built binary

Pre-compiled binaries for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and
Windows (x86_64) are attached to
each [GitHub Release](https://github.com/genc-murat/DockQL/releases).
Simply download the archive for your platform, extract it, and place the
`dol` (or `dol.exe`) binary anywhere on your `PATH`.

## Usage

### Quick Start

```bash
# List all running containers
dol "observe containers"

# Find containers with high memory usage
dol "observe containers | where memory > 500MB and state = running | select name, memory"

# Stream container crash events
dol "events containers where action = die"

# View the last 50 log lines from a container
dol "logs container my-app tail 50"

# Check if Docker daemon is reachable
dol "ping"

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

_More examples in [`docs/examples.md`](docs/examples.md)._

### Output Formats

```bash
# JSON output (pretty-printed)
dol --output json "observe containers"

# Compact (minified) JSON — single line, no indentation
dol --output json-compact "observe containers"

# CSV output
dol --output csv "observe containers | select name, state, cpu"

# JSONL (JSON Lines) output
dol --output jsonl "events containers | limit 5"

# ANSI-colored table (default when output is a terminal)
dol "observe containers"

# Light theme for terminals with light backgrounds
dol --theme light "observe containers"
```

`json-compact` is ideal for piping into other tools or for reducing output size in scripts.

The default table output uses a **dark theme** with DarkGray alternating row backgrounds
and cyan headings. Pass `--theme light` (or set `theme: light` in the config file) for
blue headings on a light background without alternating row tints.

### Running Queries from `.dol` Files

```bash
# Read the query from a .dol file instead of passing it inline
dol --file examples/ping.dol
dol -f examples/list_containers.dol

# Combine with other flags
dol --file examples/running_containers.dol --output json
dol --explain -f examples/high_cpu.dol
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

### Watch Mode & Timeout

```bash
# Re-run query every 5 seconds
dol --watch 5 "observe containers | where state = running"

# With a 10-second timeout to prevent hanging
dol --watch 5 --timeout 10 "observe containers | where state = running"
```

The `--timeout` flag sets a time limit on each query execution. If a query takes longer than the specified seconds, it is aborted and an error is logged. This is useful for:
- `--watch` mode — prevent repeated queries from hanging on a slow Docker host
- `events` streams — auto-stop after a duration (e.g., `dol --timeout 60 "events containers"`)
- `alert` loops — each metrics collection call is individually timed out
- Store (historical) queries — abort if the store is slow to respond

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
dol config set theme light
dol config set api-timeout 60
dol config set stats-timeout 15
dol config set events-timeout 60

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
theme: light
api_timeout: 60
stats_timeout: 15
events_timeout: 60
webhook_timeout: 10
restart_timeout: 30
```

Available config keys for `dol config set`:

| Key | Default | Description |
|-----|---------|-------------|
| `store` | — | Path to SQLite telemetry store |
| `output` | `table` | Default output format (`table`, `json`, `csv`, `jsonl`) |
| `host` | — | Remote Docker daemon address (e.g., `tcp://192.168.1.100:2375`) |
| `metrics-interval` | `30` | Metrics collection interval in seconds |
| `snapshot-interval` | `300` | Snapshot collection interval in seconds |
| `theme` | `dark` | Table colour theme (`dark` or `light`) |
| `api-timeout` | `30` | Timeout for standard Docker API calls (seconds) |
| `api-quick-timeout` | `10` | Timeout for lightweight Docker API calls (ping, seconds) |
| `stats-timeout` | `10` | Timeout for per-container stats collection (seconds) |
| `events-timeout` | `30` | Max wait for a single Docker event (seconds) |
| `webhook-timeout` | `10` | HTTP timeout for alert webhook actions (seconds) |
| `restart-timeout` | `30` | Timeout for alert container restart actions (seconds) |

Config values can be overridden at runtime via CLI flags. For example, `--theme dark`
takes precedence over a `theme: light` setting in the config file.

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

The Docker host can also be set permanently via config file (`dol config set host tcp://...`) or the `DOCKER_HOST` environment variable. CLI `--host` takes precedence over config, which takes precedence over the environment.

### Interactive REPL

```bash
dol repl

dol> observe containers | where cpu > 50% | select name, cpu
dol> events containers | where action = die
dol> .help
```

REPL commands: `.help`, `.exit`/`.quit`, `.history`, `.watch <secs>`, `.export <path>`, `.output <fmt>`

See [`docs/tutorial.md`](docs/tutorial.md) for the full REPL guide.

### Terminal Dashboard

```bash
# Live-updating container monitor (top-like) with CPU/MEM gauge bars
dol top

# Multi-panel dashboard with containers, stats, and live events
dol dashboard
```

Both modes use an event-driven refresh model — container state changes trigger immediate updates via the Docker events API, with periodic metrics polling and a fallback full refresh.

**`dol top`** displays: NAME, IMAGE, CPU/MEM gauge bars (color-coded), STATE, STATUS, restart count.
Keybindings: `↑`/`↓` navigate, `s` sort, `d` direction, `/` filter, `r` refresh, `q` quit.

**`dol dashboard`** layout: container list (left), state histogram + top images (right), live events stream (bottom).
Keybindings: `Tab` switch panel, `r` refresh, `c` clear events, `q` quit.

Container states color-coded: running (green), exited/dead (red), paused (yellow), restarting (cyan).

### Real Alert Actions

```bash
# Webhook: sends an HTTP POST
dol 'alert when cpu > 85% for 2m then webhook "https://hooks.example.com/alert"'

# Restart: executes docker container restart via bollard API
dol 'alert when restart_count > 5 for 3m then restart container api'

# Alert history persisted to telemetry store (requires --store)
dol --store telemetry.db 'alert when cpu > 85% for 2m then print "High CPU"'
```

## Query Language Reference

A complete language reference is in [`docs/spec.md`](docs/spec.md). Key highlights:

**Targets:** `observe containers|images|networks|volumes`, `compose <project> [services|networks|volumes|health|images|stats|ps|logs|port|config]`, `compose ls`, `events`, `inspect`, `logs container <name>`, `ping`, `fields`, `analyze`, `alert`

**Pipeline nodes:** `where`, `select`, `group by`, `having`, `sort by`, `limit`, `offset`, `distinct`, `set`, `fill`, `let`, `if`/`else`, `alert`

**Expressions:** comparisons (`=`, `!=`, `>`, `<`, `>=`, `<=`, `contains`, `matches`, `in`, `starts_with`, `ends_with`), arithmetic (`+`, `-`, `*`, `/`, `%`), range (`between`, `is null`), logical (`and`, `or`, `not`), functions (`upper`, `lower`, `concat`, `coalesce`, `now`, `date_format`, `date_diff`, `extract`, etc.)

**Labels:** `label.env` dot notation — `dol "observe containers | where label.env = production"`

**Container fields:** `id`, `name`, `image`, `status`, `state`, `ports`, `labels`, `cpu`, `memory`, `memory_limit`, `restart_count`, `network_rx/tx`, `disk_read/write`, `compose_project`, `health`

### CLI Flags

| Flag / Subcommand | Description |
|------|-------------|
| `--store <path>` | Path to SQLite telemetry store |
| `--collect` | Start background data collection |
| `--metrics-interval <s>` | Metrics collection interval in seconds |
| `--snapshot-interval <s>` | Snapshot collection interval in seconds |
| `--store-stats` | Show telemetry store statistics |
| `--apply-retention` | Apply retention policies to the store |
| `--output <fmt>` | Output format: `table`, `json`, `json-compact`, `csv`, `jsonl` |
| `--export <path>` | Write output to file instead of stdout |
| `--file <path>` / `-f <path>` | Read the DOL query from a `.dol` file |
| `--host <addr>` | Docker daemon host address |
| `--watch <s>` | Re-run query every N seconds |
| `--timeout <s>` | Query execution timeout in seconds |
| `--explain` | Show query plan without executing |
| `--diff` | Compare results with last store snapshot |
| `--completion <shell>` | Generate shell completion script |
| `--export-format <fmt>` | Export file format: `influx`, `loki`, `prometheus` |
| `--theme <dark\|light>` | Table color theme (config override: `theme: dark\|light`) |
| `--export-influx <url>` | Push results to InfluxDB v1/v2 HTTP write API |
| `--export-grafana-loki <url>` | Push results to Grafana Loki HTTP push API |
| `--export-prometheus <url>` | Push results to Prometheus Pushgateway |
| `repl` | Interactive REPL with tab completion |
| `top` | Live TUI container monitor |
| `dashboard` | Multi-panel TUI (containers + events) |
| `config init` | Create default config file |
| `config set <key> <value>` | Update a config value |
| `config view` | Display current configuration |

### Error Messages

DOL provides descriptive, coloured parse errors with source pointers and "did you mean?" suggestions:

```
$ dol "observe containerz"
error: expected collection target, found `containerz`
  --> observe containerz
                   ^
  help: did you mean `containers`? try one of: containers, images, networks, volumes

$ dol 'alert when cpu > 80% then prnt "alert"'
error: expected alert action, found `prnt`
  --> alert when cpu > 80% then prnt "alert"
                                  ^
  help: did you mean `print`? try one of: `print`, `webhook`, `restart`

$ dol "observe containers | fltr name = test"
error: expected pipeline node
  --> observe containers | fltr name = test
                            ^
  help: did you mean `fill`? use | where, | select, | sort, | group by, | limit, | set, | if, | fill, or | let
```

## Examples

101 example queries are available in [`examples/`](examples/):

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
logs container my-app
logs container my-app tail 50
logs container my-app tail 200 | where message contains "error" | select line, message
ping
compose myapp
compose myapp services
compose myapp | where cpu > 80% | select name, cpu
compose myapp networks
compose myapp volumes
compose myapp health
compose myapp health | where health = "unhealthy" | select name, service, health
compose myapp images
compose myapp stats | where cpu > 50% | select name, service, cpu, memory
compose myapp ps | where state = "running" | select name, service, state, health
compose myapp logs api-service tail 50
compose myapp logs api-service tail 100 | where message contains "error"
compose myapp port api-service 8080
compose myapp config services
compose myapp config networks
compose myapp config volumes
compose ls
compose ls | where containers > 5 | sort by project asc
observe containers join images on id = id
observe containers join images on id = id | select c.name, i.repository
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
- [**Examples**](docs/examples.md) — categorized query reference with 60+ examples
- [**Architecture**](docs/architecture.md) — pipeline architecture and module overview
- [**Analysis**](docs/analyze.md) — anomaly detection and automated analysis
- [**Storage**](docs/storage.md) — telemetry store, retention, and historical queries
