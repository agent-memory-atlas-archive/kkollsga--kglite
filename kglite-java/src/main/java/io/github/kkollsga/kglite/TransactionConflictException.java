package io.github.kkollsga.kglite;

/**
 * An explicit transaction lost its optimistic-concurrency race
 * ({@code KGLITE_STATUS_CODE_TRANSACTION_CONFLICT}, 20): another writer
 * committed between {@code begin()} and {@code commit()}, so nothing was
 * applied.
 *
 * <p>Retriable as it stands — begin a new transaction and redo the work.
 * {@link KnowledgeGraph#transaction(java.util.function.Function, int)} does
 * exactly that.
 */
public final class TransactionConflictException extends KgliteException {

    private static final long serialVersionUID = 1L;

    TransactionConflictException(int statusCode, String statusName, String message) {
        super(statusCode, statusName, message);
    }
}
