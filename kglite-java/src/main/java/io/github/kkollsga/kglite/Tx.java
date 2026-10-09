package io.github.kkollsga.kglite;

import java.lang.foreign.MemorySegment;
import java.util.List;
import java.util.Map;
import java.util.function.Consumer;

/**
 * An interactive transaction: statements run as you issue them, you read each
 * result before deciding the next statement, and nothing is visible to anyone
 * else until {@link #commit()}.
 *
 * <pre>{@code
 * try (Tx tx = graph.begin()) {
 *     long n = (Long) tx.run("MATCH (p:Person) RETURN count(p) AS n").get(0).get("n");
 *     if (n < 10) {
 *         tx.run("CREATE (:Person {id: $id})", Map.of("id", n + 1));
 *     }
 *     tx.commit();
 * }   // commit() not reached -> rolled back
 * }</pre>
 *
 * <p>The rules:
 *
 * <ul>
 *   <li><strong>Isolation.</strong> Statements see the transaction's own
 *       earlier writes and nothing other writers committed since
 *       {@link KnowledgeGraph#begin()}. Writes stay private until commit.</li>
 *   <li><strong>A failed statement is rolled back on its own</strong> and the
 *       transaction stays open; catch it and carry on, or let it propagate and
 *       the block rolls back.</li>
 *   <li><strong>Conflicts.</strong> If another writer committed since
 *       {@code begin()}, {@code commit()} applies nothing and throws
 *       {@link TransactionConflictException}. Redo the work in a new
 *       transaction, or use {@link KnowledgeGraph#transaction(java.util.function.Function,
 *       int)}, which does.</li>
 *   <li><strong>Durability.</strong> On a durable session
 *       ({@link KnowledgeGraph#open(java.nio.file.Path, OpenOptions)}) the
 *       commit is logged before it is published; a logging failure throws
 *       {@code DurabilityFailed} and applies nothing. On any other graph
 *       {@code commit()} publishes into the session only; {@code save()}
 *       persists.</li>
 *   <li><strong>Read-only transactions</strong> ({@link KnowledgeGraph#begin(boolean)})
 *       read one fixed snapshot and refuse every write with
 *       {@link ReadOnlyGraphException}.</li>
 *   <li><strong>Finished means finished.</strong> After {@code commit()},
 *       {@code rollback()} or a failed commit, {@code run} throws
 *       {@link IllegalStateException}; {@code close()} stays a no-op.</li>
 *   <li><strong>Threads.</strong> Calls on one transaction are serialized; do
 *       not rely on that for concurrency, a transaction is one thread's unit of
 *       work. Closing the graph rolls back every transaction still open on it.</li>
 * </ul>
 *
 * <p>This is a different type from the batch {@link Transaction}, which stages
 * statements and runs them at commit; that class is unchanged.
 */
public final class Tx implements AutoCloseable {

    private final boolean readOnly;
    private final Consumer<Tx> onFinish;

    /** The native transaction, or {@code null} once finished and freed. Guarded by {@code this}. */
    private MemorySegment pointer;

    Tx(MemorySegment pointer, boolean readOnly, Consumer<Tx> onFinish) {
        this.pointer = pointer;
        this.readOnly = readOnly;
        this.onFinish = onFinish;
    }

    /**
     * Whether this transaction refuses writes.
     *
     * @return {@code true} for a read-only transaction
     */
    public boolean readOnly() {
        return readOnly;
    }

    /**
     * Whether the transaction can still run statements.
     *
     * @return {@code false} once committed, rolled back, failed to commit or closed
     */
    public synchronized boolean isOpen() {
        return pointer != null;
    }

    /**
     * Run a statement and return its rows.
     *
     * @param cypher the Cypher text
     * @return the rows; they see this transaction's earlier writes
     * @throws KgliteException on an engine failure; the statement is rolled
     *     back and the transaction stays open
     * @throws ReadOnlyGraphException for a write in a read-only transaction
     * @throws IllegalStateException if the transaction is finished
     */
    public List<Map<String, Object>> run(String cypher) {
        return runResult(cypher, Map.of(), QueryOptions.none()).rows();
    }

    /**
     * Run a parameterised statement and return its rows.
     *
     * @param cypher the Cypher text, referring to bindings as {@code $name}
     * @param params the bindings; may be empty, never {@code null}
     * @return the rows
     * @throws KgliteException on an engine failure
     * @throws IllegalStateException if the transaction is finished
     */
    public List<Map<String, Object>> run(String cypher, Map<String, Object> params) {
        return runResult(cypher, params, QueryOptions.none()).rows();
    }

    /**
     * Run a parameterised statement under {@link QueryOptions}.
     *
     * @param cypher  the Cypher text
     * @param params  the bindings; may be empty, never {@code null}
     * @param options deadline, work budget, row cap and cancel token
     * @return the rows
     * @throws QueryCancelledException if the options' token fired
     * @throws KgliteException on an engine failure
     * @throws IllegalStateException if the transaction is finished
     */
    public List<Map<String, Object>> run(
            String cypher, Map<String, Object> params, QueryOptions options) {
        return runResult(cypher, params, options).rows();
    }

    /**
     * As {@link #run(String, Map, QueryOptions)}, with the engine's warnings
     * and diagnostics (including a row-limit truncation report).
     *
     * @param cypher  the Cypher text
     * @param params  the bindings; may be empty, never {@code null}
     * @param options deadline, work budget, row cap and cancel token
     * @return the rows with their warnings and diagnostics
     */
    public synchronized QueryResult runResult(
            String cypher, Map<String, Object> params, QueryOptions options) {
        if (cypher == null) {
            throw new KgliteException("a Cypher query cannot be null");
        }
        if (params == null) {
            throw new KgliteException("params cannot be null; pass Map.of() for none");
        }
        if (options == null) {
            throw new KgliteException("options cannot be null; pass QueryOptions.none()");
        }
        MemorySegment live = live("run a statement in");
        String paramsJson = params.isEmpty() ? null : Json.writeObject(params);
        return options.withCancel(cancel -> Abi.txExecute(live, cypher, paramsJson, options, cancel));
    }

    /**
     * Publish this transaction's writes atomically, then finish it.
     *
     * @throws TransactionConflictException if another writer committed since
     *     {@code begin()}; nothing was applied
     * @throws KgliteException with status {@code DurabilityFailed} if the log
     *     write failed on a durable session; nothing was applied
     * @throws IllegalStateException if the transaction is finished
     */
    public synchronized void commit() {
        MemorySegment live = live("commit");
        try {
            Abi.txCommit(live);
        } finally {
            finish();
        }
    }

    /**
     * Discard this transaction's writes, then finish it. A no-op once finished.
     */
    public synchronized void rollback() {
        if (pointer == null) {
            return;
        }
        try {
            Abi.txRollback(pointer);
        } finally {
            finish();
        }
    }

    /** Roll back unless {@link #commit()} already ran. Idempotent. */
    @Override
    public synchronized void close() {
        if (pointer != null) {
            finish();
        }
    }

    private MemorySegment live(String action) {
        if (pointer == null) {
            throw new IllegalStateException(
                    "cannot " + action + " a Tx that has already been committed, rolled back or closed");
        }
        return pointer;
    }

    /** Free the native transaction (rolling back anything still open) and deregister. */
    private void finish() {
        MemorySegment held = pointer;
        pointer = null;
        try {
            Abi.txFree(held);
        } finally {
            onFinish.accept(this);
        }
    }
}
