package io.github.kkollsga.kglite;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/**
 * The wrapper-enforced read-only guard of
 * {@link KnowledgeGraph#openReadOnly(Path)}.
 *
 * <p>The guard is convention-level — it lives on the wrapper, not the engine —
 * so these tests assert exactly that scope: reads pass, writes through the same
 * handle are refused before native code, and a normal {@code open()} is
 * untouched.
 */
class ReadOnlyGraphTest {

    /** Write a small graph to {@code path} and return it. */
    private static Path seed(Path dir) {
        Path path = dir.resolve("people.kgl");
        try (WriterLease lease = WriterLease.acquire(path);
                KnowledgeGraph graph = KnowledgeGraph.open(path, StorageMode.MEMORY)) {
            assertEquals(path.toAbsolutePath(), lease.path());
            graph.cypher("CREATE (:Person {id: 1, title: 'Ada'})");
            graph.save(path);
        }
        return path;
    }

    @Test
    @DisplayName("openReadOnly permits query()")
    void readOnlyReads(@TempDir Path dir) {
        Path path = seed(dir);
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path)) {
            List<Map<String, Object>> rows =
                    graph.query("MATCH (p:Person) RETURN p.title AS name");
            assertEquals(1, rows.size());
            assertEquals("Ada", rows.get(0).get("name"));
        }
    }

    @Test
    @DisplayName("openReadOnly refuses cypher() with ReadOnlyGraphException")
    void readOnlyRefusesCypher(@TempDir Path dir) {
        Path path = seed(dir);
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path)) {
            ReadOnlyGraphException e = assertThrows(ReadOnlyGraphException.class,
                    () -> graph.cypher("CREATE (:Person {id: 2, title: 'Grace'})"));
            // Raised by the wrapper, but under the engine's own ReadOnly identity.
            assertEquals("ReadOnly", e.statusName());
            assertEquals(24, e.statusCode());
            // The refusal is before native code: the graph is untouched and still reads.
            assertEquals(1, graph.query("MATCH (p:Person) RETURN p.id").size());
        }
    }

    @Test
    @DisplayName("openReadOnly refuses the timeout/maxWorkUnits write overloads too")
    void readOnlyRefusesWriteOverloads(@TempDir Path dir) {
        Path path = seed(dir);
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path)) {
            assertThrows(ReadOnlyGraphException.class,
                    () -> graph.cypher("CREATE (:X {id: 1})", java.time.Duration.ofSeconds(5)));
            assertThrows(ReadOnlyGraphException.class,
                    () -> graph.cypher("CREATE (:X {id: 1})", Map.of(), null, 0));
        }
    }

    @Test
    @DisplayName("openReadOnly refuses beginTransaction()")
    void readOnlyRefusesTransaction(@TempDir Path dir) {
        Path path = seed(dir);
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path)) {
            assertThrows(ReadOnlyGraphException.class, graph::beginTransaction);
        }
    }

    @Test
    @DisplayName("a normally-opened graph is unaffected by the guard")
    void normalOpenStillWrites(@TempDir Path dir) {
        Path path = seed(dir);
        try (WriterLease lease = WriterLease.acquire(path);
                KnowledgeGraph graph = KnowledgeGraph.open(path)) {
            assertEquals(path.toAbsolutePath(), lease.path());
            graph.cypher("CREATE (:Person {id: 2, title: 'Grace'})");
            assertEquals(2, graph.query("MATCH (p:Person) RETURN p.id").size());
        }
    }

    @Test
    @DisplayName("ReadOnlyGraphException is a KgliteException, so a broad catch still works")
    void isKgliteException(@TempDir Path dir) {
        Path path = seed(dir);
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path)) {
            boolean caught = false;
            try {
                graph.cypher("CREATE (:Nope {id: 1})");
            } catch (KgliteException e) {
                caught = true;
                assertTrue(e instanceof ReadOnlyGraphException, "the precise subclass is thrown");
            }
            assertTrue(caught, "a mutation on a read-only handle threw");
        }
    }

    /** SHA-256 over every file's relative path and bytes under {@code root} (or the file itself). */
    private static String fingerprint(Path root) throws java.io.IOException {
        try (java.util.stream.Stream<Path> walk = java.nio.file.Files.walk(root)) {
            java.security.MessageDigest md = java.security.MessageDigest.getInstance("SHA-256");
            for (Path p : walk.sorted().toList()) {
                md.update(root.relativize(p).toString().getBytes(java.nio.charset.StandardCharsets.UTF_8));
                if (java.nio.file.Files.isRegularFile(p)) {
                    md.update(java.nio.file.Files.readAllBytes(p));
                }
            }
            return java.util.HexFormat.of().formatHex(md.digest());
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
    }

    @Test
    @DisplayName("openReadOnly with a mode never creates a missing path")
    void readOnlyNeverCreates(@TempDir Path dir) {
        for (StorageMode mode : StorageMode.values()) {
            Path missing = dir.resolve("typo-" + mode + ".kgl");
            assertThrows(KgliteException.class, () -> KnowledgeGraph.openReadOnly(missing, mode));
            assertTrue(!java.nio.file.Files.exists(missing), mode + " read-only open created a path");
        }
        Path missing = dir.resolve("typo-null.kgl");
        assertThrows(KgliteException.class, () -> KnowledgeGraph.openReadOnly(missing));
        assertTrue(!java.nio.file.Files.exists(missing));
    }

    @Test
    @DisplayName("openReadOnly never converts a graph's storage mode and leaves its bytes alone")
    void readOnlyNeverConverts(@TempDir Path dir) throws Exception {
        Path path = seed(dir);
        String before = fingerprint(path);
        for (StorageMode mode : new StorageMode[] {StorageMode.MAPPED, StorageMode.DISK}) {
            assertThrows(KgliteException.class, () -> KnowledgeGraph.openReadOnly(path, mode));
        }
        // The matching mode, and no mode, open as-is.
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path, StorageMode.MEMORY)) {
            assertTrue(graph.convertedFrom().isEmpty());
            assertEquals(1, graph.query("MATCH (p:Person) RETURN p.id").size());
        }
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path)) {
            assertEquals(1, graph.query("MATCH (p:Person) RETURN p.id").size());
        }
        assertEquals(before, fingerprint(path));
    }

    @Test
    @DisplayName("openReadOnly on a disk graph writes nothing")
    void readOnlyDiskWritesNothing(@TempDir Path dir) throws Exception {
        Path path = dir.resolve("disk-graph");
        try (WriterLease lease = WriterLease.acquire(path);
                KnowledgeGraph graph = KnowledgeGraph.open(path, StorageMode.DISK)) {
            assertEquals(path.toAbsolutePath(), lease.path());
            graph.cypher("CREATE (:Person {id: 1, title: 'Ada'})");
            graph.save(path);
        }
        String before = fingerprint(path);
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path, StorageMode.DISK)) {
            assertEquals(1, graph.query("MATCH (p:Person) RETURN p.id").size());
        }
        try (KnowledgeGraph graph = KnowledgeGraph.openReadOnly(path)) {
            assertEquals(1, graph.query("MATCH (p:Person) RETURN p.id").size());
        }
        assertThrows(KgliteException.class, () -> KnowledgeGraph.openReadOnly(path, StorageMode.MEMORY));
        assertEquals(before, fingerprint(path));
    }
}
