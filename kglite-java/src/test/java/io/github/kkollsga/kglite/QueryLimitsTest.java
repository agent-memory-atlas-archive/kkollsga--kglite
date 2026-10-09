package io.github.kkollsga.kglite;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.time.Duration;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/** {@link QueryOptions} and {@link CancelToken} on the session and on transactions. */
class QueryLimitsTest {

    /** Runs for far longer than any test waits unless something stops it. */
    private static final String LONG_QUERY =
            "UNWIND range(1, 100000) AS a UNWIND range(1, 100000) AS b RETURN count(*) AS n";

    @Test
    @DisplayName("rowLimit truncates and reports it in the diagnostics")
    void rowLimitTruncates() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            QueryResult result = graph.queryResult("UNWIND range(1, 50) AS i RETURN i",
                    Map.of(), QueryOptions.none().rowLimit(5));
            assertEquals(5, result.rows().size());
            assertFalse(result.warnings().isEmpty(), "the truncation must be reported");
            assertEquals(5L, ((Number) result.diagnostics().get("row_limit")).longValue());
            assertEquals(50L, ((Number) result.diagnostics().get("total_rows")).longValue());

            assertEquals(50, graph.query("UNWIND range(1, 50) AS i RETURN i",
                    Map.of(), QueryOptions.none()).size());
            assertEquals(0, graph.query("UNWIND range(1, 50) AS i RETURN i",
                    Map.of(), QueryOptions.none().rowLimit(0)).size());
        }
    }

    @Test
    @DisplayName("a write's row limit caps only the reported rows; every write still happens")
    void rowLimitOnWritesCapsReportedRowsOnly() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            assertEquals(2, graph.cypher("UNWIND range(1, 10) AS i CREATE (n:Person {id: i}) RETURN n.id",
                    Map.of(), QueryOptions.none().rowLimit(2)).size());
            assertEquals(10L, graph.query("MATCH (p:Person) RETURN count(p) AS n").get(0).get("n"));
        }
    }

    @Test
    @DisplayName("timeout and work budget flow through QueryOptions")
    void timeoutAndBudget() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            KgliteException timedOut = assertThrows(KgliteException.class, () -> graph.query(
                    LONG_QUERY, Map.of(), QueryOptions.none().timeout(Duration.ofMillis(100))));
            assertEquals("CypherTimeout", timedOut.statusName());
            assertThrows(KgliteException.class, () -> graph.query(
                    LONG_QUERY, Map.of(), QueryOptions.none().maxWorkUnits(1000)));
        }
    }

    @Test
    @DisplayName("cancelling from another thread stops a long query promptly")
    void cancelFromAnotherThread() throws Exception {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory();
                CancelToken token = new CancelToken()) {
            long start = System.nanoTime();
            CompletableFuture<Object> running = CompletableFuture.supplyAsync(() ->
                    graph.query(LONG_QUERY, Map.of(), QueryOptions.none().cancel(token)));
            Thread.sleep(300);
            assertFalse(running.isDone(), "the query must still be running when cancelled");
            token.cancel();

            ExecutionException stopped = assertThrows(ExecutionException.class,
                    () -> running.get(10, TimeUnit.SECONDS));
            QueryCancelledException cancelled = (QueryCancelledException) stopped.getCause();
            assertEquals(17, cancelled.statusCode());
            assertEquals("Cancelled", cancelled.statusName());
            assertTrue(Duration.ofNanos(System.nanoTime() - start).toSeconds() < 10);
            // The graph is still usable after a cancel.
            assertEquals(1, graph.query("RETURN 1 AS x").size());
        }
    }

    @Test
    @DisplayName("a token cancelled before the call stops it at once, and stays cancelled")
    void cancelledTokenStaysCancelled() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory();
                CancelToken token = new CancelToken()) {
            token.cancel();
            token.cancel();
            QueryOptions options = QueryOptions.none().cancel(token);
            assertThrows(QueryCancelledException.class,
                    () -> graph.query("RETURN 1 AS x", Map.of(), options));
            assertThrows(QueryCancelledException.class,
                    () -> graph.cypher("CREATE (:Person {id: 1})", Map.of(), options));
            assertEquals(0L, graph.query("MATCH (p:Person) RETURN count(p) AS n").get(0).get("n"));
        }
    }

    @Test
    @DisplayName("a cancelled transaction statement leaves the transaction open")
    void cancelInsideTransaction() throws Exception {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory();
                Tx tx = graph.begin();
                CancelToken token = new CancelToken()) {
            tx.run("CREATE (:Person {id: 1})");
            CompletableFuture<Object> running = CompletableFuture.supplyAsync(() ->
                    tx.run(LONG_QUERY, Map.of(), QueryOptions.none().cancel(token)));
            Thread.sleep(300);
            token.cancel();
            ExecutionException stopped = assertThrows(ExecutionException.class,
                    () -> running.get(10, TimeUnit.SECONDS));
            assertNotNull(stopped.getCause());
            assertTrue(stopped.getCause() instanceof QueryCancelledException);
            assertTrue(tx.isOpen());
            assertEquals(1, tx.run("MATCH (p:Person) RETURN p.id").size());
        }
    }

    @Test
    @DisplayName("a closed token cannot be cancelled")
    void closedToken() {
        CancelToken token = new CancelToken();
        token.close();
        token.close();
        assertThrows(IllegalStateException.class, token::cancel);
    }
}
