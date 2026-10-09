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
 * @param months  whole calendar months
 * @param days    whole days
 * @param seconds whole seconds
 */
public record KgliteDuration(int months, int days, long seconds) {}
