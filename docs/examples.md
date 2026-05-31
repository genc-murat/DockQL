# DOL Query Examples

This document provides a reference of common, useful, and complex Docker Observability Language (DOL) queries.

## 1. Observing Live State (`observe`)

The `observe` command provides a snapshot of current Docker entities.

**List all containers:**
```dol
observe containers
```

**List only running containers:**
```dol
observe containers where state = running
```

**Find containers using a specific image base and select key columns:**
```dol
observe containers 
    where image contains "postgres" 
    | select name, status, ports
```

**List the 10 largest images:**
```dol
observe images | sort by size desc | limit 10
```

**List all networks:**
```dol
observe networks | select name, driver, scope
```

**List all volumes sorted by name:**
```dol
observe volumes | sort by name asc
```

**Find containers with high CPU:**
```dol
observe containers 
    | where cpu > 80% 
    | select name, image, cpu, memory
    | sort cpu desc
```

**Find containers consuming too much memory:**
```dol
observe containers 
    | where memory > 500MB and state = running
    | select name, memory, memory_limit
    | sort memory desc
```

**Find the top 3 memory-hungry databases:**
```dol
observe containers 
    | where image contains "mysql" or image contains "postgres"
    | select name, image, cpu, memory
    | sort memory desc
    | limit 3
```

**Filter containers by label (full label string):**
```dol
observe containers 
    | where labels contains "env=prod"
    | select name, image, state
```

**Filter by individual label using dot notation:**
```dol
observe containers 
    | where label.env = "prod"
    | select name, image, state
```

**Filter containers exposing a specific port:**
```dol
observe containers 
    | where ports contains "443"
    | select name, ports, image
```

**Regex matching on container names:**
```dol
observe containers 
    | where name matches "^api-"
    | select name, status, cpu
```

**IN operator for multiple values:**
```dol
observe containers 
    | where image in ("postgres", "mysql", "redis")
    | select name, image, state
```

## 2. Schema Introspection (`fields`)

Discover which fields are available for a target type.

**List all container fields:**
```dol
fields containers
```

**List all image fields:**
```dol
fields images
```

**List all network fields:**
```dol
fields networks
```

**List all volume fields:**
```dol
fields volumes
```

## 3. Aggregation (`group by`)

Group rows by one or more fields. Each group gets a `count` column.

**Count containers by state:**
```dol
observe containers | group by state
```

**Count containers per image, top 5:**
```dol
observe containers 
    | group by image 
    | sort by count desc 
    | limit 5
```

**Group events by image (streaming):**
```dol
events containers 
    | where action = "die" 
    | group by image
```

## 4. Alerting and Monitoring (`alert`)

Alerts run continuously and trigger actions when conditions are met for a duration.

**Print an alert when CPU is high for 2 minutes:**
```dol
alert when cpu > 85% for 2m then print "High CPU"
```

**Database container memory pressure with webhook:**
```dol
alert when name contains "worker" and memory > 90% for 5m
then webhook "https://alerts.mycompany.com/hooks/memory"
```

**Auto-restart a container on restart loop:**
```dol
alert when restart_count > 5 for 3m 
then restart container api-service
```

**Alert with real webhook POST:**
```dol
alert when cpu > 85% for 2m 
then webhook "https://hooks.example.com/alert"
```

**Inline alert in a pipeline:**
```dol
observe containers 
    | where cpu > 80% 
    | alert "High CPU detected"
```

## 5. Streaming and Event History (`events`)

`events` lets you tap into the Docker event bus. All resource types are supported: containers, images, networks, and volumes.

**Stream container crash events live:**
```dol
events containers where action = "die"
```

**Stream container restart events with selected columns:**
```dol
events containers 
    | where action = "restart" 
    | select time, container, image
```

**Monitor image pulls:**
```dol
events images | where action = "pull"
```

**Monitor network connection events:**
```dol
events networks 
    | where action = "connect"
    | select time, actor_id
```

**Monitor volume mount events:**
```dol
events volumes | where action = "mount"
```

**Take the first 10 events and stop:**
```dol
events containers | limit 10
```

**Replay events from a specific 1-hour window yesterday (requires `--store`):**
```dol
events containers 
    from "2026-05-30T10:00:00Z" 
    to "2026-05-30T11:00:00Z" 
    where action = "oom"
```

## 6. Time Travel (`inspect ... at` / `observe ... last`)

Historical queries allow you to view the exact state of containers as they were at a specific moment in the past. Requires `--store`.

**Inspect a container's current state:**
```dol
inspect container api-service
```

**Inspect a container's state right before an outage:**
```dol
inspect container db-master at "2026-05-30 04:59:59Z"
```

**Inspect a specific image:**
```dol
inspect image postgres:16
```

**Observe containers as they were 10 minutes ago:**
```dol
observe containers last 10m
```

**Observe containers at a specific point in time:**
```dol
observe containers at "2026-05-30 12:00:00Z"
```

## 7. Automated Insights (`analyze`)

The deterministic analysis engine automatically surfaces problems without writing complex queries.

**Find all anomalies (CPU, Memory, Restart Loops, Deployment Errors):**
```dol
analyze containers find anomalies
```

**Find related containers for blast radius analysis:**
```dol
analyze container api-service correlate
```

## 8. Complex Pipelines

You can chain multiple operations together using the `|` pipe operator.

**High CPU containers grouped by image:**
```dol
observe containers 
    | where cpu > 50% 
    | group by image 
    | sort by count desc
```

**Alert pipeline with sort and limit:**
```dol
observe containers 
    | where memory > 80% 
    | sort memory desc 
    | limit 3 
    | alert "Top memory consumers under pressure"
```

**Combined boolean filter with aggregation:**
```dol
observe containers 
    | where (state = running and cpu > 60%) or restart_count > 3
    | group by image
```

**Regex filtering with field selection:**
```dol
observe containers 
    | where name matches "^db-" 
    | select name, image, state, cpu
```

**Label-based filtering with compose_project:**
```dol
observe containers 
    | where label.com.docker.compose.project = "myapp"
    | select name, compose_project, state
```

## 9. Interactive REPL

Start an interactive shell with `dol repl`. Tab completion, command history, and REPL commands available.

```text
$ dol repl
DOL REPL — type .help for commands, Ctrl+C or .exit to quit

dol> observe containers | where cpu > 50% | select name, cpu
dol> .watch 3
dol> events containers | where action = die
dol> .help
```

### REPL Commands

| Command | Description |
|---------|-------------|
| `.help` | Show available commands |
| `.exit` / `.quit` | Exit the REPL |
| `.history` | Show command history |
| `.watch <secs>` | Re-run last query every N seconds |
| `.export <path>` | Set export file path |
| `.output <fmt>` | Set output format |

## 10. Config Management

Create and manage DOL configuration:

```bash
# Create a default config file
dol config init

# Set a config value
dol config set store ~/dol.db
dol config set host tcp://192.168.1.100:2375

# View current config
dol config view
```

Supported config keys: `store`, `output`, `host`, `metrics-interval`, `snapshot-interval`.

## 11. Terminal Dashboard

Interactive TUI monitors for live container observability.

### Live Container Monitor (`dol top`)

```bash
dol top
```

Displays a full-screen table of all containers with auto-refresh every 2 seconds. Columns: NAME, IMAGE, STATE, STATUS.

Keyboard controls:
- `↑`/`↓` or `j`/`k` — navigate rows
- `s` — cycle sort column
- `d` — toggle sort direction
- `r` — force refresh
- `q` / Esc — quit

Color coding: running (green), exited/dead (red), paused (yellow), restarting (cyan).

### Multi-Panel Dashboard (`dol dashboard`)

```bash
dol dashboard
```

Split-screen view with containers list (top) and live Docker events (bottom). Events are polled from `docker events --until 5s`.

Keyboard controls:
- `Tab` — switch panel focus
- `r` — force refresh
- `q` / Esc — quit

## 12. Arithmetic Expressions

Compute new fields using arithmetic with `+`, `-`, `*`, `/`, `%`.

**Convert memory to gigabytes:**
```dol
observe containers | set mem_gb = memory / 1073741824 | select name, mem_gb
```

**Calculate memory percentage:**
```dol
observe containers | set mem_pct = (memory / memory_limit) * 100 | select name, mem_pct
```

**Filter by derived value:**
```dol
observe containers | where (memory / 1073741824) > 1 | select name, memory
```

## 13. String Functions

Apply string transformations with `upper()`, `lower()`, `length()`, `trim()`, `concat()`, `substring()`.

**Normalize names to uppercase:**
```dol
observe containers | where upper(name) contains "API"
```

**Concatenate fields:**
```dol
observe containers | set label = concat(name, ":", image) | select name, label
```

**Filter by name length:**
```dol
observe containers | where length(name) > 10 | select name
```

## 14. Range and Null Checks

**Between operator for inclusive range checks:**
```dol
observe containers where cpu between 50 and 80
```

**Filter containers that have finished:**
```dol
observe containers where finished_at is not null | select name, finished_at
```

**Find containers missing a value:**
```dol
observe containers where compose_project is null | select name
```

## 15. Aggregation with Functions

Group by with `sum`, `count`, `avg`, `min`, `max`.

**Average CPU per image:**
```dol
observe containers | group by image with avg(cpu) as avg_cpu | sort by avg_cpu desc
```

**Count containers per state with having filter:**
```dol
observe containers | group by state with count(id) as cnt | having cnt > 1
```

**Sum memory per compose project:**
```dol
observe containers | group by compose_project with sum(memory) as total_mem | sort by total_mem desc
```

## 16. Multi-Field Sort

Sort by multiple fields with independent direction per field.

**Sort by state then CPU:**
```dol
observe containers | sort by state desc, cpu desc | select name, state, cpu
```

**Sort by image then name:**
```dol
observe containers | sort by image asc, name asc | select name, image
```

## 17. Distinct and Offset

**Remove duplicate rows (distinct):**
```dol
observe containers | distinct | select image
```

**Paginate with offset and limit:**
```dol
observe containers | sort by name asc | offset 5 | limit 5 | select name
```

## 18. Inline Comments

Comments start with `#` and extend to end of line.

```dol
observe containers          # list all containers
    | where state = running # only running ones
    | select name, image    # just these columns
```

## 19. External Integrations

Push query results directly to external monitoring systems.

### InfluxDB

```bash
# Push container state to InfluxDB v1
dol --export-influx "http://localhost:8086/write?db=dol" "observe containers"

# Push metrics to InfluxDB v2
dol --export-influx "http://localhost:8086/api/v2/write?org=myorg&bucket=dol" \
    "observe containers | where state = running"
```

Each row is converted to InfluxDB line protocol:
```
containers,name=web,image=nginx:latest,state=running cpu=12.5,memory=64000000
```

### Grafana Loki

```bash
# Push container state to Loki
dol --export-grafana-loki "http://localhost:3100" "observe containers"

# Push filtered events
dol --export-grafana-loki "http://localhost:3100" \
    "events containers | where action = die | select time, container, action"
```

Each row is pushed as a Loki log entry with labels `app=dol,source=docker`.

### Prometheus Pushgateway

```bash
# Push container metrics to Prometheus Pushgateway
dol --export-prometheus "http://localhost:9091" "observe containers"
```

Numeric fields become gauge metrics:
```
dol_cpu{container="web",image="nginx:latest",state="running"} 12.5
dol_memory{container="web",image="nginx:latest",state="running"} 64000000
```

### Export Format for Files

Write results in a format suitable for external tools:

```bash
# InfluxDB line protocol file
dol --export metrics.influx --export-format influx "observe containers"

# Prometheus exposition format file
dol --export metrics.prom --export-format prometheus "observe containers"

# Loki JSON payload file
dol --export metrics.loki --export-format loki "observe containers"
```



