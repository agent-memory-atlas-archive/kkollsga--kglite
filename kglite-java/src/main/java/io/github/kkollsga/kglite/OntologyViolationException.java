package io.github.kkollsga.kglite;

import java.util.List;
import java.util.Map;

/**
 * A write, or an ontology declaration, the declared ontology refused
 * ({@code KGLITE_STATUS_CODE_ONTOLOGY_VIOLATION}, 22). The graph is unchanged.
 *
 * <p>Its own type so a caller can branch on <em>which rule</em> refused what
 * without parsing the message: the fields below are read from the engine's
 * structured error detail ({@code kglite_last_error_details_json}), the same
 * facts the Python and Node surfaces attach to their errors.
 */
public final class OntologyViolationException extends KgliteException {

    private static final long serialVersionUID = 1L;

    /** The refusing rule, or {@code null}. */
    private final String rule;

    /** {@code node} or {@code relationship}, or {@code null}. */
    private final String entity;

    /** The label or relationship type refused, or {@code null}. */
    private final String entityType;

    /** The offending property, or {@code null}. */
    private final String property;

    /** The declaration report rows; empty for a refused write. */
    private final transient List<Map<String, Object>> report;

    OntologyViolationException(int statusCode, String statusName, String message, String detailJson) {
        super(statusCode, statusName, message);
        Map<?, ?> fields = parse(detailJson);
        this.rule = fields.get("rule") instanceof String value ? value : null;
        this.entity = fields.get("entity") instanceof String value ? value : null;
        this.entityType = fields.get("entity_type") instanceof String value ? value : null;
        this.property = fields.get("property") instanceof String value ? value : null;
        this.report = reportOf(fields.get("report"));
    }

    /** Degrade to no fields rather than hide the refusal behind a parse failure. */
    private static Map<?, ?> parse(String detailJson) {
        if (detailJson == null || detailJson.isBlank()) {
            return Map.of();
        }
        try {
            return Json.parse(detailJson) instanceof Map<?, ?> object ? object : Map.of();
        } catch (KgliteException malformed) {
            return Map.of();
        }
    }

    private static List<Map<String, Object>> reportOf(Object raw) {
        if (!(raw instanceof List<?> rows)) {
            return List.of();
        }
        return rows.stream()
                .filter(Map.class::isInstance)
                .map(row -> {
                    @SuppressWarnings("unchecked")
                    Map<String, Object> typed = (Map<String, Object>) row;
                    return typed;
                })
                .toList();
    }

    /**
     * The refusing rule: {@code required_property}, {@code property_type},
     * {@code closed_labels}, {@code domain}, {@code range}, {@code cardinality}, {@code required_relationship},
     * {@code min_cardinality}, {@code inverse}, {@code symmetric} or {@code transitive}.
     *
     * @return the rule, or {@code null} if the engine supplied no detail
     */
    public String rule() {
        return rule;
    }

    /**
     * Whether the refused entity is a {@code node} or a {@code relationship}.
     *
     * @return the entity kind, or {@code null} if the engine supplied no detail
     */
    public String entity() {
        return entity;
    }

    /**
     * The label or relationship type the rule refused.
     *
     * @return the type name, or {@code null} if the engine supplied no detail
     */
    public String entityType() {
        return entityType;
    }

    /**
     * The offending property when the rule is a property rule.
     *
     * @return the property name, or {@code null}
     */
    public String property() {
        return property;
    }

    /**
     * The per-rule breakdown of a refused <em>declaration</em> (each entry has
     * {@code rule}, {@code entity}, {@code entity_type}, {@code property},
     * {@code count}); empty for a refused write.
     *
     * @return the report rows, never {@code null}
     */
    public List<Map<String, Object>> report() {
        return report;
    }
}
