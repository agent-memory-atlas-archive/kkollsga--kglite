package io.github.kkollsga.kglite;

import java.time.Duration;
import java.time.LocalDate;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;

/**
 * Options for the durable open, {@link KnowledgeGraph#open(java.nio.file.Path, OpenOptions)}.
 *
 * <p>Immutable; each method returns a changed copy. Every option left unset
 * takes the engine's default:
 *
 * <ul>
 *   <li>{@link #storage(StorageMode)} creates a missing path in that mode and
 *       converts an existing graph to it. Unset: an existing graph keeps the
 *       mode it recorded, a created one is in-memory.</li>
 *   <li>{@link #durability(Durability)} defaults to {@link Durability#FULL}.
 *       A disk-mode graph has no logical log: left at the default it runs at
 *       {@link Durability#OFF} and reports it in {@link OpenInfo#degradedFrom()}.</li>
 *   <li>{@link #lockTimeout(Duration)} how long to retry a contended writer
 *       lease; unset fails fast with {@link WriterLeaseHeldException}.</li>
 *   <li>{@link #validTimeDefault(String)} the instant unprefixed statements
 *       read on a graph that declares validity intervals.</li>
 *   <li>{@link #createIfMissing(boolean)} a missing path is an error unless
 *       this is {@code true}, so a typo'd path never becomes an empty
 *       database.</li>
 *   <li>{@link #autoCheckpointWalMib(long)} the log size past which a commit
 *       folds the log into the checkpoint; default 16 MiB, {@code 0} disables.</li>
 *   <li>{@link #readOnly(boolean)} takes no lease and loads the last
 *       checkpoint with nothing created, converted, logged or written. It
 *       cannot be combined with storage, createIfMissing, a lock timeout or an
 *       explicit durability.</li>
 * </ul>
 */
public final class OpenOptions {

    private static final OpenOptions DEFAULTS =
            new OpenOptions(null, null, null, null, false, false, null);

    private final StorageMode storage;
    private final Durability durability;
    private final Duration lockTimeout;
    private final String validTimeDefault;
    private final boolean createIfMissing;
    private final boolean readOnly;
    private final Long autoCheckpointWalMib;

    private OpenOptions(
            StorageMode storage, Durability durability, Duration lockTimeout,
            String validTimeDefault, boolean createIfMissing, boolean readOnly,
            Long autoCheckpointWalMib) {
        this.storage = storage;
        this.durability = durability;
        this.lockTimeout = lockTimeout;
        this.validTimeDefault = validTimeDefault;
        this.createIfMissing = createIfMissing;
        this.readOnly = readOnly;
        this.autoCheckpointWalMib = autoCheckpointWalMib;
    }

    /**
     * Every option at the engine default.
     *
     * @return the default options
     */
    public static OpenOptions defaults() {
        return DEFAULTS;
    }

    /**
     * Open (or create, or convert) in this storage mode.
     *
     * @param storage the mode
     * @return a changed copy
     */
    public OpenOptions storage(StorageMode storage) {
        return new OpenOptions(
                storage, durability, lockTimeout, validTimeDefault, createIfMissing, readOnly, autoCheckpointWalMib);
    }

    /**
     * The commit durability level.
     *
     * @param durability the level
     * @return a changed copy
     */
    public OpenOptions durability(Durability durability) {
        return new OpenOptions(
                storage, durability, lockTimeout, validTimeDefault, createIfMissing, readOnly, autoCheckpointWalMib);
    }

    /**
     * How long to retry a contended writer lease.
     *
     * @param lockTimeout the wait; zero fails fast
     * @return a changed copy
     */
    public OpenOptions lockTimeout(Duration lockTimeout) {
        return new OpenOptions(
                storage, durability, lockTimeout, validTimeDefault, createIfMissing, readOnly, autoCheckpointWalMib);
    }

    /**
     * The default read instant: {@code "today"}, {@code "all"} or a
     * {@code YYYY-MM-DD} date.
     *
     * @param validTimeDefault the instant
     * @return a changed copy
     */
    public OpenOptions validTimeDefault(String validTimeDefault) {
        return new OpenOptions(
                storage, durability, lockTimeout, validTimeDefault, createIfMissing, readOnly, autoCheckpointWalMib);
    }

    /**
     * The default read instant as a date.
     *
     * @param date the date
     * @return a changed copy
     */
    public OpenOptions validTimeDefault(LocalDate date) {
        return validTimeDefault(Objects.requireNonNull(date, "date").toString());
    }

    /**
     * Create the path when it is missing.
     *
     * @param createIfMissing whether a missing path may be created
     * @return a changed copy
     */
    public OpenOptions createIfMissing(boolean createIfMissing) {
        return new OpenOptions(
                storage, durability, lockTimeout, validTimeDefault, createIfMissing, readOnly, autoCheckpointWalMib);
    }

    /**
     * Open read-only: no lease, nothing written.
     *
     * @param readOnly whether to open read-only
     * @return a changed copy
     */
    public OpenOptions readOnly(boolean readOnly) {
        return new OpenOptions(
                storage, durability, lockTimeout, validTimeDefault, createIfMissing, readOnly, autoCheckpointWalMib);
    }

    /**
     * The write-ahead log size, in MiB, past which a durable session folds its
     * log into the checkpoint with an online checkpoint. The checkpoint runs
     * inline on the thread of the commit that crossed the bound, so that one
     * call takes the checkpoint's time while other threads keep committing.
     *
     * @param mib the bound in MiB; {@code 0} disables it (default 16)
     * @return a changed copy
     */
    public OpenOptions autoCheckpointWalMib(long mib) {
        if (mib < 0) {
            throw new IllegalArgumentException("autoCheckpointWalMib must not be negative");
        }
        return new OpenOptions(
                storage, durability, lockTimeout, validTimeDefault, createIfMissing, readOnly, mib);
    }

    /** The {@code options_json} object the C ABI takes. */
    String toJson() {
        Map<String, Object> wire = new LinkedHashMap<>();
        if (storage != null) {
            wire.put("storage", storage.wire());
        }
        if (durability != null) {
            wire.put("durability", durability.wire());
        }
        if (readOnly) {
            if (lockTimeout != null) {
                throw new KgliteException("a read-only open takes no lease, so lockTimeout is meaningless");
            }
            wire.put("lock_timeout_ms", -1L);
        } else if (lockTimeout != null) {
            wire.put("lock_timeout_ms", KnowledgeGraph.timeoutMillis(lockTimeout));
        }
        if (validTimeDefault != null) {
            wire.put("valid_time_default", validTimeDefault);
        }
        if (createIfMissing) {
            wire.put("create_if_missing", true);
        }
        if (autoCheckpointWalMib != null) {
            wire.put("auto_checkpoint_wal_mib", autoCheckpointWalMib);
        }
        return Json.writeObject(wire);
    }
}
