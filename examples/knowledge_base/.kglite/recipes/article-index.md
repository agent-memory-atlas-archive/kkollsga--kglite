---
recipe: help
name: article_index
description: Return a deterministic prefix of the help article index.
recipe_description: Bounded navigation for the synthetic help corpus.
parameters:
  type: object
  properties:
    limit:
      type: integer
      minimum: 1
      maximum: 20
      default: 2
  required: []
  additionalProperties: false
---

```cypher
MATCH (n:Article)
RETURN n.id AS id, n.title AS title
ORDER BY id
LIMIT $limit
```
