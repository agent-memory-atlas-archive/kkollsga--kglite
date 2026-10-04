//! The edge from a spec's rows to their parent node: which one the build
//! writes, and the rows an implicit one must not write.
//!
//! Three readers share one answer — the buffered FK loader, the streamed FK
//! loader and the valid-time grouping of a sub-node's versions — so a version
//! group always names the edge the build actually wrote.

use super::super::schema::FkEdge;
use super::specs::FlatSpec;
use crate::datatypes::values::{ColumnData, ColumnType, DataFrame, Value};
use crate::graph::diagnostics::{Diagnostic, DiagnosticGroup};
use crate::graph::schema::DirGraph;
use crate::graph::storage::lookups::EndpointResolver;

/// How a spec carrying `parent_fk` reaches its parent.
pub(super) enum ParentLink {
    /// Generated `OF_<PARENT>` edge from `parent_fk` to the parent's pk.
    Implicit {
        edge_type: String,
        edge: Box<FkEdge>,
    },
    /// The spec declared an `fk_edges` entry to the parent type, so nothing is
    /// generated; the version grouping follows this edge.
    Explicit(String),
    /// The parent's pk is `"auto"`: its ids are row numbers no column value
    /// can name, so no edge is generated.
    AutoPkParent {
        parent_type: String,
        parent_fk: String,
    },
}

impl ParentLink {
    /// The edge type the build writes for the parent link, if any.
    pub(super) fn written_edge(&self) -> Option<&str> {
        match self {
            Self::Implicit { edge_type, .. } | Self::Explicit(edge_type) => Some(edge_type),
            Self::AutoPkParent { .. } => None,
        }
    }
}

/// The parent link of `spec`: its own `parent` key, else the enclosing type of
/// a sub-node, reached through `parent_fk`. `None` without a `parent_fk` or a
/// parent. An `fk_edges` entry to the parent type (a junction does not count)
/// stands in for the implicit edge, preferring the one on `parent_fk`.
pub(super) fn parent_link(spec: &FlatSpec) -> Option<ParentLink> {
    let parent_fk = spec.spec.parent_fk.as_ref()?;
    let parent_type = spec.spec.parent.as_ref().or(spec.parent.as_ref())?;
    let to_parent = || {
        spec.spec
            .connections
            .fk_edges
            .iter()
            .filter(|(_, edge)| &edge.target == parent_type)
    };
    if let Some((name, _)) = to_parent()
        .find(|(_, edge)| &edge.fk == parent_fk)
        .or_else(|| to_parent().next())
    {
        return Some(ParentLink::Explicit(name.clone()));
    }
    if spec.parent_pk_auto {
        return Some(ParentLink::AutoPkParent {
            parent_type: parent_type.clone(),
            parent_fk: parent_fk.clone(),
        });
    }
    Some(ParentLink::Implicit {
        edge_type: implicit_edge_name(parent_type),
        edge: Box::new(FkEdge::plain(parent_type.clone(), parent_fk.clone())),
    })
}

/// `OF_` + the parent type split into words: `ProjectPhase` → `OF_PROJECT_PHASE`.
pub(super) fn implicit_edge_name(parent_type: &str) -> String {
    format!("OF_{}", upper_snake(parent_type))
}

/// `CamelCase` → `UPPER_SNAKE`. A word starts at an uppercase letter that
/// follows a lowercase letter or digit (`ProjectPhase`, `Team2Lead`) and at
/// the last capital of an acronym run followed by lowercase (`HRReview` →
/// `HR_REVIEW`); digits and trailing capitals stay with their word
/// (`TeamV2` → `TEAM_V2`, `TeamHQ` → `TEAM_HQ`).
fn upper_snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push('_');
            }
        }
        out.extend(c.to_uppercase());
    }
    out
}

/// Implicit-edge rows whose `parent_fk` value names no parent node.
#[derive(Default)]
pub(super) struct UnresolvedParents {
    count: usize,
    example: Option<Value>,
}

impl UnresolvedParents {
    /// The one warning for a spec, `None` when every row resolved.
    pub(super) fn diagnostic(
        &self,
        node_type: &str,
        parent_type: &str,
        (edge_type, parent_fk): (&str, &str),
    ) -> Option<Diagnostic> {
        let example = self.example.as_ref()?;
        Some(Diagnostic::new(
            DiagnosticGroup::Stubs,
            "parent_unresolved",
            format!(
                "[{node_type}] parent_fk '{parent_fk}' matched no '{parent_type}' pk for {} \
                 row(s) (e.g. {example}); those rows get no {edge_type} edge and no stub \
                 parent. parent_fk must hold the parent's pk — declare an fk_edges entry to \
                 '{parent_type}' to link by another column.",
                self.count
            ),
        ))
    }
}

/// `df` (source id column, parent id column) without the rows whose parent id
/// is not a `parent_type` node; the dropped rows are tallied. The implicit
/// edge never vivifies a parent: a stub of a declared valid-time type is valid
/// at every instant.
pub(super) fn drop_unresolved_parents(
    graph: &DirGraph,
    df: DataFrame,
    types: (&str, &str),
    columns: (&str, &str),
    tally: &mut UnresolvedParents,
) -> Result<DataFrame, String> {
    let (source_type, parent_type) = types;
    let (src_col, tgt_col) = columns;
    let resolver = EndpointResolver::new(
        &graph.id_indices,
        &graph.graph,
        source_type.to_string(),
        parent_type.to_string(),
    )
    .ok();
    let rows = df.row_count();
    let cell = |col: &str, r: usize| df.get_value(r, col).filter(|v| *v != Value::Null);
    let keep: Vec<usize> = (0..rows)
        .filter(|&r| match cell(tgt_col, r) {
            None => true,
            Some(id) => {
                let found = resolver
                    .as_ref()
                    .is_some_and(|res| res.check_target(&id).is_some());
                if !found {
                    tally.count += 1;
                    tally.example.get_or_insert(id);
                }
                found
            }
        })
        .collect();
    if keep.len() == rows {
        return Ok(df);
    }
    let mut out = DataFrame::new(Vec::new());
    for name in [src_col, tgt_col] {
        let ty = df.get_column_type(name).unwrap_or(ColumnType::String);
        let values = keep.iter().map(|&r| cell(name, r));
        let data = if ty == ColumnType::Int64 {
            ColumnData::Int64(
                values
                    .map(|v| match v {
                        Some(Value::Int64(i)) => Some(i),
                        _ => None,
                    })
                    .collect(),
            )
        } else {
            ColumnData::String(values.map(|v| v.and_then(|v| v.as_string())).collect())
        };
        out.add_column(name.to_string(), ty, data)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::implicit_edge_name;

    #[test]
    fn the_implicit_name_splits_the_parent_type_into_words() {
        for (parent, edge) in [
            ("Team", "OF_TEAM"),
            ("ProjectPhase", "OF_PROJECT_PHASE"),
            ("TeamHQ", "OF_TEAM_HQ"),
            ("TeamV2", "OF_TEAM_V2"),
            ("Team2Lead", "OF_TEAM2_LEAD"),
            ("HRReview", "OF_HR_REVIEW"),
            ("Org_Unit", "OF_ORG_UNIT"),
            ("HR", "OF_HR"),
        ] {
            assert_eq!(implicit_edge_name(parent), edge, "{parent}");
        }
    }
}
