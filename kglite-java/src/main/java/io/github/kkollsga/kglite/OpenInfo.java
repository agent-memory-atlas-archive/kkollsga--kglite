package io.github.kkollsga.kglite;

import java.util.List;
import java.util.Map;
import java.util.Optional;

/**
 * What a durable open reports about the graph it returned.
 *
 * @param path          the path that was opened
 * @param readOnly      whether the session refuses writes
 * @param created       whether the open created the graph
 * @param storage       the storage mode now running
 * @param durability    the durability level actually in force
 * @param degradedFrom  the level that was requested but degraded to
 *     {@link Durability#OFF} (a disk-mode graph has no log), or empty
 * @param convertedFrom the mode the graph was in before an explicit storage
 *     option converted it, or empty
 * @param advisories    notices an operator should read, such as a quarantined
 *     log or a saved torn tail; empty when the open was clean
 */
public record OpenInfo(
        String path,
        boolean readOnly,
        boolean created,
        StorageMode storage,
        Durability durability,
        Optional<Durability> degradedFrom,
        Optional<StorageMode> convertedFrom,
        List<Advisory> advisories) {

    /**
     * One open-time notice.
     *
     * @param code     the machine-readable kind
     * @param message  the human-readable description
     * @param affected what it concerns (a file or path), or {@code null}
     */
    public record Advisory(String code, String message, String affected) {}

    static OpenInfo parse(String json) {
        if (!(Json.parse(json) instanceof Map<?, ?> fields)) {
            throw new KgliteException("expected a JSON object of open info, got " + json);
        }
        List<Advisory> advisories = fields.get("advisories") instanceof List<?> items
                ? items.stream().filter(Map.class::isInstance).map(item -> {
                    Map<?, ?> entry = (Map<?, ?>) item;
                    return new Advisory(text(entry.get("code")), text(entry.get("message")),
                            text(entry.get("affected")));
                }).toList()
                : List.of();
        String degraded = text(fields.get("degraded_from"));
        String converted = text(fields.get("converted_from"));
        return new OpenInfo(
                text(fields.get("path")),
                Boolean.TRUE.equals(fields.get("read_only")),
                Boolean.TRUE.equals(fields.get("created")),
                StorageMode.fromWire(text(fields.get("storage"))),
                Durability.fromWire(text(fields.get("durability"))),
                Optional.ofNullable(degraded).map(Durability::fromWire),
                Optional.ofNullable(converted).map(StorageMode::fromWire),
                advisories);
    }

    private static String text(Object value) {
        return value == null ? null : value.toString();
    }
}
