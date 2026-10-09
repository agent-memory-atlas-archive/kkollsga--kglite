package io.github.kkollsga.kglite;

/**
 * A query stopped because its {@link CancelToken} was cancelled
 * ({@code KGLITE_STATUS_CODE_CANCELLED}, 17).
 *
 * <p>Its own type because cancellation is the caller's decision, not a fault:
 * catch it where you cancel. A cancelled write publishes nothing, and a
 * transaction statement that was cancelled is rolled back on its own while the
 * transaction stays open.
 */
public final class QueryCancelledException extends KgliteException {

    private static final long serialVersionUID = 1L;

    QueryCancelledException(int statusCode, String statusName, String message) {
        super(statusCode, statusName, message);
    }
}
