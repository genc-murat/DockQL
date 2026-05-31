## 23. Cross-Target JOIN Queries

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