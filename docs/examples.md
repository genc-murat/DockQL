# DOL Query Examples

> **New to DOL?** Start with the [step-by-step Tutorial](tutorial.md) for a
> structured introduction. This page is a reference you can come back to for
> quick examples. **58 example queries** are available in the
> [`examples/`](https://github.com/genc-murat/DockQL/tree/main/examples) directory.

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

> **Pipeline order tip:** Put `where` filters *before* `select` to reduce data
> early — see the [Tutorial](tutorial.md#6-building-pipelines) for why.

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

## 4. Container Logs (`logs`)

Retrieve log output from running containers. The `logs` query returns lines with
`line` number, `message` content, and `container` name.

**View the last 100 lines (default):**
```dol
logs container my-app
```

**View the last 50 lines:**
```dol
logs container my-app tail 50
```

**Filter logs with pipeline:**
```dol
logs container my-app | where message contains "error" | select line, message
```

**Filter logs with tail and pipeline:**
```dol
logs container my-app tail 200 | where message contains "error" | select line, message
```

## 5. Docker Connectivity (`ping`)

Test connectivity to the Docker daemon. Returns `status: ok` and a message on
success, or `status: error` with details on failure.

**Basic ping:**
```dol
ping
```

## 6. Docker Compose Projects (`compose`)

Query containers within a Docker Compose project. The `compose` query family
automatically filters by the `com.docker.compose.project` label.

**List all containers in a compose project:**
```dol
compose myapp
```

**List services (adds a `service` field):**
```dol
compose myapp services
```

**Use `observe compose` syntax:**
```dol
observe compose myapp
```

**Compose with pipeline filtering:**
```dol
compose myapp | where cpu > 80% | select name, service, cpu | sort cpu desc
```

**Compose with alert:**
```dol
alert when compose_project = 'myapp' and cpu > 85% for 2m then print "High CPU in myapp"
```

## 7. Alerting and Monitoring (`alert`)

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

**Alert with `--watch` (custom evaluation cadence):**
```bash
# Evaluate alert every 3 seconds
dol --watch 3 'alert when cpu > 80% then print "High"'

# With timeout to prevent hanging metrics collection
dol --watch 5 --timeout 10 'alert when cpu > 85% for 2m then print "High CPU"'
```

**Alert with timeout (prevents hanging metrics collection):**
```bash
dol --timeout 15 'alert when cpu > 85% for 2m then print "High CPU"'
```

Each metrics collection cycle is individually timed out. If `docker stats` doesn't respond within 15 seconds, the cycle is aborted and the next one begins.

When `--watch` is used with an alert query, the watch interval controls the
evaluation cadence instead of the default 5-second loop.

## 8. Streaming and Event History (`events`)

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

**Auto-stop an events stream after a timeout:**
```bash
# Stop streaming after 60 seconds
dol --timeout 60 "events containers"

# Combine with filter
dol --timeout 30 "events containers | where action = die"
```

The `--timeout` flag is especially useful for events streams in scripts — it ensures the command doesn't run indefinitely. The stream is automatically terminated when the timeout is reached.

**Replay events from a specific 1-hour window yesterday (requires `--store`):**
```dol
events containers 
    from "2026-05-30T10:00:00Z" 
    to "2026-05-30T11:00:00Z" 
    where action = "oom"
```

## 9. Time Travel (`inspect ... at` / `observe ... last`)

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

## 10. Automated Insights (`analyze`)

The deterministic analysis engine automatically surfaces problems without writing complex queries.

**Find all anomalies (CPU, Memory, Restart Loops, Deployment Errors):**
```dol
analyze containers find anomalies
```

**Find related containers for blast radius analysis:**
```dol
analyze container api-service correlate
```

**Map container dependencies (compose, network, volume):**
```dol
analyze containers find dependencies
```

**Analyze container density by image, state, and project:**
```dol
analyze containers find density
```

**Detect memory resource leaks (requires `--store`):**
```dol
analyze containers find leaks
```

**Detect configuration drift between snapshots (requires `--store`):**
```dol
analyze containers find drift
```

## 11. Complex Pipelines

You can chain multiple operations together using the `|` pipe operator.
Data flows from left to right through each stage.

> **Pipeline order matters:** `where` filters should come first (reduces
> data volume early), then `select` (narrows columns), then `sort`/`limit`.
> See the [Tutorial](tutorial.md#6-building-pipelines) for a full walkthrough
> with a stage-by-stage breakdown.

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

## 12. Interactive REPL

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

## 13. Config Management

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

## 14. Terminal Dashboard

Interactive TUI monitors for live container observability.

### Live Container Monitor (`dol top`)

```bash
dol top
```

Displays a full-screen table of all containers with event-driven auto-refresh — container state changes (start, die, stop, etc.) trigger an immediate update via a background `docker events` listener, with a 2-second periodic metrics poll and a 30-second fallback full refresh. Columns: NAME, IMAGE, CPU (gauge bar), MEM (gauge bar), MEMORY (usage %), STATE, STATUS, RST (restart count). CPU and MEM gauge bars are color-coded: green (<50%), yellow (50–80%), red (>80%).

Keyboard controls:
- `↑`/`↓` or `j`/`k` — navigate rows
- `s` — cycle sort column (name, image, state, status)
- `d` — toggle sort direction
- `r` — force refresh
- `/` — enter filter mode (filter containers by name, case-insensitive)
- `h` — toggle help overlay
- `q` / Esc — quit

Color coding: running (green), exited/dead (red), paused (yellow), restarting (cyan).

### Multi-Panel Dashboard (`dol dashboard`)

```bash
dol dashboard
```

Three-panel view:
- **Left panel**: Container list with name, CPU%, memory usage, state
- **Right panel**: State distribution histogram (running/exited/paused/other) with bar chart + top images by count
- **Bottom panel**: Live Docker events stream (real-time via a background `docker events` API listener — events appear instantly as they occur, no polling)

Keyboard controls:
- `Tab` — switch panel focus (containers / stats)
- `r` — force refresh
- `c` — clear events panel
- `h` — toggle help overlay
- `q` / Esc — quit

## 15. Arithmetic Expressions

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

## 16. String Functions

Apply string transformations with `upper()`, `lower()`, `length()`, `trim()`, `concat()`, `substring()`, `coalesce()`.

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

**Coalesce (first non-null value):**
```dol
observe containers | set display_name = coalesce(label.name, name, "unknown")
```

## 17. Range and Null Checks

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

## 18. Aggregation with Functions

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

## 19. Multi-Field Sort

Sort by multiple fields with independent direction per field.

> See the [Tutorial](tutorial.md#5-sorting-and-limits) for more on `sort by`,
> `limit`, `offset`, and `distinct`.

**Sort by state then CPU:**
```dol
observe containers | sort by state desc, cpu desc | select name, state, cpu
```

**Sort by image then name:**
```dol
observe containers | sort by image asc, name asc | select name, image
```

## 20. Distinct and Offset

**Remove duplicate rows (distinct):**
```dol
observe containers | distinct | select image
```

**Paginate with offset and limit:**
```dol
observe containers | sort by name asc | offset 5 | limit 5 | select name
```

> The [Tutorial](tutorial.md#5-sorting-and-limits) covers sort, limit, offset,
> and distinct with a step-by-step progression.

## 21. Inline Comments

Comments start with `#` and extend to end of line.

```dol
observe containers          # list all containers
    | where state = running # only running ones
    | select name, image    # just these columns
```

## 22. External Integrations

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



