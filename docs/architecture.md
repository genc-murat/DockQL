# DOL Architecture

This document outlines the high-level architecture of the Docker Observability Language (DOL) engine.

## Overview

DOL is designed as a pipeline that reads query strings, parses them into an Abstract Syntax Tree (AST), plans execution against underlying data sources (Docker API, metrics, SQLite store), and presents results to the user.

```text
┌─────────────────┐
│ User Input      │ (CLI, File, Config)
└───────┬─────────┘
        │
        ▼
┌─────────────────┐
│ Parser          │ (Recursive Descent)
└───────┬─────────┘
        │
        ▼ (AST)
┌─────────────────┐
│ Planner         │ (Filter push-down, plan display)
└───────┬─────────┘
        │
        ▼ (LogicalPlan)
┌─────────────────┐
│ Executor        │
│ Dispatcher      │
└───────┬─────────┘
        │
   ┌────┼──────────────────────────────┐──────────────────────────────┐
   ▼    ▼                              ▼                              ▼
┌───────┴───────┐              ┌───────┴───────┐              ┌───────┴───────┐
│ Docker Client │              │ Metrics Coll. │              │ Telemetry     │
│ (Live State)  │              │ (Live Stats)  │              │ Store         │
└───────┬───────┘              └───────┬───────┘              └───────┬───────┘
   │    │                              │                              │
   │    └──────────────────────────────┼──────────────────────────────┘
   ▼                                   ▼
┌──────────────────────────────────────┴──────────────────────────────────────┐
│                            Query Pipeline Engine                            │
│                 (Where, Select, Sort, Limit, GroupBy, Set)                  │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                        │
                                        ▼ (ExecutionResult)
                                ┌───────┴───────┐
                                │ Output         │
                                │ Formatter      │
                                │ (Table, CSV,   │
                                │  JSON, JSONL)  │
                                └───────────────┘
```

## Core Components

### 1. Parser (`src/parser.rs`)
A hand-written recursive descent parser that converts raw DOL strings into a strongly typed AST (`src/ast.rs`). It features detailed error reporting with line/column context and explicit precedence handling for boolean and pipeline operators. Supports `matches` (regex), `in`, label dot-access, and `fields` introspection queries.

### 2. Planner (`src/planner.rs`)
Produces a `LogicalPlan` from the AST, performing filter push-down optimizations (e.g., moving `where` conditions closer to the data source). The plan is displayable for the `--explain` CLI flag, which shows the execution plan without running the query.

### 3. Executor (`src/executor.rs`)
The central coordinator that matches the AST against the requested query type (`observe`, `events`, `inspect`, `analyze`, `alert`, `fields`). It dispatches to the correct engine module based on the verb, applies pipeline stages, and formats results. Supports four output formats: table, CSV, JSON, and JSONL. ANSI-colored table output is auto-detected when the terminal supports it. The `render_diff` function compares current results against a stored snapshot.

### 4. Data Providers
- **Docker Client (`src/docker.rs`):** Interfaces with the Docker Engine daemon (currently via Docker CLI wrapping) to list containers, images, volumes, networks, stream events, and inspect individual containers for enriched fields (`started_at`, `finished_at`, `restart_count`).
- **Metrics Collector (`src/metrics.rs`):** Collects and normalizes live container metrics (CPU, Memory, Network I/O). Uses a ring buffer in memory to provide rolling averages if needed.
- **Telemetry Store (`src/storage.rs`, `src/sqlite_store.rs`):** Embedded SQLite database that persists metrics, events, and state snapshots for historical "time-travel" queries and retention.

### 5. Background Collector (`src/collector.rs`)
A standalone asynchronous task (`tokio`) that periodically polls the Docker API and writes metrics/snapshots to the Telemetry Store.

### 6. Analysis Engine (`src/analyze.rs`)
A deterministic rules engine that scans telemetry data for anomalies (high CPU, memory pressure, restart loops, deployment errors) and computes container health signals.

### 7. Alerting Engine (`src/alerts.rs`)
Evaluates conditions against live metrics/state at intervals. Manages duration guards (e.g., `for 2m`) to prevent flapping, and triggers actions when conditions are met. Actions are executed in real time:
- **Webhook**: Sends an HTTP POST to the configured URL via `reqwest`.
- **Restart**: Runs `docker restart <container>` via `std::process::Command`.
- **Alert history**: Fired alerts are persisted to the telemetry store's `alert_history` table when `--store` is active.

### 8. Config Loader & Subcommand (`src/config.rs`)
Loads DOL settings from YAML or TOML files at standard paths (`~/.config/dol/config.yaml`, `.dolrc`, `dol.yaml`). Supports `store`, `output`, `host`, `metrics_interval`, and `snapshot_interval` settings. The `dol config init|set|view` subcommand provides CLI-based config management.

### 9. Interactive REPL (`src/repl.rs`)
A readline-based interactive shell (`dol repl`) with tab completion for DOL keywords, command history (persisted across sessions), and REPL-specific commands (`.watch`, `.export`, `.output`, `.history`, `.help`). Supports all query types: observe, events, inspect, alert, and fields.

## Data Flow: Example Pipeline

When executing `observe containers where cpu > 80% | select name, cpu | sort cpu desc limit 5`:

1. **Parse**: The parser tokenizes and builds an AST representing the query.
2. **Plan**: The planner produces a LogicalPlan, applying filter push-down to evaluate `cpu > 80%` as early as possible.
3. **Fetch Data**: The Executor fetches all running containers via Docker Client and their current metrics via Metrics Collector.
4. **Merge**: Containers and metrics are zipped together into `Row` representations.
5. **Pipeline Filtering (`where`)**: The `where cpu > 80%` node evaluates the AST `Expression` against each row. Rows evaluating to `false` are dropped.
6. **Pipeline Projection (`select`)**: The `select name, cpu` node drops all columns except `name` and `cpu`.
7. **Pipeline Sorting (`sort`)**: The `sort cpu desc` node orders the rows in memory.
8. **Pipeline Limiting (`limit`)**: The `limit 5` node truncates the output to the top 5 rows.
9. **Render**: The resulting `ExecutionResult` is formatted as a Markdown-style table, CSV, JSON, or JSONL depending on the `--output` flag.

## CLI Integration

The CLI (`src/cli.rs`) uses `clap` for argument parsing. Key flags include:

- `--output <table|csv|json|jsonl>` — output format selection
- `--explain` — show logical plan without executing
- `--watch <s>` — repeat query every N seconds
- `--diff` — compare with last store snapshot
- `--export <path>` — write output to file
- `--host <addr>` — remote Docker daemon address
- `--completion <shell>` — generate shell completion script
