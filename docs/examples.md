## 23. Compose Project Events & Config

**Stream Compose project events:**
```dol
compose myapp events
compose myapp events | where action = "die" | select time, container
```

**Inspect Compose Configuration (services, networks, volumes):**
```dol
compose myapp config
compose myapp config services | where image = "api:latest" | select name, image, ports
compose myapp config networks | select name, driver
compose myapp config volumes
```

## 24. Colored Error Messages with Suggestions

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

## 25. String Pattern Operators (`starts_with`, `ends_with`)

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

## 26. `fill` Pipeline Node

**Fill null memory values with 0:**
```dol
observe containers | fill memory with 0 | where memory > 500
```

**Fill name with a coalesced value:**
```dol
observe containers | fill name with coalesce(label.name, name, "unnamed") | select name
```

## 27. Date/Time Functions

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

## 28. `let` Pipeline Node (Constants & Parameters)

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

## 29. `$var` Field References

**Explicit field access with $ prefix:**
```dol
observe containers where $state = running
observe containers | set label = concat($name, ":", $image) | select label
```

## 30. Cross-Target JOIN Queries

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

## 31. Compose Project Queries

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

## 32. Configurable Timeouts for Docker API Calls

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