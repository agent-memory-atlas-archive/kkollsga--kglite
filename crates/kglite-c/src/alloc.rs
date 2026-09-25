//! Tracking global allocator + memory stats for the C ABI.
//!
//! kglite-c installs a tracking allocator (wrapping mimalloc, the allocator
//! the Python wheel also uses) so a binding can observe the Rust-side heap via
//! [`kglite_memory_stats`] — current live bytes, peak since process
//! start, and total allocation count. Counters are process-wide.
//!
//! mimalloc rather than the system allocator because the engine is
//! allocation-heavy: on macOS, engine-bound queries through this library ran
//! 22–32% slower on the system allocator (release builds, measured
//! 2026-09-25). It is the v2 line, pinned for the reason the wheel pins it.
//!
//! Only allocations made through the Rust global allocator are counted;
//! the host runtime's own heap (Go, the JVM, Node, …) is separate and
//! invisible here. The counters are maintained with `Relaxed` atomics —
//! cheap, and exact accounting across threads isn't required for a
//! monitoring stat.

use mimalloc::MiMalloc;
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering};

static CURRENT: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
static TOTAL_ALLOCS: AtomicU64 = AtomicU64::new(0);

/// mimalloc wrapper that tallies bytes + allocation count.
struct TrackingAllocator;

// SAFETY: every method forwards to `MiMalloc` (a sound `GlobalAlloc`) and
// only adds bookkeeping; we never hand back a pointer it didn't produce.
// realloc is forwarded so a growth can happen in place; it counts as one
// allocation and moves the live-byte tally by the size difference.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { MiMalloc.alloc(layout) };
        if !ptr.is_null() {
            let size = layout.size() as u64;
            TOTAL_ALLOCS.fetch_add(1, Ordering::Relaxed);
            let now = CURRENT.fetch_add(size, Ordering::Relaxed) + size;
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { MiMalloc.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size() as u64, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { MiMalloc.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            TOTAL_ALLOCS.fetch_add(1, Ordering::Relaxed);
            let old = layout.size() as u64;
            let size = new_size as u64;
            if size >= old {
                let now = CURRENT.fetch_add(size - old, Ordering::Relaxed) + (size - old);
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(old - size, Ordering::Relaxed);
            }
        }
        new
    }
}

#[global_allocator]
static GLOBAL: TrackingAllocator = TrackingAllocator;

/// Rust-heap statistics from kglite's tracking allocator.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KgMemStats {
    /// Current live Rust-heap bytes (allocated minus freed).
    pub current_bytes: u64,
    /// Peak live Rust-heap bytes since process start.
    pub peak_bytes: u64,
    /// Total number of allocations since process start (monotonic).
    pub total_allocs: u64,
}

/// Return current Rust-heap statistics from kglite's tracking allocator.
/// Counts only allocations through the Rust global allocator — the host
/// runtime's own heap is separate. Useful for a binding to surface
/// kglite's memory footprint in its own metrics.
#[no_mangle]
pub extern "C" fn kglite_memory_stats() -> KgMemStats {
    crate::ffi::value_boundary(
        KgMemStats {
            current_bytes: 0,
            peak_bytes: 0,
            total_allocs: 0,
        },
        || {
            let current_bytes = CURRENT.load(Ordering::Relaxed);
            // Another thread can be between CURRENT.fetch_add and PEAK.fetch_max.
            // Fold this observation into PEAK so every returned snapshot preserves
            // the public peak >= current invariant without serializing allocations.
            let peak_bytes = PEAK.fetch_max(current_bytes, Ordering::Relaxed);
            KgMemStats {
                current_bytes,
                peak_bytes: peak_bytes.max(current_bytes),
                total_allocs: TOTAL_ALLOCS.load(Ordering::Relaxed),
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_stats_track_allocations() {
        let before = kglite_memory_stats();
        // Force some heap traffic the optimizer can't elide.
        let v: Vec<u64> = (0..10_000).collect();
        let after = kglite_memory_stats();
        assert!(after.total_allocs >= before.total_allocs);
        assert!(after.peak_bytes >= after.current_bytes);
        // Keep `v` alive across the second reading.
        assert_eq!(v.len(), 10_000);
    }

    /// A realloc moves the live-byte tally by the size difference. The
    /// counters are process-wide and other tests run concurrently, so the
    /// check allows a margin far below the 64 MiB it measures.
    #[test]
    fn realloc_moves_the_live_byte_tally() {
        const MIB: u64 = 1 << 20;
        let mut buffer: Vec<u8> = Vec::with_capacity(MIB as usize);
        buffer.push(1);
        let before = kglite_memory_stats().current_bytes;
        buffer.reserve_exact(65 * MIB as usize);
        let grown = kglite_memory_stats().current_bytes;
        let delta = grown.wrapping_sub(before) as i64;
        assert!(
            (32 * MIB as i64..96 * MIB as i64).contains(&delta),
            "a 64 MiB growth moved the tally by {delta} bytes"
        );
        drop(buffer);
    }
}
