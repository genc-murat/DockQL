## 22. Pipeline Debug Node

**Inspect intermediate pipeline state:**
```dol
observe containers | debug | where cpu > 80% | debug | select name, cpu
```
Output (to stderr):
```text
[debug] row_count=15, schema=[name:string, image:string, cpu:number, ...]
[debug] row_count=3, schema=[name:string, cpu:number]
```

See [`examples/debug_pipeline.dol`](examples/debug_pipeline.dol) for a runnable example.

## 23. Type Conversion Functions (to_int, to_float, to_string)

**Convert restart_count (integer) to float for arithmetic:**
```dol
observe containers | set ratio = to_float(restart_count) / 5 | select name, restart_count, ratio | limit 5
```

**Convert memory bytes to string for display:**
```dol
observe containers | set mem_str = to_string(memory) | select name, mem_str
```

**Parse a string field as integer:**
```dol
observe containers | set level = to_int("42") | select name, level
```

See [`examples/type_conversion.dol`](examples/type_conversion.dol) for a runnable example.

## 24. Assertion Pipeline Node

**Validate that all rows satisfy a condition:**
```dol
observe containers | assert restart_count >= 0 | select name, state, restart_count
```

**Assert after filtering — only validates matching rows:**
```dol
observe containers | where state = running | assert restart_count < 10 | select name, restart_count
```

**Assert in a compose context:**
```dol
compose myapp ps | assert state = "running" or state = "exited"
```

When a row fails the assertion, the query terminates with an error:
```text
error: assertion failed: `restart_count >= 0`
```

See [`examples/assert_pipeline.dol`](examples/assert_pipeline.dol) for a runnable example.

## 25. Extended Fill with Conditions

**Basic fill — replace null/missing values:**
```dol
observe containers | fill memory with 0 | select name, memory
```

**Fill with where condition — only fill matching rows:**
```dol
observe containers | fill restart_count with 0 where state = running
```

**Fill with if/else expression value:**
```dol
observe containers | fill tier with if state = running then "active" else "inactive"
```

**Fill with case/when expression value:**
```dol
observe containers | fill status_label with case when state = running then "up" when state = exited then "down" else "unknown" end
```

**Fill with where condition — only fill matching rows:**
```dol
observe containers | fill tier with if state = running then "active" else "inactive" where state = running | select name, state, tier
```

See [`examples/fill_conditional.dol`](examples/fill_conditional.dol) for a runnable example.

## 26. Median and Percentile Aggregate Functions

**Compute median CPU per image:**
```dol
observe containers | group by image with median(cpu) as med_cpu | sort by med_cpu desc
```

**Compute 95th percentile of memory per image:**
```dol
observe containers | group by image with percentile(memory, 95) as p95_mem | sort by p95_mem desc
```

**Combine multiple aggregates:**
```dol
observe containers | group by image with avg(cpu) as avg_cpu, median(cpu) as med_cpu, percentile(cpu, 99) as p99_cpu
```

**Percentile on restart count to detect outlier containers:**
```dol
observe containers | group by image with percentile(restart_count, 99) as p99_restarts | where p99_restarts > 5
```

**Combine median and percentile in a single query:**
```dol
observe containers | group by image with median(memory) as med_mem, percentile(memory, 95) as p95_mem
```

See [`examples/agg_median_percentile.dol`](examples/agg_median_percentile.dol) for a runnable example.

## 27. Compose Project Events & Config

**Compose project events (batch) & network events (streaming):**
```dol
compose myapp events
compose myapp events | where action = "die" | select time, container
compose myapp networks | where action = connect
compose myapp networks | where action = "connect" | select time, actor_id
```

**Inspect Compose Configuration (services, networks, volumes):**
```dol
compose myapp config
compose myapp config services | where image = "api:latest" | select name, image, ports
compose myapp config networks | select name, driver
compose myapp config volumes
```

## 28. Colored Error Messages with Suggestions

When you make a mistake in a DOL query, the parser returns an ANSI-colored
error message with a source pointer and a suggestion:

```text
[1m[31merror[0m: parse error at column 35: unknown query family�J
  [36m-->[0m [32m|[0m ohserv containers
  [36m-->[0m [32m|[0m ^
  [33mhelp:[0m try one of: `observe`, `events`, `inspect`, `logs`, `analyze`, `fields`, `compose`, `ping`, `alert`
```

**Unknown query family:**
```dol
# Typo: "ohserv" instead of "observe"
dol "ohserv containers"
# Suggestion: try one of: `observe`, `events`, `inspect`, `logs`, `analyze`, `fields`, `compose`, `ping`, `alert`
```

**Wrong target name:**
```dol
# Typo: "contaienrs" instead of "containers"
dol "observe contaienrs"
# Suggestion: did you mean `containers`?
```

**Empty query:**
```dol
dol ""
# Suggestion: try `observe containers`
```

**Unknown pipeline node:**
```dol
# Typo: "whre" instead of "where"
dol "observe containers | whre cpu > 80%"
# Suggestion: try one of: `where`, `select`, `sort by`, `group by`...
```

**Unterminated string:**
```dol
dol "observe containers where name = \"my-app"
# Suggestion: add a closing `"` to terminate the string
```

## 29. String Pattern Operators (`starts_with`, `ends_with`)

**Filter by name prefix:**
```dol
observe containers where name starts_with "api-"
```

**Filter by image suffix:**
```dol
observe containers where image ends_with ":latest"
```

**Combined with other conditions:**
```dol
observe containers where name starts_with "db-" and state = running
```

## 30. `fill` Pipeline Node

**Fill null memory values with 0:**
```dol
observe containers | fill memory with 0 | where memory > 500
```

**Fill name with a coalesced value:**
```dol
observe containers | fill name with coalesce(label.name, name, "unnamed") | select name
```

## 31. Date/Time Functions

**Current timestamp:**
```dol
observe containers | set now = now() | select name, now
```

**Format creation date:**
```dol
observe containers | set created_date = date_format(created_at, "%Y-%m-%d") | select name, created_date
```

**Time difference:**
```dol
observe containers | set uptime_hours = date_diff(created_at, started_at, "hours") | select name, uptime_hours
```

**Extract timestamp part:**
```dol
observe containers | set day = extract(created_at, "day") | select name, day
observe containers | set hour = extract(created_at, "hour") | select name, hour
observe containers | set month = extract(created_at, "month") | group by month
```

## 32. `let` Pipeline Node (Constants & Parameters)

**Declare a threshold and filter by it:**
```dol
observe containers | let $threshold = 80 | where cpu > $threshold | select name, cpu
```

**Declare an application name:**
```dol
observe containers | let $app = "myapp" | where compose_project = $app | select name, state
```

**Declare multiple parameters:**
```dol
observe containers | let $min_cpu = 50 | let $max_cpu = 90 | where cpu between $min_cpu and $max_cpu
```

## 33. `$var` Field References

**Explicit field access with $ prefix:**
```dol
observe containers where $state = running
observe containers | set label = concat($name, ":", $image) | select label
```

## 34. Cross-Target JOIN Queries

Join rows from two targets on a matching key. Output fields are prefixed with
an auto-generated alias (`c.` for containers, `i.` for images, `n.` for networks,
`v.` for volumes).

**Containers JOIN images on ID:**
```dol
observe containers join images on id = id | select c.name, i.repository, i.tag
```

**Containers JOIN images with where filter:**
```dol
observe containers join images on id = id | where c.image = "nginx:latest" | select c.name, i.size
```

**Containers JOIN networks (no matching rows — demonstrates schema):**
```dol
observe containers join networks on name = name | select c.name, n.name
```

## 35. Compose Project Queries

**List all Compose projects:**
```dol
compose ls
compose ls | sort by project asc
compose ls | where containers > 5
compose ls | where status = "running"
```

**Query Compose project images:**
```dol
compose myapp images
compose myapp images | sort by size desc
compose myapp images | select name, tag, size
```

**Compose project resource stats:**
```dol
compose myapp stats
compose myapp stats | where cpu > 80% | select name, service, cpu, memory
compose myapp stats | group by service with sum(cpu) as total_cpu
```

**Enhanced container status (ps):**
```dol
compose myapp ps
compose myapp ps | where state = "running" | select name, service, health
compose myapp ps | where health = "unhealthy"
```

**Service logs:**
```dol
compose myapp logs api-service tail 50
compose myapp logs api-service tail 100 | where message contains "error"
compose myapp logs worker tail 20 | select line, message
```

**Port mappings:**
```dol
compose myapp port api-service 8080
compose myapp port web 80
```

**Inspect Compose configuration:**
```dol
compose myapp config
compose myapp config services
compose myapp config services | where image = "api:latest"
compose myapp config networks
compose myapp config volumes
```

## 36. Window Functions

**Assign row numbers to each row:**
```dol
observe containers | row_number as rn | where rn = 1 | select name, state, rn
observe containers | sort by restart_count desc | row_number as rn | where rn <= 3
```

**Rank containers by a field (ties get same rank):**
```dol
observe containers | rank(state) as pos | select name, state, pos
observe containers | sort by restart_count desc | rank(restart_count) as rank
```

**Access previous row values (lag):**
```dol
observe containers | lag(state, 1) as prev_state | select name, state, prev_state
observe containers | lag(cpu, 2) as cpu_2_rows_ago | where cpu > cpu_2_rows_ago
```

**Access next row values (lead):**
```dol
observe containers | lead(state, 1) as next_state | select name, state, next_state
observe containers | lead(memory, 1) as next_mem | select name, memory, next_mem
```

**Combined with sort for meaningful ordering:**
```dol
observe containers | sort by name asc | row_number as rn | lag(state, 1) as prev_state
```

See [`examples/window_functions.dol`](examples/window_functions.dol) for a runnable example.


## 37. Configurable Timeouts for Docker API Calls

All Docker API timeout durations are configurable via `dol config set` or the
config file. Run `dol config set <key> <value>` to change any:

```bash
# Docker API call timeout (default: 30s)
dol config set api-timeout 60

# Lightweight call timeout (ping, fast operations, default: 10s)
dol config set api-quick-timeout 15

# Per-container stats timeout (default: 10s)
dol config set stats-timeout 20

# Events stream per-item timeout (default: 30s)
dol config set events-timeout 60

# Alert webhook HTTP POST timeout (default: 10s)
dol config set webhook-timeout 30

# Alert container restart timeout (default: 30s)
dol config set restart-timeout 60
```

**View current configuration:**
```bash
dol config view
```

**Config file example (YAML):**
```yaml
store: ~/.dol/store
output: table
host: unix:///var/run/docker.sock
metrics_interval: 10
snapshot_interval: 60
theme: dark
api_timeout: 30
api_quick_timeout: 10
stats_timeout: 10
events_timeout: 30
webhook_timeout: 10
restart_timeout: 30
```