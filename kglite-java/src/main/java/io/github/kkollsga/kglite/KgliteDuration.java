package io.github.kkollsga.kglite;

/**
 * A calendar duration as the engine stores it: months and days are calendar
 * units, so a month is not a fixed number of seconds, which is why
 * {@link java.time.Duration} cannot carry it.
 *
 * <p>A duration cell arrives as a {@code KgliteDuration}, and one bound as a
 * parameter reaches the engine as a duration. {@link java.time.Period} and
 * {@link java.time.Duration} parameters are accepted too, and map onto it.
 *
 * <p>The clock part is {@code seconds + nanos / 1e9}: {@code seconds} carries the
 * sign and {@code nanos} is always 0 to 999999999, the same split as
 * {@link java.time.Duration#getSeconds()} and {@link java.time.Duration#getNano()}.
 *
 * @param months  whole calendar months
 * @param days    whole days
 * @param seconds whole seconds, signed
 * @param nanos   sub-second part, 0 to 999999999, added to {@code seconds}
 */
public record KgliteDuration(int months, int days, long seconds, int nanos) {

    /** Reject a {@code nanos} outside 0 to 999999999. */
    public KgliteDuration {
        if (nanos < 0 || nanos >= 1_000_000_000) {
            throw new IllegalArgumentException("nanos must be in 0..999999999: " + nanos);
        }
    }

    /**
     * A whole-second duration.
     *
     * @param months  whole calendar months
     * @param days    whole days
     * @param seconds whole seconds, signed
     */
    public KgliteDuration(int months, int days, long seconds) {
        this(months, days, seconds, 0);
    }
}
