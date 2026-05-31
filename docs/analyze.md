# DOL Analysis & Insight Engine

This document describes the deterministic analysis capabilities added in Phase 9.
The analysis engine provides anomaly detection, health scoring, and correlation features without relying on external AI/LLM dependencies.

## Overview

The `analyze` and `explain` query families help you move beyond raw telemetry by automatically surfacing patterns and issues in your Docker environment.

## Commands

### 1. `analyze containers find anomalies`
Scans all running containers and live metrics to detect issues. 
Outputs a prioritized list of anomalies ranked by severity (Critical > Warning > Info).

**Detected Anomalies:**
- **Restart Loops**: Containers that have restarted too many times.
- **High CPU**: Containers exceeding CPU utilization thresholds.
- **Memory Pressure**: Containers approaching their memory limits.
- **Unhealthy States**: Containers in `exited`, `dead`, or `restarting` states.
- **Deployment Errors**: Containers with frequent `die` events in historical storage.

**Example:**
```dol
analyze containers find anomalies
```

### 2. `explain container <name>`
Produces a detailed diagnostic summary for a specific container, including its current state, key metrics (CPU, memory, network), and any detected anomalies affecting it.

**Example:**
```dol
explain container api-service
# Equivalent to: analyze container api-service explain
```

### 3. `analyze containers explain`
Produces a diagnostic summary for *all* containers in the system.

**Example:**
```dol
analyze containers explain
```

### 4. `analyze containers correlate`
Finds commonalities between containers. This is useful for grouping related services or identifying blast radiuses.

**Supported Correlations:**
- **Shared Images**: Multiple containers running the exact same image.
- **Shared Labels**: Containers sharing the same label keys/values.

**Example:**
```dol
analyze containers correlate
```

### 5. `analyze containers find dependencies`
Maps out container dependencies across compose projects, networks, and volumes.

**Outputs per dependency type:**
- **compose_project**: Containers grouped by `com.docker.compose.project` label.
- **network**: Containers attached to each Docker network.
- **volume**: Detected volumes (without direct container mapping).

**Example:**
```dol
analyze containers find dependencies
```

### 6. `analyze containers find density`
Analyzes container distribution across images, states, and compose projects with density percentages.

**Dimensions:**
- **image**: How many containers run each image.
- **state**: Distribution by container state (running, exited, etc.).
- **compose_project**: Container grouping by compose project.
- **total**: Summary row with total container count.

**Example:**
```dol
analyze containers find density
```

### 7. `analyze containers find leaks` *(requires `--store`)*
Detects potential memory leaks by analyzing historical metric samples. Compares the first and last memory readings per container and flags those where memory usage grew beyond a threshold (default: 20% increase).

**Requirements:**
- A telemetry store (`--store telemetry.db`) with collected metrics.
- At least 3 metric samples per container.

**Example:**
```dol
analyze containers find leaks
# (requires --store)
```

### 8. `analyze containers find drift` *(requires `--store`)*
Detects configuration drift by comparing the two most recent telemetry snapshots. Emits anomalies for any changes between snapshots.

**Detected Drift Types:**
- **config_drift**: Container image changed.
- **state_change**: Container state changed (e.g., running → exited).
- **restart_increase**: Restart count increased.
- **label_drift**: Container labels added or removed.
- **container_appeared**: New container appeared in latest snapshot.
- **container_disappeared**: Container removed from latest snapshot.
- **config_baseline**: Single snapshot — reports current configuration as baseline.

**Example:**
```dol
analyze containers find drift
# (requires --store)
```

## Thresholds

The anomaly detectors use the following default thresholds:

| Metric | Warning Threshold | Critical Threshold |
|--------|-------------------|--------------------|
| CPU | 80% | 95% |
| Memory (Usage/Limit) | 85% | 95% |
| Restart Count | 3 | 6 |
| Deployment Errors (`die` events) | 3 | 6 |
| Resource Leak (memory increase) | 20% | 40% |
| Resource Leak (min samples) | 3 | — |

## Historical Analysis

If you have a telemetry store configured (`--store telemetry.db`), the analysis engine can also process historical data.

For example, `analyze containers find anomalies` will cross-reference live metrics with the historical event stream to identify deployment errors (e.g., frequent `die` events) that might not be visible in a live snapshot alone.

## Architecture

The engine is built around a set of stateless detector functions in `src/analyze.rs`.
Each detector inspects a slice of telemetry (e.g., `&[MetricSample]` or `&[Container]`) and yields zero or more `Anomaly` structs.

```rust
pub struct Anomaly {
    pub severity: Severity,
    pub kind: String,
    pub container: String,
    pub message: String,
    pub evidence: Vec<String>,
}
```

The execution layer aggregates these anomalies, sorts them by severity, and transforms them into standard tabular `Row` formats for CLI presentation.
