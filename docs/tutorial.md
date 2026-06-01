# DOL Tutorial — Learn by Doing

This hands-on tutorial walks you through Docker Observability Language (DOL) from
your first query to advanced pipelines. Each step builds on the previous one, with
real examples you can run against your Docker environment.

---

## 1. Prerequisites & Installation

**You'll need:**

- Docker Engine running on your machine (local or remote)
- Rust 1.75+ toolchain (for building from source)

**Install DOL:**

```bash
# Option A: Build from source
git clone https://github.com/genc-murat/DockQL.git
cd DockQL
cargo build --release
alias dol='./target/release/dol'

# Option B: Install via cargo
cargo install dol --git https://github.com/genc-murat/DockQL

# Option C: Download a pre-built binary
# Grab the latest from https://github.com/genc-murat/DockQL/releases
```

**Verify it works:**

```bash
$ dol --version
dol 0.1.1
```

---

## 2. Your First Query

Let's start simple. The `observe` command gives you a snapshot of your Docker
environment. It's like `docker ps` but more powerful and with a query language.

```bash
dol "observe containers"
```

You'll see a table of all containers (running and stopped):

```text
┌──────────────┬──────────────────┬──────────┬────────┬─────────┐
│ name         │ image            │ state    │ status │ cpu     │
├──────────────┼──────────────────┼──────────┼────────┼─────────┤
│ web-01       │ nginx:1.25       │ running  │ Up 2h  │ 12.5%   │
│ api-gateway  │ envoy:1.29       │ running  │ Up 2h  │ 8.3%    │
│ redis-cache  │ redis:7-alpine   │ running  │ Up 3h  │ 2.1%    │
│ db-master    │ postgres:16      │ running  │ Up 1h  │ 15.7%   │
│ old-worker   │ python:3.11      │ exited   │ Ex 2d  │ 0.0%    │
└──────────────┴──────────────────┴──────────┴────────┴─────────┘
```

> **What happened?** DOL connected to your Docker daemon, listed all containers,
> fetched live metrics (CPU, memory), and rendered them as a formatted table.

**Try these variations:**

```bash
# List images instead of containers
dol "observe images"

# List networks
dol "observe networks"

# List volumes
dol "observe volumes"

# View the last 50 log lines from a container
dol "logs container my-app tail 50"

# Check if Docker daemon is reachable
dol "ping"
```

---

## 3. Filtering with `where`

The `where` clause filters results — only rows matching the condition are kept.

**Running containers only:**

```bash
dol "observe containers where state = running"
```

No more squinting at the `STATUS` column to find running containers.

**Containers running a specific image:**

```bash
dol "observe containers where image contains 'postgres'"
```

The `contains` operator does a case-sensitive substring match. Great for
finding all containers from a family of images.

**Combining conditions with `and` / `or`:**

```bash
dol "observe containers where (state = running and cpu > 50%) or image contains 'postgres'"
```

You can use parentheses to control precedence, just like in any programming
language.

**Other comparison operators:**

| Operator | Meaning           | Example                              |
|----------|-------------------|--------------------------------------|
| `=`      | equals            | `state = running`                    |
| `!=`     | not equals        | `state != exited`                    |
| `>`      | greater than      | `cpu > 80%`                          |
| `<`      | less than         | `memory < 100MB`                     |
| `>=`     | greater or equal  | `restart_count >= 3`                 |
| `<=`     | less or equal     | `cpu <= 50%`                         |
| `contains` | substring match | `image contains \"nginx\"`           |
| `matches`  | regex match     | `name matches \"^api-\"`            |
| `in`       | set membership   | `image in (\"postgres\", \"mysql\")` |
| `between`  | range check      | `cpu between 50 and 80`             |
| `is null`  | null check       | `finished_at is null`               |

---

## 4. Narrowing Output with `select`

When you only need a few columns, use `select`:

```bash
dol "observe containers | where state = running | select name, image, cpu"
```

```text
┌──────────────┬──────────────────┬──────┐
│ name         │ image            │ cpu  │
├──────────────┼──────────────────┼──────┤
│ web-01       │ nginx:1.25       │ 12.5 │
│ api-gateway  │ envoy:1.29       │ 8.3  │
│ redis-cache  │ redis:7-alpine   │ 2.1  │
│ db-master    │ postgres:16      │ 15.7 │
└──────────────┴──────────────────┴──────┘
```

**Discover available fields:**

```bash
dol "fields containers"
```

This shows all fields you can use in `select`, `where`, and other pipeline
stages. Try it for images, networks, and volumes too!

```bash
dol "fields images"
dol "fields networks"
dol "fields volumes"
```

---

## 5. Sorting and Limits

**Sort by CPU descending:**

```bash
dol "observe containers | where state = running | sort cpu desc | select name, cpu"
```

```text
┌──────────────┬──────┐
│ name         │ cpu  │
├──────────────┼──────┤
│ db-master    │ 15.7 │
│ web-01       │ 12.5 │
│ api-gateway  │ 8.3  │
│ redis-cache  │ 5.1  │
└──────────────┴──────┘
```

**Top 3 by memory:**

```bash
dol "observe containers | sort memory desc | limit 3 | select name, image, memory"
```

**Remove duplicates with `distinct`:**

```bash
dol "observe containers | distinct | select image"
```

This lists each unique image only once, even if multiple containers use it.

**Multi-field sort:**

Sort by state (running first), then by CPU within each state group:

```bash
dol "observe containers | sort by state desc, cpu desc | select name, state, cpu"
```

Each field can have its own direction (`asc` or `desc`).

**Pagination with `offset`:**

```bash
# Skip the first 5 containers, show the next 5
dol "observe containers | sort name asc | offset 5 | limit 5 | select name"
```

---

## 6. Building Pipelines

DOL queries are built as pipelines: data flows from left to right through
each stage (`|`). Every stage transforms the data stream.

```bash
# A typical pipeline:
dol "observe containers |
     where state = running |
     where cpu > 50% |
     select name, image, cpu |
     sort cpu desc |
     limit 5"
```

The order matters:

| Stage | What it does |
|-------|-------------|
| `observe containers` | Fetches all container data |
| `where state = running` | Drops non-running containers |
| `where cpu > 50%` | Keeps only high-CPU containers |
| `select name, image, cpu` | Keeps only 3 columns |
| `sort cpu desc` | Orders by CPU (highest first) |
| `limit 5` | Shows only top 5 |

> **Why pipeline order matters:** Putting `where` before `select` reduces the
> data volume early, making the query faster. DOL's planner also does
> automatic filter push-down to optimize execution.

**Boolean operators in filters:**

```bash
# Find either high-CPU or recently restarted containers
dol "observe containers | where (cpu > 80% and state = running) or restart_count > 3"
```

**Inline comments (`#`):**

```dol
observe containers          # list all containers
    | where state = running # only running ones
    | select name, image    # just these columns
```

---

## 7. Working with Labels & Docker Compose

Docker labels are key-value pairs attached to containers. DOL gives you two
ways to work with them.

**Filter by the full labels string:**

```bash
dol "observe containers | where labels contains 'env=prod'"
```

**Use dot notation for individual labels:**

If a container has `com.docker.compose.project=myapp`:

```bash
dol "observe containers | where label.com.docker.compose.project = 'myapp'"
```

**Docker Compose projects:**

The `compose_project` field is automatically populated for containers started
by Docker Compose:

```bash
# List all compose project names
dol "observe containers | select name, compose_project"

# Filter by compose project
dol "observe containers | where compose_project = 'myapp' | select name, state"
```

**Dedicated `compose` query family:**

DOL also has a dedicated query family for working with Compose projects.
These queries filter containers by the compose project label automatically:

```bash
# List all containers in the 'myapp' project
dol "compose myapp"

# List services in a compose project (adds a 'service' field)
dol "compose myapp services"

# Pipeline on compose results
dol "compose myapp | where cpu > 80% | select name, service, cpu | sort cpu desc"

# Alternative syntax using 'observe compose'
dol "observe compose myapp"
```

The `observe compose <project>` syntax is also supported, making it read
consistently with other `observe` sub-queries like `observe containers`.

**Compose networks:**

```bash
dol "compose myapp networks"
```

Lists Docker networks filtered by the compose project label.

**Compose volumes:**

```bash
dol "compose myapp volumes | select name, driver"
```

Lists Docker volumes filtered by the compose project label.

**Compose health:**

```bash
dol "compose myapp health"
```

Shows each container in the compose project with its `service` name and `health`
status (healthy, unhealthy, starting, or none).

---

## 8. Cross-Target JOIN

A JOIN merges rows from two Docker targets on a matching key. This lets you
correlate data across containers, images, networks, and volumes in a single
query.

```bash
dol "observe containers join images on id = id"
```

Output rows contain all fields from both targets, prefixed to avoid name
collisions:

- `c.` prefix for container fields (`c.name`, `c.image`, `c.state`)
- `i.` prefix for image fields (`i.repository`, `i.tag`, `i.size`)
- `n.` prefix for network fields
- `v.` prefix for volume fields

**Containers JOIN images with pipeline filtering:**

```bash
dol "observe containers join images on id = id | where c.image = 'nginx:latest' | select c.name, i.size"
```

**Containers JOIN with field selection:**

```bash
dol "observe containers join images on id = id | select c.name, c.state, i.repository, i.tag"
```

**How it works:**

1. The left target (`containers`) is fetched first.
2. For each left row, the right target (`images`) is scanned for matching rows.
3. Match is determined by evaluating the left key and right key expressions
   against their respective rows and comparing with `=`.
4. Matching row pairs are merged into a single output row with prefixed fields.
5. All downstream pipeline nodes use the prefixed field names.

---

## 9. Computing New Fields with `set`

The `set` stage adds or overrides a field on each row — like assigning a
variable in a loop.

**Add a static label:**

```bash
dol "observe containers | set tier = 'production' | select name, tier"
```

**Convert raw memory bytes to gigabytes:**

```bash
dol "observe containers | set mem_gb = memory / 1073741824 | select name, mem_gb"
```

Memory values come in bytes. Dividing by 1073741824 (1024³) converts to GB.

**Calculate memory usage percentage:**

```bash
dol "observe containers | set mem_pct = (memory / memory_limit) * 100 |
     select name, image, mem_pct | sort mem_pct desc"
```

> **Note:** If `memory_limit` is 0 (unlimited), this will produce a division
> by zero. Use a conditional or `coalesce()` (introduced in Step 9) to handle
> this safely.

**Conditional values with `if/then/else`:**

```bash
dol "observe containers | set health =
     if state = running then 'healthy'
     else if state = paused then 'degraded'
     else 'down' | select name, state, health"
```

**Case/when for multiple conditions:**

```bash
dol "observe containers | set severity = case
     when cpu > 80% then 'critical'
     when cpu > 50% then 'warning'
     else 'ok'
     end | select name, cpu, severity"
```

---

## 10. Filtering with String Functions

DOL provides several string functions for data transformation.

**Case-insensitive filtering:**

```bash
dol "observe containers | where upper(name) contains 'API' | select name"
```

The `upper()` function converts names to uppercase, so 'api-gateway',
'API-GATEWAY', and 'Api-Gateway' all match.

**Find containers with long names:**

```bash
dol "observe containers | where length(name) > 15 | select name"
```

**Concatenate fields:**

```bash
dol "observe containers | set label = concat(name, ':', image) | select name, label"
```

**Safely handle nulls with `coalesce`:**

```bash
dol "observe containers | set display_name =
     coalesce(label.name, name, 'unnamed') | select name, display_name"
```

`coalesce()` returns the first non-null, non-empty value from its arguments.
Here it tries `label.name` first, falls back to the container `name`, and
uses `'unnamed'` if both are null.

**Other string functions:**

| Function   | Description                 | Example                     |
|------------|-----------------------------|-----------------------------|
| `upper(s)` | Convert to uppercase        | `upper(name)`               |
| `lower(s)` | Convert to lowercase        | `lower(image)`              |
| `length(s)`| String length               | `length(name) > 10`         |
| `trim(s)`  | Strip whitespace            | `trim(name) = 'web-01'`     |
| `concat(a, b, ...)` | Concatenate strings | `concat(name, ':', image)` |
| `substring(s, start, len)` | Substring extraction | `substring(name, 0, 3)` |
| `coalesce(a, b, ...)` | First non-null value | `coalesce(label.env, 'dev')` |
| `starts_with(s, prefix)` | Prefix check (Boolean) | `starts_with(name, "api-")` |
| `ends_with(s, suffix)` | Suffix check (Boolean) | `ends_with(image, ":latest")` |
| `replace(s, from, to)` | Replace substring | `replace(name, "-", "_")` |
| `reverse(s)` | Reverse string | `reverse(name)` |
| `repeat(s, n)` | Repeat string | `repeat("-", 10)` |
| `position(s, substr)` | Find position (Integer) | `position(name, "api")` |
| `split_part(s, delim, n)` | Split & extract | `split_part(image, ":", 1)` |

#### String Pattern Operators

DOL also supports `starts_with` and `ends_with` as comparison operators for
filtering:

```bash
# Filter containers whose name starts with "api-"
dol "observe containers where name starts_with 'api-'"

# Filter images ending with ":latest"
dol "observe containers where image ends_with ':latest'"

# Combined with other filters
dol "observe containers where name starts_with 'api-' and state = running"
```

These operators are equivalent to the function forms but offer a more natural
reading syntax in `where` clauses.

---

## 11. Filling Null Values with `fill`

Docker sometimes returns null or empty values for optional fields like health
checks, labels, or finished timestamps. The `fill` pipeline node lets you
supply a default value for these fields.

**Fill missing memory values with 0:**

```bash
dol "observe containers | fill memory with 0 | select name, memory"
```

**Fill with an expression:**

```bash
dol "observe containers | fill name with coalesce(label.name, 'unnamed') | select name"
```

The `fill` node checks if a field is null or empty, and if so, replaces it
with the result of the expression.

---

## 12. Declaring Constants with `let`

The `let` pipeline node lets you declare constants and parameters that can be
referenced in downstream pipeline stages. This is especially useful for avoiding
hard-coded values in filters and making queries more readable.

**Declare a threshold parameter:**

```bash
dol "observe containers | let $threshold = 80 | where cpu > $threshold | select name, cpu"
```

**Declare an application name filter:**

```bash
dol "observe containers | let $app = 'myapp' | where compose_project = $app | select name, state"
```

**Use without the `$` prefix:**

The `$` prefix is optional. Both forms work identically:

```bash
# With $
dol "observe containers | let $threshold = 80 | where cpu > $threshold"

# Without $
dol "observe containers | let threshold = 80 | where cpu > threshold"
```

> **Note:** `let` is for declaring constant values and simple expressions. For
> per-row computed fields that reference other fields, use `set` instead.

---

## 13. Aggregation with `group by`

Group rows by field values to see summaries.

**Count containers by state:**

```bash
dol "observe containers | group by state"
```

```text
┌──────────┬───────┐
│ state    │ count │
├──────────┼───────┤
│ running  │ 4     │
│ exited   │ 1     │
│ paused   │ 1     │
└──────────┴───────┘
```

**Top 5 images by container count:**

```bash
dol "observe containers | group by image | sort by count desc | limit 5"
```

**With aggregate functions (avg, sum, min, max):**

```bash
# Average CPU per image
dol "observe containers | group by image with avg(cpu) as avg_cpu | sort by avg_cpu desc"

# Total memory per compose project
dol "observe containers | group by compose_project with sum(memory) as total_mem |
     sort by total_mem desc"
```

**Filter groups with `having`:**

Only show images with more than 2 containers:

```bash
dol "observe containers | group by image with count(id) as cnt | having cnt > 2"
```

The `having` clause is like `where` but operates on aggregate values *after*
grouping.

---

## 14. Date and Time Functions

DOL provides a set of date/time functions for timestamp manipulation. These
are especially useful for events, logs, and historical queries.

**Current time:**

```bash
dol "observe containers | set now = now() | select name, now"
```

**Format a timestamp:**

```bash
dol "observe containers | set day = date_format(created_at, '%Y-%m-%d') | select name, day"
```

**Difference between two timestamps:**

```bash
# Hours between creation and last start
dol "observe containers | set uptime = date_diff(created_at, started_at, 'hours') | select name, uptime"
```

**Extract a component from a timestamp:**

```bash
dol "observe containers | set month = extract(created_at, 'month') | select name, month"
```

Supported `extract` parts: `year`, `month`, `day`, `hour`, `minute`, `second`

Supported `date_diff` units: `seconds`, `minutes`, `hours`, `days`

#### `$var` Field References

Field names can be prefixed with `$` for explicit field access. This is
particularly useful on the right side of a comparison where bare identifiers
are treated as literal values:

```bash
# Without $, 'running' is a literal value
dol "observe containers where state = running"

# With $, $state is explicitly a field reference
dol "observe containers where $state = running"
```

Both forms produce the same result. The `$` prefix is especially helpful when
a field name might collide with a keyword or when writing scripts.

---

## 15. Streaming Events

`events` opens a live stream from the Docker event bus. It keeps running until
you press Ctrl+C.

Group rows by field values to see summaries.

**Count containers by state:**

```bash
dol "observe containers | group by state"
```

```text
┌──────────┬───────┐
│ state    │ count │
├──────────┼───────┤
│ running  │ 4     │
│ exited   │ 1     │
│ paused   │ 1     │
└──────────┴───────┘
```

**Top 5 images by container count:**

```bash
dol "observe containers | group by image | sort by count desc | limit 5"
```

**With aggregate functions (avg, sum, min, max):**

```bash
# Average CPU per image
dol "observe containers | group by image with avg(cpu) as avg_cpu | sort by avg_cpu desc"

# Total memory per compose project
dol "observe containers | group by compose_project with sum(memory) as total_mem |
     sort by total_mem desc"
```

**Filter groups with `having`:**

Only show images with more than 2 containers:

```bash
dol "observe containers | group by image with count(id) as cnt | having cnt > 2"
```

The `having` clause is like `where` but operates on aggregate values *after*
grouping.

---

## 13. Date and Time Functions

DOL provides a set of date/time functions for timestamp manipulation. These
are especially useful for events, logs, and historical queries.

**Current time:**

```bash
dol "observe containers | set now = now() | select name, now"
```

**Format a timestamp:**

```bash
dol "observe containers | set day = date_format(created_at, '%Y-%m-%d') | select name, day"
```

**Difference between two timestamps:**

```bash
# Hours between creation and last start
dol "observe containers | set uptime = date_diff(created_at, started_at, 'hours') | select name, uptime"
```

**Extract a component from a timestamp:**

```bash
dol "observe containers | set month = extract(created_at, 'month') | select name, month"
```

Supported `extract` parts: `year`, `month`, `day`, `hour`, `minute`, `second`

Supported `date_diff` units: `seconds`, `minutes`, `hours`, `days`

#### `$var` Field References

Field names can be prefixed with `$` for explicit field access. This is
particularly useful on the right side of a comparison where bare identifiers
are treated as literal values:

```bash
# Without $, 'running' is a literal value
dol "observe containers where state = running"

# With $, $state is explicitly a field reference
dol "observe containers where $state = running"
```

Both forms produce the same result. The `$` prefix is especially helpful when
a field name might collide with a keyword or when writing scripts.

## 14. Streaming Events

`events` opens a live stream from the Docker event bus. It keeps running until
you press Ctrl+C.

**Watch all container events live:**

```bash
dol "events containers"
```

**Filter to specific event types:**

```bash
# Only crash events
dol "events containers where action = 'die'"

# Restart events with selected columns
dol "events containers | where action = 'restart' | select time, container, image"
```

**Stop after N events:**

```bash
dol "events containers | limit 10"
```

**Historical replay (requires `--store`):**

```bash
dol --store telemetry.db "events containers last 1h"
dol --store telemetry.db "events containers from '2026-05-30 10:00:00Z' to '2026-05-30 11:00:00Z' where action = 'oom'"
```

**Monitor different resource types:**

```bash
# Image pulls
dol "events images | where action = 'pull'"

# Network connections
dol "events networks | where action = 'connect' | select time, actor_id"

# Volume mounts
dol "events volumes | where action = 'mount'"
```

---

## 16. Setting Up Alerts

Alerts run continuously, evaluating a condition and triggering an action when
it's been true for a specified duration.

**Simple CPU alert:**

```bash
dol 'alert when cpu > 85% for 2m then print "High CPU detected"'
```

This monitors all containers. If any container's CPU stays above 85% for 2
consecutive minutes, DOL prints the message.

**Alert with webhook (POST to a URL):**

```bash
dol 'alert when memory > 90% for 1m then webhook "https://hooks.example.com/alert"'
```

**Auto-restart a container on restart loop:**

```bash
dol 'alert when restart_count > 5 for 3m then restart container api-service'
```

> **Warning:** The `restart` action actually runs `docker restart <container>`.
> Use with caution in production.

**Inline alert in a pipeline:**

```bash
dol "observe containers | where cpu > 80% | alert 'High CPU detected'"
```

This fires an alert for each container that passes the filter at query time —
a one-shot check, not a continuous monitor.

**Persist alert history (with `--store`):**

```bash
dol --store telemetry.db 'alert when restart_count > 3 for 5m then print "Restart loop"'
```

All fired alerts are logged to the `alert_history` table in the telemetry
store. You can review them later with SQLite queries.

---

## 17. Time Travel (Historical Queries)

With a telemetry store configured, you can query the past. This requires the
background collector to be running.

**Start the collector:**

```bash
dol --store telemetry.db --collect
```

This polls Docker every 30 seconds (metrics) and takes snapshots every 5
minutes. Let it run for a while to accumulate data.

**Observe containers as they were 10 minutes ago:**

```bash
dol --store telemetry.db "observe containers last 10m"
```

**Inspect a specific container at a point in time:**

```bash
# Current state
dol "inspect container db-master"

# State right before last night's outage
dol --store telemetry.db 'inspect container db-master at "2026-05-30 04:59:59Z"'
```

**Replay events from a time window:**

```bash
dol --store telemetry.db \
  'events containers last 1h | where action = "die" | group by image | sort by count desc'
```

> **Tip:** Time travel is invaluable for post-mortems. Instead of guessing
> what happened, you can see exactly which containers were running and what
> their metrics looked like at the time of the incident.

---

## 18. Automated Analysis

The `analyze` command runs deterministic checks across your Docker environment.

**Find all anomalies:**

```bash
dol "analyze containers find anomalies"
```

This detects:

- **Restart loops** — containers restarting frequently
- **High CPU** — sustained CPU above thresholds
- **Memory pressure** — containers near their memory limits
- **Unhealthy states** — containers that have exited or died
- **Deployment errors** — frequent `die` events (requires `--store`)

**Diagnose a specific container:**

```bash
dol "explain container api-service"
```

Shows a detailed diagnostic: current state, key metrics, and any detected
anomalies affecting that container.

**Find related containers (blast radius):**

```bash
dol "analyze containers correlate"
```

Groups containers by shared images and labels. Useful for understanding: "If
this container fails, what else is affected?"

**Analyze container dependencies:**

```bash
dol "analyze containers find dependencies"
```

Maps out compose project groupings, network attachments, and volume dependencies.

**Analyze container density:**

```bash
dol "analyze containers find density"
```

Shows container distribution across images, states, and compose projects with percentages.

**Detect memory leaks (requires `--store`):**

```bash
dol --store telemetry.db "analyze containers find leaks"
```

Analyzes historical metric samples to find containers with sustained memory growth (≥20%).

**Detect configuration drift (requires `--store`):**

```bash
dol --store telemetry.db "analyze containers find drift"
```

Compares the two most recent telemetry snapshots and reports image/state/label/restart changes.

**Identify restart loops historically:**

```bash
dol --store telemetry.db "analyze containers find restart_loops last 30m"
```

---

## 19. Using the REPL

The interactive REPL gives you a shell with tab completion, command history,
and in-session state.

**Start it up:**

```bash
dol repl
```

```text
DOL REPL — type .help for commands, Ctrl+C or .exit to quit

dol>
```

**Run queries interactively:**

```text
dol> observe containers | where state = running | select name, cpu
dol> events containers | limit 5
```

**REPL commands:**

| Command           | Description                       |
|-------------------|-----------------------------------|
| `.help`           | Show available commands           |
| `.exit` / `.quit` | Exit the REPL                     |
| `.history`        | Show command history              |
| `.watch <secs>`   | Re-run the last query every N sec |
| `.export <path>`  | Write results to a file           || `.output <fmt>`   | Set output format (table, json, json-compact, csv, jsonl) |

> **Error feedback:** If a query has a syntax error, DOL shows a detailed
> error message with the exact column position, the surrounding query context
> (with `-->`), and a `^` pointer under the error location. Errors are
> displayed in **red** for easy visual scanning.

**Auto-refresh a query:**

```text
dol> observe containers | where cpu > 80% | select name, cpu
dol> .watch 5
```

This re-runs the query every 5 seconds — like a poor man's monitoring
dashboard.

**Prevent hanging with `--timeout`:**

When using `--watch` or running long-lived queries, use `--timeout` to set a
maximum execution time per query:

```bash
# Stop watching if a query takes longer than 10 seconds
dol --watch 5 --timeout 10 "observe containers"

# Auto-stop an events stream after 60 seconds
dol --timeout 60 "events containers"

# Timeout alert metrics collection
dol --timeout 15 'alert when cpu > 85% for 2m then print "High CPU"'
```

If the query exceeds the timeout, it's aborted and an error is shown. The
`--watch` loop continues to the next iteration.

**`--watch` + Alert Integration**

The `--watch` flag works with alert queries too. When combined, the watch
interval controls the alert evaluation cadence instead of the hardcoded 5-second
loop:

```bash
# Evaluate alert every 3 seconds
dol --watch 3 'alert when cpu > 80% then print "High"'

# With timeout to prevent hanging on slow metrics
dol --watch 5 --timeout 10 'alert when cpu > 85% for 2m then print "High CPU"'
```

When `--watch` is used without an alert query, it re-runs the batch query at the
specified interval (existing behavior). When used with an alert query, it drives
the evaluation loop timing.

---

## 20. Next Steps

You've covered all the core DOL features. Here's what to explore next:

| Resource | Description |
|----------|-------------|
| [Query Examples](examples.md) | 54 categorized examples for every feature |
| [Language Spec](spec.md) | Complete syntax, types, and operator reference |
| [Architecture](architecture.md) | How DOL works under the hood |
| [Analysis Docs](analyze.md) | Anomaly detection and health scoring |
| [Storage Docs](storage.md) | Telemetry store schema and retention |
| [TUI Dashboard](examples.md#14-terminal-dashboard) | `dol top` and `dol dashboard` commands |
| [API Docs](https://genc-murat.github.io/DockQL/dol/index.html) | Rust API documentation (cargo doc) |

**Quick tips to remember:**

- Use `""` quotes for the full query string: `dol "observe containers"`
- Use backticks or single quotes for string literals *inside* the query
- Pipeline order matters: `where` early, `select` mid, `sort`/`limit` late
- `--store telemetry.db` unlocks historical queries, alert history, and `--diff`
- `--watch <secs>` repeats batch queries for live monitoring
- `--output json` for machine-readable output, pipe to `jq` for processing
- `--host tcp://<addr>:2375` to query a remote Docker daemon
