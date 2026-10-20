//! Conversion and scoring kernels for the quantized scan copies.
//!
//! Every kernel has a scalar form that is also the test oracle. `f16` and the
//! Hamming popcount get a runtime-detected x86 fast path (F16C+AVX, POPCNT) —
//! wheels build at baseline x86-64, so a compile-time `target_feature` would
//! never be on — while aarch64 relies on the baseline NEON auto-vectorization
//! of the scalar forms (`f16` is a stable *storage* type only through bit
//! patterns: the language `f16` is unstable).

/// Largest finite `f16`.
const F16_MAX_BITS: u16 = 0x7bff;

/// Round an `f32` to the nearest `f16` bit pattern (ties to even), saturating
/// to ±65504 instead of overflowing to infinity so a stored copy of a finite
/// vector is always finite. NaN maps to a quiet NaN; inputs are validated
/// finite before they reach a store.
pub(crate) fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let abs = bits & 0x7fff_ffff;
    if abs > 0x7f80_0000 {
        return sign | 0x7e00;
    }
    // 65520.0 is the first magnitude that rounds to infinity.
    if abs >= 0x477f_f000 {
        return sign | F16_MAX_BITS;
    }
    let exponent = (abs >> 23) as i32;
    if exponent < 113 {
        // Below 2^-14: a subnormal half (unit 2^-24) or zero.
        let shift = (126 - exponent) as u32;
        if shift > 24 {
            return sign;
        }
        let mantissa = (abs & 0x007f_ffff) | 0x0080_0000;
        let half = 1u32 << (shift - 1);
        let rest = mantissa & ((1u32 << shift) - 1);
        let mut out = mantissa >> shift;
        if rest > half || (rest == half && out & 1 == 1) {
            out += 1;
        }
        return sign | out as u16;
    }
    let mantissa = abs & 0x007f_ffff;
    let mut out = (((exponent - 112) as u32) << 10) | (mantissa >> 13);
    let rest = mantissa & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && out & 1 == 1) {
        out += 1;
    }
    sign | out as u16
}

/// Widen an `f16` bit pattern. Branch-free on the common path so the loops
/// that call it vectorize on SSE2 and NEON.
#[inline(always)]
pub(crate) fn f16_to_f32(half: u16) -> f32 {
    let half = half as u32;
    let sign = (half & 0x8000) << 16;
    let magnitude = half & 0x7fff;
    // Shifting the exponent/mantissa field into f32 position leaves it 112
    // binades low; the multiply restores that and normalizes subnormals.
    let scaled = f32::from_bits(magnitude << 13) * f32::from_bits(0x7780_0000);
    let bits = if magnitude >= 0x7c00 {
        0x7f80_0000 | ((magnitude & 0x3ff) << 13)
    } else {
        scaled.to_bits()
    };
    f32::from_bits(sign | bits)
}

/// Scalar `f16`·`f32` dot product; also the reference for the x86 kernel.
pub(crate) fn dot_f16_scalar(codes: &[u16], query: &[f32]) -> f32 {
    let (mut s0, mut s1, mut s2, mut s3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (code_chunks, code_rest) = codes.as_chunks::<8>();
    let (query_chunks, query_rest) = query.as_chunks::<8>();
    for (c, q) in code_chunks.iter().zip(query_chunks) {
        s0 += f16_to_f32(c[0]) * q[0] + f16_to_f32(c[4]) * q[4];
        s1 += f16_to_f32(c[1]) * q[1] + f16_to_f32(c[5]) * q[5];
        s2 += f16_to_f32(c[2]) * q[2] + f16_to_f32(c[6]) * q[6];
        s3 += f16_to_f32(c[3]) * q[3] + f16_to_f32(c[7]) * q[7];
    }
    for (c, q) in code_rest.iter().zip(query_rest) {
        s0 += f16_to_f32(*c) * q;
    }
    (s0 + s1) + (s2 + s3)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,f16c")]
unsafe fn dot_f16_f16c(codes: &[u16], query: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = codes.len().min(query.len());
    let (codes_ptr, query_ptr) = (codes.as_ptr(), query.as_ptr());
    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();
    let mut i = 0;
    // SAFETY: every load reads 8 or 16 elements starting at `i` with
    // `i + width <= n <= both slice lengths`; the loads are unaligned.
    unsafe {
        while i + 16 <= n {
            let a = _mm256_cvtph_ps(_mm_loadu_si128(codes_ptr.add(i).cast()));
            let b = _mm256_cvtph_ps(_mm_loadu_si128(codes_ptr.add(i + 8).cast()));
            acc0 = _mm256_add_ps(acc0, _mm256_mul_ps(a, _mm256_loadu_ps(query_ptr.add(i))));
            acc1 = _mm256_add_ps(
                acc1,
                _mm256_mul_ps(b, _mm256_loadu_ps(query_ptr.add(i + 8))),
            );
            i += 16;
        }
        if i + 8 <= n {
            let a = _mm256_cvtph_ps(_mm_loadu_si128(codes_ptr.add(i).cast()));
            acc0 = _mm256_add_ps(acc0, _mm256_mul_ps(a, _mm256_loadu_ps(query_ptr.add(i))));
            i += 8;
        }
    }
    let mut lanes = [0.0f32; 8];
    // SAFETY: `lanes` holds exactly the 8 floats stored.
    unsafe { _mm256_storeu_ps(lanes.as_mut_ptr(), _mm256_add_ps(acc0, acc1)) };
    let mut sum = ((lanes[0] + lanes[1]) + (lanes[2] + lanes[3]))
        + ((lanes[4] + lanes[5]) + (lanes[6] + lanes[7]));
    while i < n {
        sum += f16_to_f32(codes[i]) * query[i];
        i += 1;
    }
    sum
}

#[cfg(target_arch = "x86_64")]
fn dot_f16_hw(codes: &[u16], query: &[f32]) -> f32 {
    // SAFETY: only selected by `dot_f16_kernel` after runtime detection of
    // both features the function enables.
    unsafe { dot_f16_f16c(codes, query) }
}

/// The best `f16`·`f32` dot kernel this CPU supports, resolved once per query
/// so the per-vector call is a plain indirect call.
pub(crate) fn dot_f16_kernel() -> fn(&[u16], &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("f16c") && std::arch::is_x86_feature_detected!("avx")
        {
            return dot_f16_hw;
        }
    }
    dot_f16_scalar
}

/// Name of the kernel [`dot_f16_kernel`] selects, for diagnostics and tests.
pub(crate) fn f16_kernel_name() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("f16c") && std::arch::is_x86_feature_detected!("avx")
        {
            return "f16c";
        }
    }
    "scalar"
}

/// Name of the kernel [`hamming_kernel`] selects.
pub(crate) fn hamming_kernel_name() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("popcnt") {
            return "popcnt";
        }
    }
    "scalar"
}

/// Integer dot product of two int8 rows. i32 cannot overflow below
/// 133,000 dimensions; quantization refuses wider stores.
#[inline]
pub(crate) fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
    a.iter().zip(b).map(|(&x, &y)| x as i32 * y as i32).sum()
}

#[inline(always)]
fn hamming_impl(a: &[u8], b: &[u8]) -> u32 {
    let (a_words, a_rest) = a.as_chunks::<8>();
    let (b_words, b_rest) = b.as_chunks::<8>();
    let mut bits = 0u32;
    for (x, y) in a_words.iter().zip(b_words) {
        bits += (u64::from_le_bytes(*x) ^ u64::from_le_bytes(*y)).count_ones();
    }
    for (x, y) in a_rest.iter().zip(b_rest) {
        bits += (x ^ y).count_ones();
    }
    bits
}

/// Scalar Hamming distance; also the reference for the POPCNT kernel.
pub(crate) fn hamming_scalar(a: &[u8], b: &[u8]) -> u32 {
    hamming_impl(a, b)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "popcnt")]
unsafe fn hamming_popcnt(a: &[u8], b: &[u8]) -> u32 {
    hamming_impl(a, b)
}

#[cfg(target_arch = "x86_64")]
fn hamming_hw(a: &[u8], b: &[u8]) -> u32 {
    // SAFETY: only selected by `hamming_kernel` after runtime POPCNT detection.
    unsafe { hamming_popcnt(a, b) }
}

/// The best Hamming kernel this CPU supports (resolved once per query).
pub(crate) fn hamming_kernel() -> fn(&[u8], &[u8]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("popcnt") {
            return hamming_hw;
        }
    }
    hamming_scalar
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(state: &mut u64) -> f32 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
    }

    /// Every f16 pattern round-trips through f32, and re-encoding the widened
    /// value reproduces the pattern (NaN payloads excepted).
    #[test]
    fn every_half_round_trips() {
        for bits in 0..=u16::MAX {
            let widened = f16_to_f32(bits);
            let exponent = (bits >> 10) & 0x1f;
            let mantissa = bits & 0x3ff;
            if exponent == 0x1f {
                if mantissa == 0 {
                    assert!(widened.is_infinite(), "{bits:#06x}");
                    assert_eq!(widened.is_sign_negative(), bits & 0x8000 != 0);
                } else {
                    assert!(widened.is_nan(), "{bits:#06x}");
                }
                continue;
            }
            assert_eq!(f32_to_f16(widened), bits, "{bits:#06x} -> {widened}");
        }
    }

    /// Nearest-even rounding against exact arithmetic: the encoded value is
    /// the closest representable half, with ties to the even pattern.
    #[test]
    fn rounding_picks_the_nearest_half() {
        let mut state = 7u64;
        for _ in 0..200_000 {
            let scale = [1.0, 1e-3, 1e-6, 100.0, 60000.0][(state % 5) as usize];
            let value = lcg(&mut state).abs() * scale;
            let encoded = f32_to_f16(value);
            let error = (f16_to_f32(encoded) as f64 - value as f64).abs();
            if encoded > 0 {
                let lower = f16_to_f32(encoded - 1) as f64;
                assert!(error <= (lower - value as f64).abs(), "{value}");
            }
            if encoded < F16_MAX_BITS {
                let upper = f16_to_f32(encoded + 1) as f64;
                assert!(error <= (upper - value as f64).abs(), "{value}");
            }
        }
        // Exact ties go to the even pattern: 1 + 2^-11 sits between 1.0 and 1 + 2^-10.
        assert_eq!(f32_to_f16(1.0 + 2f32.powi(-11)), 0x3c00);
        assert_eq!(f32_to_f16(1.0 + 3.0 * 2f32.powi(-11)), 0x3c02);
    }

    #[test]
    fn overflow_saturates_and_specials_survive() {
        assert_eq!(f32_to_f16(65504.0), 0x7bff);
        assert_eq!(f32_to_f16(65519.0), 0x7bff);
        assert_eq!(f32_to_f16(1e30), 0x7bff);
        assert_eq!(f32_to_f16(-1e30), 0xfbff);
        assert_eq!(f32_to_f16(f32::INFINITY), 0x7bff);
        assert_eq!(f32_to_f16(0.0), 0);
        assert_eq!(f32_to_f16(-0.0), 0x8000);
        assert_eq!(f32_to_f16(1e-9), 0);
        assert!(f16_to_f32(f32_to_f16(f32::NAN)).is_nan());
    }

    #[test]
    fn selected_f16_kernel_matches_the_scalar_reference() {
        let kernel = dot_f16_kernel();
        let mut state = 11u64;
        for len in [0usize, 1, 7, 8, 9, 15, 16, 17, 31, 33, 100, 1024] {
            let codes: Vec<u16> = (0..len).map(|_| f32_to_f16(lcg(&mut state))).collect();
            let query: Vec<f32> = (0..len).map(|_| lcg(&mut state)).collect();
            let reference: f32 = codes
                .iter()
                .zip(&query)
                .map(|(&c, &q)| f16_to_f32(c) as f64 * q as f64)
                .sum::<f64>() as f32;
            let scalar = dot_f16_scalar(&codes, &query);
            let selected = kernel(&codes, &query);
            let tolerance = 1e-4 * (1.0 + len as f32).sqrt();
            assert!((scalar - reference).abs() <= tolerance, "scalar len {len}");
            assert!(
                (selected - reference).abs() <= tolerance,
                "{} len {len}: {selected} vs {reference}",
                f16_kernel_name()
            );
        }
    }

    #[test]
    fn integer_and_bit_kernels_match_naive_loops() {
        let mut state = 5u64;
        for len in [0usize, 1, 3, 8, 9, 63, 64, 65, 300] {
            let a: Vec<i8> = (0..len).map(|_| (lcg(&mut state) * 127.0) as i8).collect();
            let b: Vec<i8> = (0..len).map(|_| (lcg(&mut state) * 127.0) as i8).collect();
            let naive: i32 = a.iter().zip(&b).map(|(&x, &y)| x as i32 * y as i32).sum();
            assert_eq!(dot_i8(&a, &b), naive);
            let x: Vec<u8> = (0..len)
                .map(|_| (lcg(&mut state) * 127.0) as i8 as u8)
                .collect();
            let y: Vec<u8> = (0..len)
                .map(|_| (lcg(&mut state) * 127.0) as i8 as u8)
                .collect();
            let naive_bits: u32 = x.iter().zip(&y).map(|(p, q)| (p ^ q).count_ones()).sum();
            assert_eq!(hamming_scalar(&x, &y), naive_bits);
            assert_eq!(hamming_kernel()(&x, &y), naive_bits);
        }
    }
}
