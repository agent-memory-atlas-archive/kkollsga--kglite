package io.github.kkollsga.kglite;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.time.Instant;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

/** {@link ValidAt}: the literal rule, and reads as of an instant through the C ABI. */
class ValidAtTest {

    @Test
    void rendersTheLiteralFromJavaTime() {
        assertEquals("date('2009-06-30')", ValidAt.of(LocalDate.of(2009, 6, 30)).literal());
        assertEquals("datetime('2009-06-30T12:00:00')",
                ValidAt.of(LocalDateTime.of(2009, 6, 30, 12, 0)).literal());
        assertEquals("datetime('2009-06-30T12:00:00.5')",
                ValidAt.of(LocalDateTime.of(2009, 6, 30, 12, 0, 0, 500_000_000)).literal());
        // Offsets and instants are UTC before they are rendered.
        assertEquals("datetime('2009-06-30T10:00:00')",
                ValidAt.of(OffsetDateTime.of(2009, 6, 30, 12, 0, 0, 0, ZoneOffset.ofHours(2)))
                        .literal());
        assertEquals("datetime('2009-06-30T00:00:00')",
                ValidAt.of(Instant.parse("2009-06-30T00:00:00Z")).literal());
        assertEquals(ValidAt.of(LocalDate.of(2009, 6, 30)), ValidAt.parse("2009-06-30"));
        assertEquals("datetime('2009-06-30T10:00:00')",
                ValidAt.parse("2009-06-30T12:00:00+02:00").literal());
        assertEquals("FOR VALID_TIME AS OF date('2009-06-30') MATCH (n) RETURN n",
                ValidAt.parse("2009-06-30").prefix("MATCH (n) RETURN n"));
    }

    @Test
    void refusesWhatALiteralCannotSpell() {
        assertThrows(IllegalArgumentException.class, () -> ValidAt.of(LocalDate.of(10000, 1, 1)));
        assertThrows(IllegalArgumentException.class, () -> ValidAt.of(LocalDate.of(-5, 1, 1)));
        assertThrows(IllegalArgumentException.class, () -> ValidAt.parse("last tuesday"));
        assertThrows(IllegalArgumentException.class, () -> ValidAt.parse("2009-06-30'); MATCH"));
        assertThrows(IllegalArgumentException.class, () -> ValidAt.of((LocalDate) null));
    }

    private static KnowledgeGraph wells() {
        KnowledgeGraph graph = KnowledgeGraph.createInMemory();
        graph.cypher("CREATE (:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}),"
                + " (:Well {id: 2, vf: date('2005-01-01')})");
        graph.cypher("CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt',"
                + " convention: 'closed'}) YIELD declared RETURN declared");
        return graph;
    }

    private static List<Object> ids(List<Map<String, Object>> rows) {
        return rows.stream().map(row -> row.get("id")).toList();
    }

    @Test
    @SuppressWarnings("unchecked")
    void readsAsOfTheInstantAndEchoesIt() {
        try (KnowledgeGraph graph = wells()) {
            String query = "MATCH (w:Well) RETURN w.id AS id ORDER BY id";
            ValidAt at2003 = ValidAt.of(LocalDate.of(2003, 6, 30));
            assertEquals(List.of(1L, 2L), ids(graph.query(query)));
            assertEquals(List.of(1L), ids(graph.query(query, Map.of(), at2003)),
                    "the same rows as the typed prefix");
            assertEquals(ids(graph.query(at2003.prefix(query))),
                    ids(graph.query(query, Map.of(), at2003)));

            QueryResult result = graph.queryResult(query, Map.of(), at2003);
            Map<String, Object> echo = (Map<String, Object>) result.diagnostics().get("temporal");
            assertEquals("VALID_TIME", echo.get("axis"));
            assertEquals("2003-06-30", echo.get("instant"));
            assertEquals(List.of("(:Well)"), echo.get("targets"));
            assertEquals("guarded", echo.get("route"));
            assertEquals(Boolean.FALSE, echo.get("slice"));
            assertFalse(graph.queryResult(query, Map.of()).diagnostics().containsKey("temporal"));

            // EXPLAIN may follow the prefix.
            List<Map<String, Object>> plan = graph.query("EXPLAIN " + query, Map.of(), at2003);
            assertTrue(String.valueOf(plan.get(0).get("operation"))
                    .startsWith("ValidTimeContext axis=VALID_TIME"), plan.toString());

            // A second context is the parser's error.
            KgliteException doubled = assertThrows(KgliteException.class, () -> graph.query(
                    "FOR VALID_TIME AS OF date('2001-01-01') " + query, Map.of(), at2003));
            assertTrue(doubled.getMessage().contains("this one has two"), doubled.getMessage());
        }
    }

    @Test
    void aBatchReadsOneSnapshotAsOfOneInstant() {
        try (KnowledgeGraph graph = wells()) {
            List<BatchQuery> report = List.of(
                    BatchQuery.of("MATCH (w:Well) RETURN count(w) AS n"),
                    new BatchQuery("MATCH (w:Well {id: $id}) RETURN w.id AS id", Map.of("id", 2)));
            List<QueryResult> now = graph.queryBatch(report);
            assertEquals(2L, now.get(0).rows().get(0).get("n"));
            assertEquals(List.of(2L), ids(now.get(1).rows()));

            List<QueryResult> then = graph.queryBatch(report, ValidAt.parse("2003-06-30"));
            assertEquals(1L, then.get(0).rows().get(0).get("n"));
            assertEquals(List.of(), ids(then.get(1).rows()));
            assertEquals(List.of(), graph.queryBatch(List.of()));
            assertThrows(IllegalArgumentException.class, () -> graph.queryBatch(List.of(), null));
            // A null binding is a legal parameter on every query method.
            java.util.Map<String, Object> nullBound = new java.util.HashMap<>();
            nullBound.put("x", null);
            List<QueryResult> bound = graph.queryBatch(List.of(new BatchQuery("RETURN $x AS x", nullBound)));
            assertEquals(1, bound.get(0).rows().size());
            assertTrue(bound.get(0).rows().get(0).containsKey("x"));
            assertEquals(null, bound.get(0).rows().get(0).get("x"));
            assertThrows(KgliteException.class,
                    () -> graph.queryBatch(List.of(BatchQuery.of("CREATE (:Well {id: 3})"))));
        }
    }
}
