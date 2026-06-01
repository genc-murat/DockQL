## 24. String Pattern Operators (`starts_with`, `ends_with`)

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

## 25. `fill` Pipeline Node

**Fill null memory values with 0:**
```dol
observe containers | fill memory with 0 | where memory > 500
```

**Fill name with a coalesced value:**
```dol
observe containers | fill name with coalesce(label.name, name, "unnamed") | select name
```

## 26. Date/Time Functions

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

## 27. `$var` Field References

**Explicit field access with $ prefix:**
```dol
observe containers where $state = running
observe containers | set label = concat($name, ":", $image) | select label
```

## 28. Cross-Target JOIN Queries

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