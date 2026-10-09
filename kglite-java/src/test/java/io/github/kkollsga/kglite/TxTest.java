package io.github.kkollsga.kglite;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.Map;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/** Interactive transactions: {@code graph.begin()} and {@link Tx}. */
class TxTest {

    private static long count(KnowledgeGraph graph) {
        return ((Number) graph.query("MATCH (p:Person) RETURN count(p) AS n").get(0).get("n"))
                .longValue();
    }

    @Test
    @DisplayName("commit publishes; intermediate results are readable and branchable")
    void commitPublishes() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory();
                Tx tx = graph.begin()) {
            tx.run("CREATE (:Person {id: 1, title: 'Ada'})");
            // Read-your-writes inside the transaction, invisible outside it.
            long inside = ((Number) tx.run("MATCH (p:Person) RETURN count(p) AS n")
                    .get(0).get("n")).longValue();
            assertEquals(1, inside);
            assertEquals(0, count(graph));
            if (inside < 2) {
                tx.run("CREATE (:Person {id: $id})", Map.of("id", 2));
            }
            tx.commit();
            assertFalse(tx.isOpen());
            assertEquals(2, count(graph));
        }
    }

    @Test
    @DisplayName("rollback discards the writes and finishes the transaction")
    void rollbackDiscards() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            Tx tx = graph.begin();
            tx.run("CREATE (:Person {id: 1})");
            tx.rollback();
            assertEquals(0, count(graph));
            assertThrows(IllegalStateException.class, () -> tx.run("RETURN 1"));
            tx.close();
            tx.rollback();
        }
    }

    @Test
    @DisplayName("leaving the block without commit rolls back (auto-rollback)")
    void closeWithoutCommitRollsBack() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            try (Tx tx = graph.begin()) {
                tx.run("CREATE (:Person {id: 1})");
            }
            assertEquals(0, count(graph));

            assertThrows(IllegalArgumentException.class, () -> {
                try (Tx tx = graph.begin()) {
                    tx.run("CREATE (:Person {id: 2})");
                    throw new IllegalArgumentException("boom");
                }
            });
            assertEquals(0, count(graph));
        }
    }

    @Test
    @DisplayName("a failed statement leaves the transaction open and its own writes undone")
    void failedStatementKeepsTransactionOpen() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory();
                Tx tx = graph.begin()) {
            tx.run("CREATE (:Person {id: 1})");
            assertThrows(KgliteException.class, () -> tx.run("THIS IS NOT CYPHER"));
            assertTrue(tx.isOpen());
            tx.commit();
            assertEquals(1, count(graph));
        }
    }

    @Test
    @DisplayName("a read-only transaction reads a fixed snapshot and refuses writes")
    void readOnlyTransaction() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            graph.cypher("CREATE (:Person {id: 1})");
            try (Tx tx = graph.begin(true)) {
                assertTrue(tx.readOnly());
                graph.cypher("CREATE (:Person {id: 2})");
                long seen = ((Number) tx.run("MATCH (p:Person) RETURN count(p) AS n")
                        .get(0).get("n")).longValue();
                assertEquals(1, seen, "the snapshot predates the concurrent commit");
                assertThrows(ReadOnlyGraphException.class, () -> tx.run("CREATE (:Person {id: 3})"));
                tx.commit();
            }
            assertEquals(2, count(graph));
        }
    }

    @Test
    @DisplayName("a read-write transaction on a read-only graph is refused")
    void readOnlyGraphRefusesWriteTransaction(@TempDir Path dir) {
        Path path = dir.resolve("g.kgl");
        try (KnowledgeGraph graph = KnowledgeGraph.open(
                path, OpenOptions.defaults().createIfMissing(true))) {
            graph.cypher("CREATE (:Person {id: 1})");
        }
        try (KnowledgeGraph reader = KnowledgeGraph.open(path, OpenOptions.defaults().readOnly(true))) {
            assertThrows(ReadOnlyGraphException.class, () -> reader.begin());
            try (Tx tx = reader.begin(true)) {
                assertEquals(1, tx.run("MATCH (p:Person) RETURN p.id").size());
            }
        }
    }

    @Test
    @DisplayName("a lost race is a TransactionConflictException and applies nothing")
    void conflictIsTyped() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            Tx tx = graph.begin();
            tx.run("CREATE (:Person {id: 1})");
            graph.cypher("CREATE (:Person {id: 2})");
            TransactionConflictException lost =
                    assertThrows(TransactionConflictException.class, tx::commit);
            assertEquals(20, lost.statusCode());
            assertFalse(tx.isOpen());
            assertEquals(1, count(graph));
        }
    }

    @Test
    @DisplayName("transaction(fn) retries a conflicted commit and returns the winning attempt")
    void transactionRetries() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            AtomicInteger attempts = new AtomicInteger();
            long result = graph.transaction(tx -> {
                tx.run("CREATE (:Person {id: 1})");
                if (attempts.incrementAndGet() == 1) {
                    graph.cypher("CREATE (:Person {id: 2})"); // a competing writer
                }
                return ((Number) tx.run("MATCH (p:Person) RETURN count(p) AS n")
                        .get(0).get("n")).longValue();
            }, 2);
            assertEquals(2, attempts.get());
            assertEquals(2, result);
            assertEquals(2, count(graph));
        }
    }

    @Test
    @DisplayName("transaction(fn) gives up after the retries and rethrows the conflict")
    void transactionGivesUp() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            AtomicInteger attempts = new AtomicInteger();
            assertThrows(TransactionConflictException.class, () -> graph.transaction(tx -> {
                attempts.incrementAndGet();
                tx.run("CREATE (:Person {id: 1})");
                graph.cypher("CREATE (:Other {id: 1})");
                return null;
            }, 1));
            assertEquals(2, attempts.get());
            assertEquals(0, count(graph));
        }
    }

    @Test
    @DisplayName("transaction(fn) rolls back and propagates any other exception without retry")
    void transactionPropagatesOtherFailures() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            AtomicInteger attempts = new AtomicInteger();
            assertThrows(KgliteException.class, () -> graph.transaction(tx -> {
                attempts.incrementAndGet();
                tx.run("CREATE (:Person {id: 1})");
                return tx.run("THIS IS NOT CYPHER");
            }, 5));
            assertEquals(1, attempts.get());
            assertEquals(0, count(graph));
        }
    }

    @Test
    @DisplayName("closing the graph rolls back transactions still open")
    void graphCloseRollsBackOpenTransactions(@TempDir Path dir) {
        Path path = dir.resolve("g.kgl");
        KnowledgeGraph graph = KnowledgeGraph.open(
                path, OpenOptions.defaults().createIfMissing(true));
        Tx tx = graph.begin();
        tx.run("CREATE (:Person {id: 1})");
        graph.close();
        assertFalse(tx.isOpen());
        assertThrows(IllegalStateException.class, () -> tx.run("RETURN 1"));
        try (KnowledgeGraph again = KnowledgeGraph.open(path, OpenOptions.defaults())) {
            assertEquals(0, count(again));
        }
    }

    @Test
    @DisplayName("a durable commit survives a close and reopen; an unsaved memory commit does not need to")
    void durableCommitPersists(@TempDir Path dir) {
        Path path = dir.resolve("g.kgl");
        try (KnowledgeGraph graph = KnowledgeGraph.open(
                path, OpenOptions.defaults().createIfMissing(true))) {
            try (Tx tx = graph.begin()) {
                tx.run("CREATE (:Person {id: 1, title: 'Ada'})");
                tx.commit();
            }
        }
        try (KnowledgeGraph again = KnowledgeGraph.open(path, OpenOptions.defaults())) {
            assertEquals(1, count(again));
        }
    }
}
