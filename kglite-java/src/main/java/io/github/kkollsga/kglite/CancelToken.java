package io.github.kkollsga.kglite;

import java.lang.foreign.MemorySegment;
import java.util.function.Function;

/**
 * A handle that stops a running query from another thread.
 *
 * <p>Make one token per query you may want to stop, attach it with
 * {@link QueryOptions#cancel(CancelToken)}, and call {@link #cancel()} from any
 * thread. The query fails with {@link QueryCancelledException} at its next
 * check and a cancelled write publishes nothing. A token stays cancelled: a
 * later call that carries it fails at once, so a token is not reusable.
 *
 * <pre>{@code
 * try (CancelToken token = new CancelToken()) {
 *     watchdog.schedule(token::cancel, 5, TimeUnit.SECONDS);
 *     graph.query("MATCH (a)-[*]-(b) RETURN count(*)", Map.of(),
 *             QueryOptions.none().cancel(token));
 * }
 * }</pre>
 *
 * <p>{@link #cancel()} is safe from any thread, any number of times, and after
 * the query finished (then it does nothing). {@link #close()} must come after
 * the queries using the token have returned; it waits for calls that are
 * attaching it.
 */
public final class CancelToken implements AutoCloseable {

    private final NativeHandle token;

    /**
     * Create a token.
     *
     * @throws KgliteException if the engine could not allocate it
     */
    public CancelToken() {
        this.token = new NativeHandle(Abi.cancelTokenNew(), "CancelToken", Abi::cancelTokenFree);
    }

    /**
     * Ask every call carrying this token to stop.
     *
     * @throws IllegalStateException if this token is closed
     */
    public void cancel() {
        token.run(Abi::cancelTokenCancel);
    }

    /** Release the native token. Idempotent. */
    @Override
    public void close() {
        token.close();
    }

    /** Run {@code call} with the live pointer held, so it cannot be freed mid-attach. */
    <T> T attach(Function<MemorySegment, T> call) {
        return token.use(call);
    }
}
