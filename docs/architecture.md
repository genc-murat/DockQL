# DOL Architecture

This document outlines the high-level architecture of the Docker Observability Language (DOL) engine.

## Overview

DOL is designed as a pipeline that reads query strings, parses them into an Abstract Syntax Tree (AST), plans execution against underlying data sources (Docker API, metrics, SQLite store), and presents results to the user.

```text
┌─────────────────┐
│ User Input      │ (CLI, File, REPL)
└───────┬─────────┘
        │
        ▼
┌─────────────────┐
│ Parser          │ (Recursive Descent)
└───────┬─────────┘
        │
        ▼ (AST)
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
│                      (Where, Select, Sort, Limit)                           │
└──────────────────────────────────────┬──────────────────────────────────────┘
                                       │
                                       ▼ (ExecutionResult)
                               ┌───────┴───────┐
                               │ Output Formatter│
                               │ (Table, JSON)   │
                               └───────────────┘
```

## Core Components

### 1. Parser (`src/parser.rs`)
A hand-written recursive descent parser that converts raw DOL strings into a strongly typed AST (`src/ast.rs`). It features detailed error reporting with line/column context and explicit precedence handling for boolean and pipeline operators.

### 2. Executor (`src/executor.rs`)
The central coordinator that matches the AST against the requested query type (`observe`, `events`, `inspect`, `analyze`, `alert`). It dispatches to the correct engine module based on the verb.

### 3. Data Providers
- **Docker Client (`src/docker.rs`):** Interfaces with the Docker Engine daemon (currently via Docker CLI wrapping) to list containers, images, volumes, networks, and stream events.
- **Metrics Collector (`src/metrics.rs`):** Collects and normalizes live container metrics (CPU, Memory, Network I/O). Uses a ring buffer in memory to provide rolling averages if needed.
- **Telemetry Store (`src/storage.rs`, `src/sqlite_store.rs`):** Embedded SQLite database that persists metrics, events, and state snapshots for historical "time-travel" queries and retention.

### 4. Background Collector (`src/collector.rs`)
A standalone asynchronous task (`tokio`) that periodically polls the Docker API and writes metrics/snapshots to the Telemetry Store.

### 5. Analysis Engine (`src/analyze.rs`)
A deterministic rules engine that scans telemetry data for anomalies (high CPU, memory pressure, restart loops, deployment errors) and computes container health signals.

### 6. Alerting Engine (`src/alerts.rs`)
Evaluates conditions against live metrics/state at intervals. Manages duration guards (e.g., `for 2m`) to prevent flapping, and triggers actions (Print, Webhook, Restart) when conditions are met.

## Data Flow: Example Pipeline

When executing `observe containers where cpu > 80% | select name, cpu | sort cpu desc limit 5`:

1. **Fetch Data**: The Executor fetches all running containers via Docker Client and their current metrics via Metrics Collector.
2. **Merge**: Containers and metrics are zipped together into `Row` representations.
3. **Pipeline Filtering (`where`)**: The `where cpu > 80%` node evaluates the AST `Expression` against each row. Rows evaluating to `false` are dropped.
4. **Pipeline Projection (`select`)**: The `select name, cpu` node drops all columns except `name` and `cpu`.
5. **Pipeline Sorting (`sort`)**: The `sort cpu desc` node orders the rows in memory.
6. **Pipeline Limiting (`limit`)**: The `limit 5` node truncates the output to the top 5 rows.
7. **Render**: The resulting `ExecutionResult` is formatted as a Markdown-style table or JSON.
