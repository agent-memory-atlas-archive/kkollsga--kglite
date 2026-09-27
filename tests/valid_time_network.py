"""The declared stop network and the seven trailing-aggregate shapes the
streaming pipeline folds, shared by the per-surface streaming goldens.

Stop 2 closes in 2005, the direct 1→3 link runs 2000–2005, and 4→5 has two
parallel links (2000–2005 and from 2006), so ``AT`` (2008) filters both nodes
and relationships."""

NETWORK = [
    "CREATE (s1:Stop {id: 1}), (s2:Stop {id: 2, vf: date('2000-01-01'), vt: date('2005-01-01')}),"
    " (s3:Stop {id: 3}), (s4:Stop {id: 4}), (s5:Stop {id: 5}),"
    " (s1)-[:LINK]->(s2), (s2)-[:LINK]->(s3),"
    " (s1)-[:LINK {since: date('2000-01-01'), until: date('2005-01-01')}]->(s3),"
    " (s1)-[:LINK]->(s4),"
    " (s4)-[:LINK {since: date('2000-01-01'), until: date('2005-01-01')}]->(s5),"
    " (s4)-[:LINK {since: date('2006-01-01')}]->(s5), (s5)-[:LINK]->(s3)",
    "CALL db.temporal.declare({node: 'Stop', from: 'vf', to: 'vt', convention: 'closed'})"
    " YIELD declared RETURN declared",
    "CALL db.temporal.declare({relationship: 'LINK', from: 'since', to: 'until', convention: 'half_open'})"
    " YIELD declared RETURN declared",
]

STREAMING_SHAPES = [
    "MATCH (:Stop {id: 1})-[:LINK*1..3]->(t) RETURN count(DISTINCT t) AS c",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, count(t) AS c",
    "MATCH (s:Stop)-[r:LINK]->(t) RETURN count(DISTINCT t.id) AS d, count(*) AS c, count(r) AS r",
    "MATCH (s:Stop)-[:LINK]->(t) WITH s, count(t) AS c WHERE c > 0 RETURN s.id AS s, c",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, count(*) AS c ORDER BY s DESC LIMIT 2",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN min(t.id) AS lo, max(t.id) AS hi, sum(t.id) AS s, avg(t.id) AS a",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, sum(COUNT { (t)-[:LINK]->() }) AS n",
]

AT = "FOR VALID_TIME AS OF date('2008-01-01') "
