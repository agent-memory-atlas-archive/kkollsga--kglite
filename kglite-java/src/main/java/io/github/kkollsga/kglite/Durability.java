package io.github.kkollsga.kglite;

/**
 * How much of each commit a durable session ({@link KnowledgeGraph#open(java.nio.file.Path,
 * OpenOptions)}) guarantees on stable storage.
 */
public enum Durability {

    /** Every commit is on stable storage when the call returns (the default). */
    FULL("full"),

    /** The log is written but flushed only by {@code sync()} or a checkpoint. */
    NORMAL("normal"),

    /** No log: changes persist only through {@code checkpoint()}, {@code save} or {@code close()}. */
    OFF("off");

    private final String wire;

    Durability(String wire) {
        this.wire = wire;
    }

    /**
     * The option's wire spelling.
     *
     * @return the lowercase name the C ABI takes
     */
    public String wire() {
        return wire;
    }

    static Durability fromWire(String value) {
        for (Durability level : values()) {
            if (level.wire.equals(value)) {
                return level;
            }
        }
        throw new KgliteException("unknown kglite durability level: " + value);
    }
}
