package io.github.kkollsga.kglite;

/**
 * The outcome of {@link KnowledgeGraph#checkpoint()}.
 *
 * @param written {@code true} when a file was written, {@code false} when the
 *     graph was unchanged since this handle's last checkpoint
 * @param version the graph version that was checkpointed
 */
public record Checkpoint(boolean written, long version) {}
