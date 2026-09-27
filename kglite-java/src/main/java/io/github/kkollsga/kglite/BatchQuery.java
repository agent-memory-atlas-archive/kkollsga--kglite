package io.github.kkollsga.kglite;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * One read statement of a {@link KnowledgeGraph#queryBatch(java.util.List)}
 * batch: its Cypher text and its parameters.
 *
 * @param query  the Cypher text, referring to bindings as {@code $name}
 * @param params the bindings; may be empty, never {@code null}
 */
public record BatchQuery(String query, Map<String, Object> params) {

    /**
     * Validates both components.
     *
     * @throws KgliteException if {@code query} or {@code params} is {@code null}
     */
    public BatchQuery {
        if (query == null) {
            throw new KgliteException("a Cypher query cannot be null");
        }
        if (params == null) {
            throw new KgliteException("params cannot be null; pass Map.of() for none");
        }
        // Map.copyOf refuses a null value; a null binding is a legal Cypher
        // parameter (it writes as JSON null), so copy through a map that
        // keeps it.
        params = Collections.unmodifiableMap(new LinkedHashMap<>(params));
    }

    /**
     * A statement without parameters.
     *
     * @param query the Cypher text
     * @return the statement
     */
    public static BatchQuery of(String query) {
        return new BatchQuery(query, Map.of());
    }
}
