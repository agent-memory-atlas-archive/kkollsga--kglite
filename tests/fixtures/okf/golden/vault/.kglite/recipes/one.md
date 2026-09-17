---
recipe: vault
name: by_keyword
description: The notes carrying one keyword.
recipe_description: Navigating the golden vault.
parameters:
  {type: object, properties: {keyword: {type: string}},
   required: [keyword], additionalProperties: false}
---

```cypher
MATCH (n)-[:HAS_KEYWORD]->(k:Keyword {id: $keyword})
RETURN n.concept_id AS id, n.title AS title ORDER BY title LIMIT 50
```
