package io.github.kkollsga.kglite;

import java.time.Instant;
import java.time.LocalDate;
import java.time.LocalDateTime;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.time.ZonedDateTime;
import java.time.format.DateTimeFormatter;
import java.time.format.DateTimeParseException;
import java.time.temporal.ChronoField;

/**
 * The instant a read runs as of: the statement prefix
 * {@code FOR VALID_TIME AS OF date('2009-06-30')} (or {@code datetime('…')})
 * written before the query text, which the engine reads exactly as if the
 * caller had typed it. The Python and MCP bindings' {@code valid_at} write
 * the same prefix.
 *
 * <p>The literal is rendered from {@code java.time}, never spliced from
 * caller text, so it cannot carry anything but a date or a datetime:
 * <ul>
 *   <li>a {@link LocalDate} becomes {@code date('yyyy-MM-dd')};</li>
 *   <li>a {@link LocalDateTime} becomes {@code datetime('yyyy-MM-ddTHH:mm:ss')}
 *       (with the fraction of a second when it has one), read as naive UTC —
 *       the engine's rule for every datetime, and the Python binding's;</li>
 *   <li>an {@link OffsetDateTime}, {@link ZonedDateTime} or {@link Instant} is
 *       converted to UTC first, then rendered as a {@code LocalDateTime};</li>
 *   <li>a {@link String} is parsed as an ISO date, an ISO local datetime or an
 *       ISO offset datetime, in that order, then rendered as above.</li>
 * </ul>
 *
 * <p>A year outside {@code 0..9999} is refused: {@code java.time} prints such a
 * year with a sign ({@code +10000-01-01}), which the Cypher literal does not
 * read. The engine itself accepts a five-digit year typed into the prefix.
 * A query that already carries a {@code FOR … AS OF} prefix is refused by
 * the engine's parser (a statement takes one context), surfacing as a
 * {@link KgliteException}.
 *
 * <p>Immutable and safe to share across threads.
 */
public final class ValidAt {

    private static final DateTimeFormatter DATE = DateTimeFormatter.ofPattern("uuuu-MM-dd");
    private static final DateTimeFormatter DATE_TIME =
            DateTimeFormatter.ofPattern("uuuu-MM-dd'T'HH:mm:ss");

    /** What {@link #literal()} names for {@link #all()}. */
    private static final String ALL_LITERAL = "ALL";

    private static final ValidAt ALL = new ValidAt(ALL_LITERAL);

    private final String literal;

    private ValidAt(String literal) {
        this.literal = literal;
    }

    /**
     * Every version: the statement prefix {@code FOR VALID_TIME ALL}, which
     * opts one read out of the graph's valid-time default (as of today unless
     * configured otherwise) and filters nothing. A graph with no validity
     * declaration answers the same with or without it.
     *
     * @return the all-versions instant
     */
    public static ValidAt all() {
        return ALL;
    }

    /**
     * As of a date.
     *
     * @param date the date; never {@code null}
     * @return the instant
     * @throws IllegalArgumentException if {@code date} is {@code null} or its
     *     year is outside {@code 0..9999}
     */
    public static ValidAt of(LocalDate date) {
        if (date == null) {
            throw new IllegalArgumentException("validAt needs a date");
        }
        checkYear(date.getYear());
        return new ValidAt("date('" + DATE.format(date) + "')");
    }

    /**
     * As of a datetime, read as naive UTC.
     *
     * @param dateTime the datetime; never {@code null}
     * @return the instant
     * @throws IllegalArgumentException if {@code dateTime} is {@code null} or
     *     its year is outside {@code 0..9999}
     */
    public static ValidAt of(LocalDateTime dateTime) {
        if (dateTime == null) {
            throw new IllegalArgumentException("validAt needs a datetime");
        }
        checkYear(dateTime.getYear());
        String text = DATE_TIME.format(dateTime);
        int nanos = dateTime.get(ChronoField.NANO_OF_SECOND);
        if (nanos != 0) {
            String fraction = String.format("%09d", nanos).replaceAll("0+$", "");
            text = text + "." + fraction;
        }
        return new ValidAt("datetime('" + text + "')");
    }

    /**
     * As of an offset datetime, converted to UTC.
     *
     * @param dateTime the datetime; never {@code null}
     * @return the instant
     * @throws IllegalArgumentException if {@code dateTime} is {@code null} or
     *     its UTC year is outside {@code 0..9999}
     */
    public static ValidAt of(OffsetDateTime dateTime) {
        if (dateTime == null) {
            throw new IllegalArgumentException("validAt needs a datetime");
        }
        return of(dateTime.withOffsetSameInstant(ZoneOffset.UTC).toLocalDateTime());
    }

    /**
     * As of a zoned datetime, converted to UTC.
     *
     * @param dateTime the datetime; never {@code null}
     * @return the instant
     * @throws IllegalArgumentException if {@code dateTime} is {@code null} or
     *     its UTC year is outside {@code 0..9999}
     */
    public static ValidAt of(ZonedDateTime dateTime) {
        if (dateTime == null) {
            throw new IllegalArgumentException("validAt needs a datetime");
        }
        return of(dateTime.toOffsetDateTime());
    }

    /**
     * As of an instant, in UTC.
     *
     * @param instant the instant; never {@code null}
     * @return the instant
     * @throws IllegalArgumentException if {@code instant} is {@code null} or
     *     its UTC year is outside {@code 0..9999}
     */
    public static ValidAt of(Instant instant) {
        if (instant == null) {
            throw new IllegalArgumentException("validAt needs an instant");
        }
        return of(instant.atOffset(ZoneOffset.UTC));
    }

    /**
     * As of an ISO date ({@code "2009-06-30"}), local datetime
     * ({@code "2009-06-30T12:00:00"}) or offset datetime
     * ({@code "2009-06-30T12:00:00+02:00"}).
     *
     * @param iso the ISO text; never {@code null}
     * @return the instant
     * @throws IllegalArgumentException if {@code iso} is {@code null}, is none
     *     of the three forms, or names a year outside {@code 0..9999}
     */
    public static ValidAt parse(String iso) {
        if (iso == null) {
            throw new IllegalArgumentException("validAt needs an ISO date or datetime");
        }
        try {
            return of(LocalDate.parse(iso));
        } catch (DateTimeParseException notADate) {
            try {
                return of(LocalDateTime.parse(iso));
            } catch (DateTimeParseException notLocal) {
                try {
                    return of(OffsetDateTime.parse(iso));
                } catch (DateTimeParseException notOffset) {
                    throw new IllegalArgumentException(
                            "validAt: '" + iso + "' is not an ISO date or datetime", notOffset);
                }
            }
        }
    }

    /**
     * The literal the prefix names: {@code date('…')}, {@code datetime('…')},
     * or {@code ALL} for {@link #all()}.
     *
     * @return the literal
     */
    public String literal() {
        return literal;
    }

    /**
     * {@code query} behind this instant's {@code FOR VALID_TIME AS OF} prefix,
     * or behind {@code FOR VALID_TIME ALL} for {@link #all()}. {@code EXPLAIN}
     * and {@code PROFILE} may lead {@code query}.
     *
     * @param query the Cypher text
     * @return the prefixed text
     */
    public String prefix(String query) {
        if (this == ALL) {
            return "FOR VALID_TIME ALL " + query;
        }
        return "FOR VALID_TIME AS OF " + literal + " " + query;
    }

    private static void checkYear(int year) {
        if (year < 0 || year > 9999) {
            throw new IllegalArgumentException(
                    "validAt: year " + year + " is outside 0..9999; java.time renders it with a"
                            + " sign, which the FOR VALID_TIME AS OF literal does not read");
        }
    }

    @Override
    public boolean equals(Object other) {
        return other instanceof ValidAt that && literal.equals(that.literal);
    }

    @Override
    public int hashCode() {
        return literal.hashCode();
    }

    @Override
    public String toString() {
        return "ValidAt[" + literal + "]";
    }
}
