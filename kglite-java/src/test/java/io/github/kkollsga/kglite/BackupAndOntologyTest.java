package io.github.kkollsga.kglite;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/** {@code backup()} and the ontology declaration surface on a session. */
class BackupAndOntologyTest {

    private static final Map<String, Object> PERSON_NEEDS_EMAIL = Map.of(
            "classes", Map.of("Person", Map.of(
                    "required_properties", List.of("email"), "enforcement", "error")));

    @Test
    @DisplayName("backup of a live durable graph round-trips and leaves the original alone")
    void backupRoundTrip(@TempDir Path dir) {
        Path live = dir.resolve("live.kgl");
        Path copy = dir.resolve("copy.kgl");
        try (KnowledgeGraph graph = KnowledgeGraph.open(
                live, OpenOptions.defaults().createIfMissing(true))) {
            graph.cypher("CREATE (:Person {id: 1, title: 'Ada'})-[:KNOWS]->(:Person {id: 2})");
            BackupReport report = graph.backup(copy);
            assertEquals(2, report.nodes());
            assertEquals(1, report.relationships());
            assertTrue(report.bytes() > 0);
            assertTrue(report.lsn().isPresent(), "a durable session reports its log position");
            assertTrue(Files.exists(copy));

            // Writers keep going after the backup, and the backup does not see them.
            graph.cypher("CREATE (:Person {id: 3})");
        }
        try (KnowledgeGraph restored = KnowledgeGraph.open(copy)) {
            assertEquals(2L, restored.query("MATCH (p:Person) RETURN count(p) AS n").get(0).get("n"));
            assertEquals("Ada",
                    restored.query("MATCH (p:Person {id: 1}) RETURN p.title AS t").get(0).get("t"));
        }
    }

    @Test
    @DisplayName("a backup over the live file is refused and the live file survives")
    void backupOverLiveFileIsRefused(@TempDir Path dir) {
        Path live = dir.resolve("live.kgl");
        try (KnowledgeGraph graph = KnowledgeGraph.open(
                live, OpenOptions.defaults().createIfMissing(true))) {
            graph.cypher("CREATE (:Person {id: 1})");
            graph.checkpoint();
            KgliteException refused = assertThrows(KgliteException.class, () -> graph.backup(live));
            assertEquals("FileIo", refused.statusName());
        }
        try (KnowledgeGraph again = KnowledgeGraph.open(live, OpenOptions.defaults())) {
            assertEquals(1L, again.query("MATCH (p:Person) RETURN count(p) AS n").get(0).get("n"));
        }
    }

    @Test
    @DisplayName("backup works on an in-memory graph (no log position)")
    void backupInMemory(@TempDir Path dir) {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            graph.cypher("CREATE (:Person {id: 1})");
            BackupReport report = graph.backup(dir.resolve("mem.kgl"));
            assertEquals(1, report.nodes());
            assertFalse(report.lsn().isPresent());
        }
    }

    @Test
    @DisplayName("declare then refuse then show then clear")
    void ontologyLifecycle() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            assertFalse(graph.ontology().isPresent());
            List<String> warnings = graph.declareOntology(PERSON_NEEDS_EMAIL);
            // No Person nodes exist yet, which the engine flags as a warn-level finding.
            assertEquals(1, warnings.size(), "warnings: " + warnings);
            assertTrue(warnings.get(0).contains("Person"));
            assertTrue(graph.ontology().isPresent());

            OntologyViolationException refused = assertThrows(OntologyViolationException.class,
                    () -> graph.cypher("CREATE (:Person {id: 1})"));
            assertEquals("required_property", refused.rule());
            assertEquals("node", refused.entity());
            assertEquals("Person", refused.entityType());
            assertEquals("email", refused.property());

            graph.cypher("CREATE (:Person {id: 2, email: 'a@b'})");
            graph.clearOntology();
            assertFalse(graph.ontology().isPresent());
            graph.cypher("CREATE (:Person {id: 3})");
            graph.clearOntology();
        }
    }

    @Test
    @DisplayName("declaring over data that breaks an error rule is refused with the report as fields")
    void declarationRefusalCarriesReport() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            graph.cypher("CREATE (:Person {id: 1})");
            OntologyViolationException refused = assertThrows(OntologyViolationException.class,
                    () -> graph.declareOntology(PERSON_NEEDS_EMAIL));
            assertEquals("required_property", refused.rule());
            assertEquals("Person", refused.entityType());
            assertEquals("email", refused.property());
            assertEquals(1L, ((Number) refused.report().get(0).get("count")).longValue());
            assertFalse(graph.ontology().isPresent(), "a refused declaration changes nothing");
        }
    }

    @Test
    @DisplayName("a malformed declaration is InvalidArgument, not an ontology violation")
    void malformedDeclaration() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            KgliteException bad = assertThrows(KgliteException.class,
                    () -> graph.declareOntology("{not json"));
            assertEquals("InvalidArgument", bad.statusName());
            assertFalse(bad instanceof OntologyViolationException);
        }
    }

    @Test
    @DisplayName("a durable declaration survives a close and reopen")
    void durableDeclaration(@TempDir Path dir) {
        Path path = dir.resolve("g.kgl");
        try (KnowledgeGraph graph = KnowledgeGraph.open(
                path, OpenOptions.defaults().createIfMissing(true))) {
            graph.declareOntology(PERSON_NEEDS_EMAIL);
        }
        try (KnowledgeGraph again = KnowledgeGraph.open(path, OpenOptions.defaults())) {
            assertTrue(again.ontology().isPresent());
            assertThrows(OntologyViolationException.class,
                    () -> again.cypher("CREATE (:Person {id: 1})"));
        }
    }

    @Test
    @DisplayName("a read-only graph refuses to declare or clear")
    void readOnlyRefusesOntologyWrites(@TempDir Path dir) {
        Path path = dir.resolve("g.kgl");
        try (KnowledgeGraph graph = KnowledgeGraph.open(
                path, OpenOptions.defaults().createIfMissing(true))) {
            graph.cypher("CREATE (:Person {id: 1})");
        }
        try (KnowledgeGraph reader = KnowledgeGraph.open(path, OpenOptions.defaults().readOnly(true))) {
            assertThrows(ReadOnlyGraphException.class, () -> reader.declareOntology(PERSON_NEEDS_EMAIL));
            assertThrows(ReadOnlyGraphException.class, reader::clearOntology);
        }
    }

    @Test
    @DisplayName("typed values still decode on a durable session")
    void typedValuesDecode(@TempDir Path dir) {
        try (KnowledgeGraph graph = KnowledgeGraph.open(
                dir.resolve("g.kgl"), OpenOptions.defaults().createIfMissing(true))) {
            Map<String, Object> row = graph.query(
                    "RETURN date('2026-10-09') AS d, 1.5 AS f").get(0);
            assertEquals(java.time.LocalDate.of(2026, 10, 9), row.get("d"));
            assertEquals(1.5, row.get("f"));
            try (Tx tx = graph.begin()) {
                assertEquals(java.time.LocalDate.of(2026, 10, 9),
                        tx.run("RETURN date('2026-10-09') AS d").get(0).get("d"));
            }
        }
    }
}
