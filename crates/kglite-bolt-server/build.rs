//! Rebuild this binary whenever the workspace lockfile changes.
//!
//! cargo rebuilds a binary for a lock change only when it alters one of the
//! binary's own dependencies. The Python suites run the prebuilt binary and
//! treat one older than `Cargo.lock` as stale, so without this a lock change
//! could leave the "rebuild with cargo build" advice unable to clear that
//! verdict. The lockfile is not part of the published crate, so the line is
//! emitted only when the file is there.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let lock = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    if lock.exists() {
        println!("cargo:rerun-if-changed={}", lock.display());
    }
}
