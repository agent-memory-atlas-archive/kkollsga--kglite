//! Classified build advisories.
//!
//! A blueprint build can say a dozen different things about its input, and a
//! caller reading them needs to know which ones change what the graph means
//! and which are housekeeping. Every advisory therefore carries a [`group`]
//! (how much it matters) and a stable [`kind`] code (which check raised it),
//! assigned where the advisory is raised so no binding re-derives either from
//! the message text.
//!
//! [`group`]: Diagnostic::group
//! [`kind`]: Diagnostic::kind

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How much an advisory matters, most severe first: the derived ordering is
/// the severity ordering, so sorting ascending lists the groups a caller must
/// read first at the front.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticGroup {
    /// The build changes what queries on the graph mean: validity-interval
    /// declarations that were not made, defaults a reader should know about.
    Declarations,
    /// Placeholder nodes or dropped edges where an edge pointed at a node the
    /// build never loaded.
    Stubs,
    /// Input whose layout is not the one the blueprint declared: unknown keys,
    /// unsupported column types, cells that are not the declared shape.
    DataShape,
    /// Values that were loaded but are probably wrong: duplicate ids,
    /// identical rows, rows that are valid at no instant.
    DataQuality,
    /// Notes that change nothing a query returns.
    Cosmetic,
}

impl DiagnosticGroup {
    /// Every group, most severe first.
    pub const ALL: [DiagnosticGroup; 5] = [
        DiagnosticGroup::Declarations,
        DiagnosticGroup::Stubs,
        DiagnosticGroup::DataShape,
        DiagnosticGroup::DataQuality,
        DiagnosticGroup::Cosmetic,
    ];

    /// The stable name the group has in JSON, `graph_info()` and warnings.
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticGroup::Declarations => "declarations",
            DiagnosticGroup::Stubs => "stubs",
            DiagnosticGroup::DataShape => "data_shape",
            DiagnosticGroup::DataQuality => "data_quality",
            DiagnosticGroup::Cosmetic => "cosmetic",
        }
    }
}

impl DiagnosticGroup {
    /// The group a name from [`as_str`](Self::as_str) denotes.
    pub fn parse(name: &str) -> Option<DiagnosticGroup> {
        Self::ALL.into_iter().find(|g| g.as_str() == name)
    }
}

/// Groups `strict: true` fails a build on.
pub const STRICT_DEFAULT_GROUPS: [DiagnosticGroup; 2] =
    [DiagnosticGroup::Declarations, DiagnosticGroup::Stubs];

/// Kinds that inform rather than flag a fault: strict mode never fails on them,
/// whatever groups it was given, because a build that raises one is not wrong.
pub const INFORMATIONAL_KINDS: [&str; 1] = [KIND_DEFAULT_TODAY];

/// The note that a graph declaring validity reads valid-today by default.
pub const KIND_DEFAULT_TODAY: &str = "default_today";

/// Items per failing group a strict-mode error message lists.
const STRICT_ITEMS_SHOWN: usize = 5;

/// A blueprint's `strict` setting: `true` (the groups in
/// [`STRICT_DEFAULT_GROUPS`]), `false`, or an explicit list of group names.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum StrictSetting {
    Flag(bool),
    Groups(Vec<String>),
}

impl StrictSetting {
    /// The groups this setting fails a build on; empty when it is off.
    /// An unknown group name is an error naming the valid ones.
    pub fn groups(&self) -> Result<Vec<DiagnosticGroup>, String> {
        match self {
            StrictSetting::Flag(false) => Ok(Vec::new()),
            StrictSetting::Flag(true) => Ok(STRICT_DEFAULT_GROUPS.to_vec()),
            StrictSetting::Groups(names) => names
                .iter()
                .map(|name| {
                    DiagnosticGroup::parse(name).ok_or_else(|| {
                        let valid: Vec<&str> =
                            DiagnosticGroup::ALL.iter().map(|g| g.as_str()).collect();
                        format!(
                            "strict: unknown diagnostic group '{name}' — the groups are {}",
                            valid.join(", ")
                        )
                    })
                })
                .collect(),
        }
    }
}

/// The error a strict build fails with, or `None` when no advisory falls in
/// `groups`. It carries every failing group's count and its first items, so
/// the failure is readable without the report the build never returned.
pub fn strict_failure(groups: &[DiagnosticGroup], diagnostics: &[Diagnostic]) -> Option<String> {
    use std::fmt::Write;
    let mut text = String::new();
    for group in DiagnosticGroup::ALL {
        if !groups.contains(&group) {
            continue;
        }
        let items: Vec<&Diagnostic> = diagnostics
            .iter()
            .filter(|d| d.group == group && !INFORMATIONAL_KINDS.contains(&d.kind))
            .collect();
        if items.is_empty() {
            continue;
        }
        let _ = write!(
            text,
            "\n[{}] {} advisory(ies):",
            group.as_str(),
            items.len()
        );
        for d in items.iter().take(STRICT_ITEMS_SHOWN) {
            let _ = write!(text, "\n  - {}", d.message);
        }
        if items.len() > STRICT_ITEMS_SHOWN {
            let _ = write!(text, "\n  … and {} more", items.len() - STRICT_ITEMS_SHOWN);
        }
    }
    if text.is_empty() {
        return None;
    }
    Some(format!(
        "strict build failed — the build raised advisories in a strict group; nothing was \
         saved. Fix the input, or narrow `strict`:{text}"
    ))
}

/// One classified advisory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub group: DiagnosticGroup,
    /// Short stable code naming the check that raised it, for example
    /// `"typed_only_no_validity"`. Messages are prose and may be reworded;
    /// the kind is what a script matches on.
    pub kind: &'static str,
    pub message: String,
}

impl Diagnostic {
    pub fn new(group: DiagnosticGroup, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            group,
            kind,
            message: message.into(),
        }
    }
}

impl Diagnostic {
    /// The same advisory with `prefix` in front of its message, for a caller
    /// that reports a loader's advisory under its own heading.
    pub fn prefixed(self, prefix: &str) -> Self {
        Self {
            message: format!("{prefix}{}", self.message),
            ..self
        }
    }
}

/// Count `diagnostics` per group; groups with none are absent.
pub fn summarize(diagnostics: &[Diagnostic]) -> BTreeMap<DiagnosticGroup, usize> {
    let mut counts = BTreeMap::new();
    for d in diagnostics {
        *counts.entry(d.group).or_insert(0) += 1;
    }
    counts
}

/// Diagnostics a graph remembers about its own build.
const MAX_RECORDED: usize = 100;

/// A [`Diagnostic`] as read back from a saved graph, where the kind is data
/// rather than a compiled-in code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedDiagnostic {
    pub group: DiagnosticGroup,
    pub kind: String,
    pub message: String,
}

/// What a graph remembers about the blueprint build that produced it: the
/// counts per group in full, and the most severe [`MAX_RECORDED`] advisories.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub summary: BTreeMap<DiagnosticGroup, usize>,
    pub diagnostics: Vec<RecordedDiagnostic>,
}

impl BuildInfo {
    /// The record of a build that raised `diagnostics`. A clean build records
    /// an empty one, which is how a graph built without advisories differs
    /// from one that was not blueprint-built at all.
    pub fn record(diagnostics: &[Diagnostic]) -> Self {
        let mut ordered: Vec<&Diagnostic> = diagnostics.iter().collect();
        ordered.sort_by_key(|d| d.group);
        Self {
            summary: summarize(diagnostics),
            diagnostics: ordered
                .into_iter()
                .take(MAX_RECORDED)
                .map(|d| RecordedDiagnostic {
                    group: d.group,
                    kind: d.kind.to_string(),
                    message: d.message.clone(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(group: DiagnosticGroup, kind: &'static str) -> Diagnostic {
        Diagnostic::new(group, kind, format!("{kind} message"))
    }

    #[test]
    fn groups_order_by_severity() {
        let mut shuffled = vec![
            DiagnosticGroup::Cosmetic,
            DiagnosticGroup::Declarations,
            DiagnosticGroup::DataQuality,
            DiagnosticGroup::Stubs,
            DiagnosticGroup::DataShape,
        ];
        shuffled.sort();
        assert_eq!(shuffled, DiagnosticGroup::ALL);
    }

    #[test]
    fn strict_settings_resolve_to_groups() {
        assert!(StrictSetting::Flag(false).groups().unwrap().is_empty());
        assert_eq!(
            StrictSetting::Flag(true).groups().unwrap(),
            STRICT_DEFAULT_GROUPS
        );
        let named = StrictSetting::Groups(vec!["data_quality".into()]);
        assert_eq!(named.groups().unwrap(), [DiagnosticGroup::DataQuality]);
        let err = StrictSetting::Groups(vec!["stub".into()])
            .groups()
            .unwrap_err();
        assert!(
            err.contains("'stub'") && err.contains("data_shape"),
            "{err}"
        );
    }

    #[test]
    fn strict_failure_names_only_the_failing_groups() {
        let all = [
            d(DiagnosticGroup::Stubs, "s"),
            d(DiagnosticGroup::DataQuality, "q"),
        ];
        assert!(strict_failure(&[DiagnosticGroup::Declarations], &all).is_none());
        let text = strict_failure(&STRICT_DEFAULT_GROUPS, &all).unwrap();
        assert!(
            text.contains("[stubs] 1") && text.contains("s message"),
            "{text}"
        );
        assert!(!text.contains("data_quality"), "{text}");
    }

    #[test]
    fn strict_failure_ignores_informational_kinds_in_any_group_list() {
        let note = Diagnostic::new(DiagnosticGroup::Declarations, KIND_DEFAULT_TODAY, "note");
        let real = d(DiagnosticGroup::Declarations, "real");
        assert!(strict_failure(&DiagnosticGroup::ALL, std::slice::from_ref(&note)).is_none());
        let text = strict_failure(&STRICT_DEFAULT_GROUPS, &[note, real]).unwrap();
        assert!(
            text.contains("[declarations] 1") && !text.contains("note"),
            "{text}"
        );
    }

    #[test]
    fn summary_counts_only_present_groups() {
        let all = [
            d(DiagnosticGroup::DataQuality, "a"),
            d(DiagnosticGroup::DataQuality, "b"),
            d(DiagnosticGroup::Declarations, "c"),
        ];
        let summary = summarize(&all);
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[&DiagnosticGroup::DataQuality], 2);
        assert_eq!(summary[&DiagnosticGroup::Declarations], 1);
    }

    #[test]
    fn record_keeps_the_most_severe_first_and_caps() {
        assert_eq!(BuildInfo::record(&[]), BuildInfo::default());
        let mut all: Vec<Diagnostic> = (0..MAX_RECORDED + 20)
            .map(|_| d(DiagnosticGroup::Cosmetic, "note"))
            .collect();
        all.push(d(DiagnosticGroup::Declarations, "late"));
        let info = BuildInfo::record(&all);
        assert_eq!(info.diagnostics.len(), MAX_RECORDED);
        assert_eq!(info.diagnostics[0].kind, "late");
        assert_eq!(info.summary[&DiagnosticGroup::Cosmetic], MAX_RECORDED + 20);
        let json = serde_json::to_string(&info).unwrap();
        let back: BuildInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }
}
