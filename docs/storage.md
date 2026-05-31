# DOL Historical Storage & Time Travel

This document describes the historical storage layer and time-travel query
capabilities added in Phase 8, along with Layer 2 enhancements (historical
observe, diff mode, passive collection).

## Overview

DOL supports persisting Docker telemetry (metrics, events, and snapshots) to
an embedded SQLite database. Once data is collected, you can run historical
queries to inspect the state of your Docker environment at any point in the past,
compare container states over time, and run historical observe queries.

## Architecture

```text
┌─────────────────┐     ┌──────────────────┐
│   DOL CLI       │────▶│   Query Router   │
└─────────────────┘     └────────┬─────────┘
                                  │
             ┌────────────────────┼────────────────────┐
             ▼                    ▼                    ▼
    ┌────────────────┐  ┌────────────────┐   ┌────────────────┐
    │  Batch Execute │  │  Historical    │   │  Stream        │
    │  (Docker API)  │  │  Execute       │   │  Execute       │
    └────────────────┘  │  (SQLite)      │   └────────────────┘
                        └────────┬───────┘
                                 │
                        ┌────────▼───────┐
                        │  SQLite Store  │
                        │  (TelemetryStore)│
                        └────────────────┘
```

## Storage Schema

The SQLite database contains three tables:

### `metrics`
| Column          | Type    | Description                    |
|-----------------|---------|--------------------------------|
| id              | INTEGER | Primary key                    |
| container_id    | TEXT    | Docker container ID            |
| container_name  | TEXT    | Container name                 |
| timestamp       | TEXT    | ISO 8601 timestamp             |
| cpu_percent     | REAL    | CPU usage percentage           |
| memory_usage    | INTEGER | Memory usage in bytes          |
| memory_limit    | INTEGER | Memory limit in bytes          |
| network_rx      | INTEGER | Network bytes received         |
| network_tx      | INTEGER | Network bytes transmitted      |
| disk_read       | INTEGER | Disk bytes read                |
| disk_write      | INTEGER | Disk bytes written             |

### `events`
| Column     | Type    | Description                         |
|------------|---------|-------------------------------------|
| id         | INTEGER | Primary key                         |
| time       | TEXT    | ISO 8601 timestamp                  |
| event_type | TEXT    | Docker event type (container, etc.) |
| action     | TEXT    | Event action (start, stop, die)     |
| actor_id   | TEXT    | Actor (container) ID                |
| container  | TEXT    | Container name (nullable)           |
| image      | TEXT    | Image name (nullable)               |
| attributes | TEXT    | JSON-serialized key-value pairs     |

### `snapshots`
| Column    | Type    | Description                                    |
|-----------|---------|------------------------------------------------|
| id        | INTEGER | Primary key                                    |
| timestamp | TEXT    | ISO 8601 timestamp                             |
| data      | TEXT    | JSON-serialized full Docker state snapshot      |

### `alert_history`
| Column          | Type    | Description                    |
|-----------------|---------|--------------------------------|
| id              | INTEGER | Primary key                    |
| timestamp       | TEXT    | ISO 8601 timestamp             |
| container_id    | TEXT    | Docker container ID            |
| container_name  | TEXT    | Container name                 |
| rule_condition  | TEXT    | The alert condition string     |
| action_type     | TEXT    | Action type (print, webhook, restart) |
| action_detail   | TEXT    | Action parameters (URL, target name) |
| success         | INTEGER | Whether the action succeeded   |

## CLI Usage

### Starting the Background Collector

To collect telemetry data, run DOL in collector mode:

```bash
# Start collecting with default intervals (metrics: 30s, snapshots: 5m)
dol --store telemetry.db --collect

# Custom intervals
dol --store telemetry.db --collect --metrics-interval 10 --snapshot-interval 60
```

Press `Ctrl+C` to stop the collector gracefully.

### Historical Queries

Once you have collected data, you can query the past:

```bash
# Inspect a container's state at a specific time
dol --store telemetry.db "inspect container api at \"2026-01-01 12:00:00\""

# Observe containers as they were 5 minutes ago
dol --store telemetry.db "observe containers last 5m"

# Observe containers at a specific time
dol --store telemetry.db "observe containers at \"2026-01-01 12:00:00\""

# Replay events in a time range
dol --store telemetry.db 'events containers from "2026-01-01T12:00:00Z" to "2026-01-01T13:00:00Z"'

# Replay events with filtering
dol --store telemetry.db 'events containers from "2026-01-01T12:00:00Z" to "2026-01-01T13:00:00Z" where action = "die" | select time, action, container'
```

### Diff Mode

Compare current container state with the last stored snapshot to see what changed:

```bash
dol --store telemetry.db "observe containers" --diff
```

Diff output shows:
- **Added containers** (green) — containers that appeared since the last snapshot
- **Removed containers** (red) — containers that disappeared since the last snapshot
- **Changed containers** — containers whose state transitioned (e.g., `running → exited`)

### Store Management

```bash
# View store statistics
dol --store telemetry.db --store-stats

# Apply retention policy (cleanup old data)
dol --store telemetry.db --apply-retention
```

### Passive Collection During Normal Use

When `--store` is provided during normal operations (alerts, events), DOL will
automatically persist data to the store:

```bash
# Stream events and persist them
dol --store telemetry.db "events containers where action = \"die\""

# Run alert evaluation and persist metrics
dol --store telemetry.db 'alert when cpu > 85% for 2m then print "High CPU"'
```

## Retention Policies

By default, the following retention thresholds apply:

| Data Type  | Default Retention |
|------------|-------------------|
| Metrics    | 7 days            |
| Events     | 30 days           |
| Snapshots  | 30 days           |

Use `--apply-retention` to clean up data older than these thresholds.

## TelemetryStore Trait

The `TelemetryStore` trait defines the storage interface:

```rust
pub trait TelemetryStore {
    fn write_metric(&mut self, sample: MetricSample) -> Result<(), TelemetryError>;
    fn latest_metrics(&self) -> Result<Vec<MetricSample>, TelemetryError>;
    fn metrics_between(&self, from: &str, to: &str) -> Result<Vec<MetricSample>, TelemetryError>;
    fn write_event(&mut self, event: DockerEvent) -> Result<(), TelemetryError>;
    fn events_between(&self, from: &str, to: &str) -> Result<Vec<DockerEvent>, TelemetryError>;
    fn write_snapshot(&mut self, snapshot: TelemetrySnapshot) -> Result<(), TelemetryError>;
    fn snapshot_at_or_before(&self, timestamp: &str) -> Result<Option<TelemetrySnapshot>, TelemetryError>;
    fn write_alert_event(&mut self, event: AlertHistoryEvent) -> Result<(), TelemetryError>;
    fn alert_history(&self, from: &str, to: &str) -> Result<Vec<AlertHistoryEvent>, TelemetryError>;
}
```

Two implementations exist:
- **`InMemoryTelemetryStore`**: For testing and ephemeral use.
- **`SqliteTelemetryStore`**: For persistent storage with retention policies.

## Implementation Notes

- Timestamps are stored as ISO 8601 strings for lexicographic ordering.
- Snapshots serialize the full Docker state (containers, images, networks,
  volumes) as a single JSON blob for atomic reads.
- Event attributes are stored as JSON arrays of `[key, value]` pairs.
- SQLite indices on timestamp columns ensure efficient range queries.
- The `TimeSelector::Last` variant computes a proper time window relative to
  the current time using chrono, rather than scanning all data.
- Diff mode queries `snapshot_at_or_before` for the latest snapshot and compares
  container IDs and states with current results.
