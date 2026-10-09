package io.github.kkollsga.kglite;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.Map;
import org.junit.jupiter.api.DisplayName;
import org.junit.jupiter.api.Test;

/** The structured fields of an ontology refusal survive the C ABI into Java. */
class OntologyViolationTest {

    private static final String DECLARE =
            "CALL db.ontology.declare({classes: {Person: {required_properties: ['email'],"
                    + " enforcement: 'error'}}})";

    @Test
    @DisplayName("a refused write exposes rule, entity, type and property as fields")
    void refusedWriteCarriesTheStructuredFields() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            graph.cypher(DECLARE);

            OntologyViolationException refused = assertThrows(
                    OntologyViolationException.class,
                    () -> graph.cypher("CREATE (:Person {id: 1})"));

            assertEquals(22, refused.statusCode());
            assertEquals("OntologyViolation", refused.statusName());
            assertEquals("required_property", refused.rule());
            assertEquals("node", refused.entity());
            assertEquals("Person", refused.entityType());
            assertEquals("email", refused.property());
            assertTrue(refused.report().isEmpty(), "a refused write has no declaration report");
        }
    }

    @Test
    @DisplayName("a refused declaration carries the per-rule report")
    void refusedDeclarationCarriesTheReport() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            graph.cypher("CREATE (:Person {id: 1})");

            OntologyViolationException refused = assertThrows(
                    OntologyViolationException.class, () -> graph.cypher(DECLARE));

            assertEquals("required_property", refused.rule());
            assertEquals("Person", refused.entityType());
            assertEquals("email", refused.property());
            Map<String, Object> first = refused.report().get(0);
            assertEquals("Person", first.get("entity_type"));
            assertEquals(1L, ((Number) first.get("count")).longValue());
        }
    }

    @Test
    @DisplayName("an unrelated failure is not an ontology violation and keeps no stale fields")
    void unrelatedFailuresAreNotOntologyViolations() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            graph.cypher(DECLARE);
            assertThrows(OntologyViolationException.class,
                    () -> graph.cypher("CREATE (:Person {id: 1})"));

            KgliteException syntax = assertThrows(
                    KgliteException.class, () -> graph.cypher("THIS IS NOT CYPHER"));
            assertTrue(!(syntax instanceof OntologyViolationException));
            assertNull(syntax.getCause());
        }
    }
}
