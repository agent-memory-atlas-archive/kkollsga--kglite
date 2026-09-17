//! `okf::validate` — the build report as a value (VAULT.md §9).
//!
//! The check is the build. Everything §9 classifies is something the builder
//! learns while building — a collision it had to settle, a link that reached
//! nothing, a declaration whose label the vault does not carry — so a separate
//! "checker" would be a second implementation of the reader, free to disagree
//! with the thing it is checking. This runs the real pipeline and throws the
//! graph away.

use super::model::{BuildOptions, BuildReport};
use std::path::Path;

/// Build `root` and return the report, discarding the graph.
///
/// `Err` is reserved for a root that cannot be read at all — absent, or not a
/// directory — which is a caller mistake rather than a finding about a vault.
/// Everything else comes back *in* the report, including the `vault.yaml`
/// schema failure that stops [`super::build`] outright (VAULT.md §7): a
/// validator whose whole job is to describe what is wrong cannot be the one
/// caller that refuses to say.
pub fn validate(root: &Path, opts: &BuildOptions) -> Result<BuildReport, String> {
    super::walk::check_root(root)?;
    Ok(match super::build(root, opts) {
        Ok(output) => output.report,
        Err(message) => BuildReport {
            errors: vec![message],
            ..BuildReport::default()
        },
    })
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod validate_tests;
