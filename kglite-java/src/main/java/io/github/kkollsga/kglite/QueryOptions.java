package io.github.kkollsga.kglite;

import java.lang.foreign.MemorySegment;
import java.time.Duration;
import java.util.function.Function;

/**
 * Per-query limits: a deadline, a work budget, a result-row cap and a
 * {@link CancelToken}.
 *
 * <p>Immutable; each method returns a changed copy, so a shared base can be
 * specialised per call:
 *
 * <pre>{@code
 * QueryOptions bounded = QueryOptions.none().timeout(Duration.ofSeconds(5)).rowLimit(1000);
 * graph.query("MATCH (n) RETURN n", Map.of(), bounded);
 * }</pre>
 *
 * <ul>
 *   <li>{@link #timeout(Duration)} past the deadline the query fails with
 *       {@link KgliteException} status {@code CypherTimeout}.</li>
 *   <li>{@link #maxWorkUnits(long)} is a work budget, not a row cap: exceeding
 *       it fails the query.</li>
 *   <li>{@link #rowLimit(long)} <em>truncates</em>: the query still runs to
 *       completion and the rows kept stop at the cap. The truncation is
 *       reported in the {@link QueryResult#diagnostics()} ({@code row_limit},
 *       {@code total_rows}) and {@link QueryResult#warnings()}. On a write only
 *       the rows the trailing {@code RETURN} reports are capped; every write
 *       still happens.</li>
 *   <li>{@link #cancel(CancelToken)} makes the query stoppable from another
 *       thread.</li>
 * </ul>
 */
public final class QueryOptions {

    private static final QueryOptions NONE = new QueryOptions(null, 0L, -1L, null);

    private final Duration timeout;
    private final long maxWorkUnits;
    private final long rowLimit;
    private final CancelToken cancel;

    private QueryOptions(Duration timeout, long maxWorkUnits, long rowLimit, CancelToken cancel) {
        this.timeout = timeout;
        this.maxWorkUnits = maxWorkUnits;
        this.rowLimit = rowLimit;
        this.cancel = cancel;
    }

    /**
     * No limits.
     *
     * @return the empty options
     */
    public static QueryOptions none() {
        return NONE;
    }

    /**
     * A wall-clock budget.
     *
     * @param timeout the budget; {@code null}, zero or negative means none
     * @return a copy with the deadline set
     */
    public QueryOptions timeout(Duration timeout) {
        return new QueryOptions(timeout, maxWorkUnits, rowLimit, cancel);
    }

    /**
     * A work budget.
     *
     * @param maxWorkUnits the work units the query may charge; zero or negative means none
     * @return a copy with the budget set
     */
    public QueryOptions maxWorkUnits(long maxWorkUnits) {
        return new QueryOptions(timeout, Math.max(0L, maxWorkUnits), rowLimit, cancel);
    }

    /**
     * A result-row cap that truncates with a report rather than failing.
     *
     * @param rowLimit the most rows to keep; {@code 0} keeps none and still
     *     reports the total
     * @return a copy with the cap set
     * @throws IllegalArgumentException if {@code rowLimit} is negative
     */
    public QueryOptions rowLimit(long rowLimit) {
        if (rowLimit < 0) {
            throw new IllegalArgumentException("rowLimit cannot be negative: " + rowLimit);
        }
        return new QueryOptions(timeout, maxWorkUnits, rowLimit, cancel);
    }

    /**
     * A token that stops the query from another thread.
     *
     * @param cancel the token, or {@code null} for none
     * @return a copy carrying the token
     */
    public QueryOptions cancel(CancelToken cancel) {
        return new QueryOptions(timeout, maxWorkUnits, rowLimit, cancel);
    }

    long timeoutMillis() {
        return KnowledgeGraph.timeoutMillis(timeout);
    }

    long maxWorkUnits() {
        return maxWorkUnits;
    }

    boolean hasRowLimit() {
        return rowLimit >= 0;
    }

    long rowLimit() {
        return rowLimit;
    }

    /** Run {@code call} with the cancel token's pointer (or NULL) kept alive across it. */
    <T> T withCancel(Function<MemorySegment, T> call) {
        return cancel == null ? call.apply(MemorySegment.NULL) : cancel.attach(call);
    }
}
