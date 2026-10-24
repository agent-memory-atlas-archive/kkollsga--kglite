//! What the running server's skill registry resolved, handed to `--selftest`.
//!
//! `prompts/list` carries names only, so over the wire a skill that was
//! skipped at load (a file past the framework's hard size limit, bad
//! frontmatter) is indistinguishable from one that was never there. The
//! selftest parent sets [`REPORT_ENV`] on the child it spawns; the child
//! writes the registry's active set (with the layer each skill won from) and
//! its per-file load failures to that path after every composition, so the
//! parent reports what the child actually serves rather than a re-derivation.

use std::path::PathBuf;

use mcp_methods::server::{ActiveSkill, ParseWarning, SkillProvenance};
use serde_json::{json, Value};

/// Path the child writes its skill report to. Set only by `--selftest`.
pub(crate) const REPORT_ENV: &str = "KGLITE_SKILLS_REPORT";

/// The layer a skill resolved from, as the selftest line names it.
pub(crate) fn layer_label(provenance: &SkillProvenance) -> String {
    match provenance {
        SkillProvenance::Project => "project".to_string(),
        SkillProvenance::DomainPack(_) => "pack".to_string(),
        SkillProvenance::Owned(label) => label.clone(),
        SkillProvenance::Inline => "manifest".to_string(),
        SkillProvenance::Bundled => "bundled".to_string(),
    }
}

pub(crate) fn render_report(active: &[ActiveSkill], warnings: &[ParseWarning]) -> Value {
    json!({
        "served": active
            .iter()
            .map(|skill| json!({"name": skill.name, "layer": layer_label(&skill.provenance)}))
            .collect::<Vec<_>>(),
        "skipped": warnings
            .iter()
            .map(|w| json!({"path": w.path.display().to_string(), "error": w.error}))
            .collect::<Vec<_>>(),
    })
}

/// Write the report when a selftest parent asked for one; otherwise a no-op.
pub(crate) fn publish(active: &[ActiveSkill], warnings: &[ParseWarning]) {
    let Some(path) = std::env::var_os(REPORT_ENV).map(PathBuf::from) else {
        return;
    };
    let body = render_report(active, warnings).to_string();
    if let Err(error) = std::fs::write(&path, body) {
        tracing::warn!(%error, path = %path.display(), "could not write the skills report");
    }
}
