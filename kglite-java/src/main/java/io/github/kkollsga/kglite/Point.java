package io.github.kkollsga.kglite;

/**
 * A geographic point as the engine stores it: WGS84 degrees.
 *
 * <p>A point cell arrives as a {@code Point}, and a {@code Point} bound as a
 * parameter reaches the engine as a point, so a cell read back and bound again
 * is unchanged.
 *
 * @param latitude  degrees north
 * @param longitude degrees east
 */
public record Point(double latitude, double longitude) {}
