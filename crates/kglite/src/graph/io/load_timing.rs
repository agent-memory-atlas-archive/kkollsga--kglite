//! Per-stage wall-clock timing for disk load (`load_disk_dir`) and save
//! (`save_disk`), gated by the `KGLITE_LOAD_TIMING` env var. Off by default
//! (zero overhead); when set, each stage emits a single
//! `[TIMING] stage=<name> dur_ms=<ms>` line to stderr. Consumed by the 0.8.13
//! load-perf benchmarks and by `kglite.load(path, timing=True)` in the Python
//! wrapper.

use std::time::Instant;

/// Start a timer if `KGLITE_LOAD_TIMING` is set in the environment.
/// Returns `None` otherwise (zero overhead on the hot path).
#[inline]
pub fn stage_timer() -> Option<Instant> {
    if std::env::var_os("KGLITE_LOAD_TIMING").is_some() {
        Some(Instant::now())
    } else {
        None
    }
}

/// Emit a `[TIMING]` line for this stage if timing is enabled. Accepts
/// the `Option<Instant>` returned by `stage_timer()`, so callers don't
/// need to conditionally test the env var themselves.
#[inline]
pub fn log_stage(name: &str, timer: Option<Instant>) {
    if let Some(t) = timer {
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("[TIMING] stage={} dur_ms={:.1}", name, ms);
    }
}

/// Sequential stage clock for a multi-step operation such as a disk save: each
/// [`Self::mark`] logs the time since the previous mark (or construction) under
/// the same `KGLITE_LOAD_TIMING` gate and line format as [`log_stage`], then
/// restarts. Zero overhead when the variable is unset.
pub struct StageClock {
    last: Option<Instant>,
}

impl StageClock {
    #[inline]
    pub fn start() -> Self {
        Self {
            last: stage_timer(),
        }
    }

    #[inline]
    pub fn mark(&mut self, name: &str) {
        if let Some(last) = self.last {
            let now = Instant::now();
            let ms = now.duration_since(last).as_secs_f64() * 1000.0;
            eprintln!("[TIMING] stage={} dur_ms={:.1}", name, ms);
            self.last = Some(now);
        }
    }
}
