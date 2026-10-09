package io.github.kkollsga.kglite;

import java.nio.file.Path;

/**
 * Entry point of the child JVM {@link DurableSessionTest} kills: opens a
 * durable session, commits one node, reports readiness, then waits to be
 * SIGKILLed without ever closing, checkpointing or saving.
 */
public final class DurableChild {

    private DurableChild() {}

    /**
     * @param args {@code path durability}
     * @throws Exception on any failure
     */
    public static void main(String[] args) throws Exception {
        Durability level = Durability.valueOf(args[1]);
        KnowledgeGraph graph = KnowledgeGraph.open(Path.of(args[0]),
                OpenOptions.defaults().createIfMissing(true).durability(level));
        graph.cypher("CREATE (:Person {id: 1, title: 'Ada'})");
        if (level == Durability.NORMAL) {
            graph.sync();
        }
        System.out.println("READY");
        System.out.flush();
        Thread.sleep(600_000);
    }
}
