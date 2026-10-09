package io.github.kkollsga.kglite;

import java.util.Map;
import java.util.OptionalLong;

/**
 * What {@link KnowledgeGraph#backup(java.nio.file.Path)} wrote.
 *
 * @param path           the backup file
 * @param bytes          its size
 * @param nodes          nodes in the backup
 * @param relationships  relationships in the backup
 * @param graphVersion   the graph version the backup captured
 * @param lsn            the write-ahead-log position it captured, or empty for
 *     a session without a log
 * @param lockHoldMs     how long writers were held off
 * @param elapsedMs      total duration
 * @param preparedCopy   whether the snapshot needed a private prepared copy first
 */
public record BackupReport(
        String path,
        long bytes,
        long nodes,
        long relationships,
        long graphVersion,
        OptionalLong lsn,
        double lockHoldMs,
        double elapsedMs,
        boolean preparedCopy) {

    static BackupReport parse(String json) {
        if (!(Json.parse(json) instanceof Map<?, ?> f)) {
            throw new KgliteException("expected a JSON object backup report, got " + json);
        }
        return new BackupReport(
                String.valueOf(f.get("path")),
                number(f.get("bytes")).longValue(),
                number(f.get("nodes")).longValue(),
                number(f.get("relationships")).longValue(),
                number(f.get("graph_version")).longValue(),
                f.get("lsn") instanceof Number n ? OptionalLong.of(n.longValue()) : OptionalLong.empty(),
                number(f.get("lock_hold_ms")).doubleValue(),
                number(f.get("elapsed_ms")).doubleValue(),
                Boolean.TRUE.equals(f.get("prepared_copy")));
    }

    private static Number number(Object value) {
        return value instanceof Number n ? n : 0L;
    }
}
