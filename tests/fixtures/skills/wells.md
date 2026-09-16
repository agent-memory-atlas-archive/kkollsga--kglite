---
name: wells
description: "How to ask this graph about wells and the fields they drain."
references_tools:
  - cypher_query
  - graph_overview
delivery: eager
---

# Wells methodology

Start from `MATCH (w:Well)` — every well carries `name`, `spud_date` and a
`DRAINS` edge to its field. Aggregate by field before filtering by date; the
date column is sparse on wells older than 1990.
