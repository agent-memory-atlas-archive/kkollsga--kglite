//! Scoped thread-local switches for the tests' failure injection.

use std::cell::Cell;
use std::thread::LocalKey;

/// Puts a switch back when dropped, so a panicking `body` cannot leave it set
/// for the next test that runs on the same thread.
struct Restore<V: Copy + 'static> {
    switch: &'static LocalKey<Cell<V>>,
    off: V,
}

impl<V: Copy + 'static> Drop for Restore<V> {
    fn drop(&mut self) {
        self.switch.with(|cell| cell.set(self.off));
    }
}

/// Run `body` with `switch` set to `on` on this thread, then back to `off`,
/// whether `body` returns or panics.
pub(crate) fn scoped<V: Copy + 'static, T>(
    switch: &'static LocalKey<Cell<V>>,
    on: V,
    off: V,
    body: impl FnOnce() -> T,
) -> T {
    switch.with(|cell| cell.set(on));
    let _restore = Restore { switch, off };
    body()
}
