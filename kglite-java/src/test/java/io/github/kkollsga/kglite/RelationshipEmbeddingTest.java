package io.github.kkollsga.kglite;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/** Relationship vectors use the existing parameterised Cypher session surface. */
class RelationshipEmbeddingTest {
    @Test
    void suppliedVectorsAndTypedResultsRoundTrip(@TempDir Path directory) {
        Path path = directory.resolve("relationships.kgl");
        try (WriterLease lease = WriterLease.acquire(path);
                KnowledgeGraph graph = KnowledgeGraph.open(path, StorageMode.MEMORY)) {
            assertEquals(path.toAbsolutePath(), lease.path());
            graph.cypher(
                    "CREATE (a:T {id:1})-[:EVIDENCE {text:'heat study'}]->(b:T {id:2})");
            graph.cypher(
                    "MATCH ()-[r:EVIDENCE]->() WITH collect(r) AS rs "
                            + "CALL db.edge_embeddings.set({type:'EVIDENCE',text_property:'text',"
                            + "entries:[{relationship:rs[0],vector:$v}]}) "
                            + "YIELD stored RETURN stored",
                    Map.of("v", new float[] {1.0f, 0.0f}));
            List<Map<String, Object>> rows = graph.query(
                    "MATCH ()-[r:EVIDENCE]->() RETURN r AS relationship,"
                            + "vector_score(r,'text_emb',$q) AS score",
                    Map.of("q", List.of(1.0f, 0.0f)));
            assertEquals(1.0, rows.get(0).get("score"));
            Map<?, ?> relationship = (Map<?, ?>) rows.get(0).get("relationship");
            assertEquals("EVIDENCE", relationship.get("type"));
            assertEquals("heat study", ((Map<?, ?>) relationship.get("properties")).get("text"));
            assertFalse(relationship.containsKey("incarnation"));
            assertEquals(
                    List.of(Map.of("entity", "relationship", "count", 1L)),
                    graph.query(
                            "CALL db.edge_embeddings.list({type:'EVIDENCE',text_property:'text'}) "
                                    + "YIELD entity,count RETURN entity,count"));
            graph.save(path);
        }
        try (WriterLease lease = WriterLease.acquire(path);
                KnowledgeGraph graph = KnowledgeGraph.open(path)) {
            assertEquals(path.toAbsolutePath(), lease.path());
            assertEquals(
                    1.0,
                    graph.query(
                                    "MATCH ()-[r:EVIDENCE]->() "
                                            + "RETURN vector_score(r,'text_emb',[1.0,0.0]) AS score")
                            .get(0)
                            .get("score"));
            assertEquals(
                    List.of(Map.of("removed", 1L)),
                    graph.cypher(
                            "MATCH ()-[r:EVIDENCE]->() WITH collect(r) AS relationships "
                                    + "CALL db.edge_embeddings.remove({type:'EVIDENCE',"
                                    + "text_property:'text',relationships:relationships}) "
                                    + "YIELD removed RETURN removed"));
        }
    }

    @Test
    void relationshipShapedMapIsNotAProcedureRelationship() {
        try (KnowledgeGraph graph = KnowledgeGraph.createInMemory()) {
            graph.cypher("CREATE (:T {id:1})-[:EVIDENCE]->(:T {id:2})");
            KgliteException error = assertThrows(
                    KgliteException.class,
                    () -> graph.cypher(
                            "CALL db.edge_embeddings.remove({type:'EVIDENCE',text_property:'text',"
                                    + "relationships:[{id:0,type:'EVIDENCE'}]}) "
                                    + "YIELD removed RETURN removed"));
            assertTrue(error.getMessage().contains("relationship"), error.getMessage());
        }
    }
}
