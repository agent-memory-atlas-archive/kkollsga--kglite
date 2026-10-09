package io.github.kkollsga.kglite;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

/** NaN and the infinities are bound and returned as Java doubles, never as null. */
class NonFiniteFloatTest {

    @Test
    void doublesRoundTripThroughParametersAndResults() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            for (double value : new double[] {
                Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, -0.0, 1.5
            }) {
                Object back = graph.query("RETURN $x AS x", Map.of("x", value)).get(0).get("x");
                assertEquals(Double.class, back.getClass(), "value " + value);
                assertEquals(Double.doubleToLongBits(value),
                        Double.doubleToLongBits((Double) back), "value " + value);
            }
        }
    }

    @Test
    void nonFiniteFloatsNestInListsMapsAndStoredProperties() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            Object nested = graph.query("RETURN $x AS x",
                    Map.of("x", List.of(Double.NaN, Map.of("k", Double.NEGATIVE_INFINITY))))
                    .get(0).get("x");
            assertEquals(List.of(Double.NaN, Map.of("k", Double.NEGATIVE_INFINITY)), nested);

            graph.cypher("CREATE (:N {id: 1, v: $x})", Map.of("x", Double.POSITIVE_INFINITY));
            Map<String, Object> row = graph.query("MATCH (n:N) RETURN n.v AS v, n AS n").get(0);
            assertEquals(Double.POSITIVE_INFINITY, row.get("v"));
            Object props = ((Map<?, ?>) row.get("n")).get("properties");
            assertEquals(Double.POSITIVE_INFINITY, ((Map<?, ?>) props).get("v"));
        }
    }

    @Test
    void nonFiniteFloatsRoundTripThroughTransactionBatchResults() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            List<QueryResult> results = graph.queryBatch(List.of(
                    new BatchQuery("RETURN $x AS x", Map.of("x", Double.NaN))));
            Object x = results.get(0).rows().get(0).get("x");
            assertTrue(x instanceof Double d && d.isNaN(), "batch cell: " + x);
        }
    }
}
