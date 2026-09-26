//! Mandatory lowering of a statement's `FOR <axis> AS OF <instant>` context.
//!
//! Lowering runs on every optimize, after `fold_constant_inline_maps` and
//! outside `PASSES`, so `disabled_passes` can never change what a context
//! means. It walks every scope itself — the top level, each UNION arm and
//! each `CALL { }` body — because the recursion inside `PASSES`
//! (`optimize_nested_queries`) can be disabled. Each scope gets its own
//! [`GuardTemplate`]: the declared targets its patterns can reach. The
//! instant is not in the plan; it is evaluated once per execution.
//!
//! A statement lowering refuses keeps its context with the refusal on it,
//! raised before execution and before EXPLAIN renders a plan (see
//! [`check_executable`]).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::Datelike;

use super::ast::{CallClause, Clause, CypherQuery, Expression, StatementContext};
use super::executor::{is_context_free_procedure, is_mutation_query, CypherExecutor};
use super::parameter_presence::{walk_query, AstSink};
use super::result::ResultRow;
use super::tokenizer::{tokenize_cypher_with_positions, CypherToken};
use crate::datatypes::values::Value;
use crate::graph::core::graph_filter::{
    EdgeGuard, GraphFilter, GuardBounds, GuardTemplate, NodeGuard, ValidTimeSelector,
};
use crate::graph::core::pattern_matching::{Pattern, PatternElement};
use crate::graph::features::temporal::{self, eval, TemporalTarget};
use crate::graph::schema::{DirGraph, InternedKey};

/// The one axis that lowers.
const VALID_TIME: &str = "VALID_TIME";

/// The refusal every execution under a context meets in this build.
pub(crate) const NOT_EXECUTABLE_YET: &str =
    "valid-time contexts are not executable yet in this build: FOR VALID_TIME AS OF \
     parses, lowers and renders under EXPLAIN, but a statement carrying one does not run";

/// Compile `query`'s context into a guard template per scope, or record why
/// the statement is refused. Without a context this returns on its first
/// line; nested scopes re-enter with none, so a context lowers once.
pub(crate) fn lower(query: &mut CypherQuery, graph: &DirGraph) {
    if query.context.is_none() {
        return;
    }
    lower_context(query, graph);
}

#[cold]
#[inline(never)]
fn lower_context(query: &mut CypherQuery, graph: &DirGraph) {
    let declarations = temporal::declared(graph);
    let refusal = statement_refusal(query, declarations.is_empty())
        .or_else(|| attach_templates(query, graph, &declarations).err());
    if refusal.is_some() {
        clear_templates(query);
    }
    if let Some(context) = query.context.as_mut() {
        context.refusal = refusal;
    }
}

/// Refusals that hold for the whole statement, whatever its scopes reach.
fn statement_refusal(query: &CypherQuery, no_declarations: bool) -> Option<String> {
    let axis = &query.context.as_ref()?.axis;
    if !axis.eq_ignore_ascii_case(VALID_TIME) {
        return Some(format!(
            "axis {axis} is not supported on this graph; the supported axis is {VALID_TIME}"
        ));
    }
    if no_declarations {
        return Some(
            "FOR VALID_TIME AS OF needs a validity declaration, and this graph has none; \
             declare one with CALL db.temporal.declare(...)"
                .to_string(),
        );
    }
    if is_mutation_query(query) {
        return Some(
            "a statement under FOR VALID_TIME AS OF cannot write; run the write without \
             the context"
                .to_string(),
        );
    }
    None
}

/// Give `query` and every nested scope its template, or the first refusal a
/// scope earns.
fn attach_templates(
    query: &mut CypherQuery,
    graph: &DirGraph,
    declarations: &[temporal::DeclarationInfo],
) -> Result<(), String> {
    let mut reach = Reach::default();
    walk_query(query, &mut reach);
    if let Some(name) = reach.refused_function {
        return Err(format!(
            "{name}() counts or walks a node's relationships outside the pattern matcher, \
             so it is not available under a valid-time context yet"
        ));
    }
    if let Some(name) = reach.refused_procedure {
        return Err(format!(
            "procedure {name} enumerates graph elements and is not valid-time aware, so it \
             cannot run under FOR VALID_TIME AS OF; metadata procedures such as db.labels \
             and db.temporal.declarations can"
        ));
    }
    query.guard = Some(Arc::new(reach.template(graph, declarations)?));
    for clause in &mut query.clauses {
        match clause {
            Clause::Union(arm) => attach_templates(&mut arm.query, graph, declarations)?,
            Clause::CallSubquery { body, .. } => attach_templates(body, graph, declarations)?,
            _ => {}
        }
    }
    Ok(())
}

fn clear_templates(query: &mut CypherQuery) {
    query.guard = None;
    for clause in &mut query.clauses {
        match clause {
            Clause::Union(arm) => clear_templates(&mut arm.query),
            Clause::CallSubquery { body, .. } => clear_templates(body),
            _ => {}
        }
    }
}

/// What one scope's read patterns and procedure calls can reach.
#[derive(Default)]
struct Reach {
    /// A node pattern with no label (or a label bound from a parameter), or
    /// the intermediate nodes of a multi-hop segment.
    any_node: bool,
    labels: BTreeSet<String>,
    /// A relationship pattern with no type (or a type from a parameter).
    any_rel: bool,
    rel_types: BTreeSet<String>,
    refused_procedure: Option<String>,
    /// A scalar function that reads relationships without the matcher.
    refused_function: Option<String>,
}

/// Scalar functions that read a node's relationships directly — every
/// incident edge, or a search of their own — so a guard in the matcher
/// cannot see them.
const TOPOLOGY_FUNCTIONS: &[&str] = &["degree", "indegree", "outdegree", "shortest_path_length"];

impl AstSink for Reach {
    fn parameter(&mut self, _name: &str) {}

    fn read_pattern(&mut self, pattern: &Pattern) {
        for element in &pattern.elements {
            match element {
                PatternElement::Node(node) => {
                    let labels = node.label_alternatives().iter().chain(&node.extra_labels);
                    self.labels.extend(labels.cloned());
                    let labelled =
                        !node.label_alternatives().is_empty() || !node.extra_labels.is_empty();
                    if !labelled || !node.label_params.is_empty() {
                        self.any_node = true;
                    }
                }
                PatternElement::Edge(edge) => {
                    // A segment of two or more hops passes through anonymous,
                    // untyped nodes, which can carry any declared label.
                    if edge.var_length.is_some_and(|(_, max)| max > 1) {
                        self.any_node = true;
                    }
                    let types = match (&edge.connection_types, &edge.connection_type) {
                        (Some(types), _) => types.as_slice(),
                        (None, Some(single)) => std::slice::from_ref(single),
                        (None, None) => &[],
                    };
                    if types.is_empty() || !edge.type_params.is_empty() {
                        self.any_rel = true;
                    }
                    self.rel_types.extend(types.iter().cloned());
                }
            }
        }
    }

    fn procedure(&mut self, call: &CallClause) {
        if self.refused_procedure.is_none() && !is_context_free_procedure(&call.procedure_name) {
            self.refused_procedure = Some(call.procedure_name.clone());
        }
    }

    fn function(&mut self, name: &str) {
        if self.refused_function.is_none()
            && TOPOLOGY_FUNCTIONS
                .iter()
                .any(|f| f.eq_ignore_ascii_case(name))
        {
            self.refused_function = Some(name.to_string());
        }
    }

    fn nested_scopes(&self) -> bool {
        false
    }
}

impl Reach {
    /// The declared targets this scope reaches, in lookup order. A node
    /// declaration on label L governs every node carrying L, primary or
    /// secondary, so once the graph has secondary labels a labelled pattern
    /// can reach any declared label.
    fn template(
        &self,
        graph: &DirGraph,
        declarations: &[temporal::DeclarationInfo],
    ) -> Result<GuardTemplate, String> {
        let all_nodes = self.any_node || (graph.has_secondary_labels && !self.labels.is_empty());
        let mut template = GuardTemplate::default();
        for info in declarations {
            match &info.target {
                TemporalTarget::Node(label) => {
                    if all_nodes || self.labels.contains(label) {
                        template.nodes.push(NodeGuard {
                            label: label.clone(),
                            bounds: GuardBounds::of(&info.config),
                        });
                    }
                }
                TemporalTarget::Relationship {
                    rel_type,
                    source_type,
                } => {
                    if !(self.any_rel || self.rel_types.contains(rel_type)) {
                        continue;
                    }
                    if info.ambiguous {
                        return Err(format!(
                            "relationship type '{rel_type}' holds several declarations with no \
                             source type, so which one applies depends on declaration order; \
                             re-declare them per source_type before querying it under \
                             FOR VALID_TIME AS OF"
                        ));
                    }
                    template.edges.push(EdgeGuard {
                        rel_type: rel_type.clone(),
                        rel_key: InternedKey::from_str(rel_type),
                        source_type: source_type.clone(),
                        source_type_key: source_type.as_deref().map(InternedKey::from_str),
                        bounds: GuardBounds::of(&info.config),
                    });
                }
            }
        }
        Ok(template)
    }
}

/// Raise what stops `query` from running. `rendering_only` is true where
/// the caller renders an EXPLAIN plan and executes nothing: a lowering
/// refusal still stops it, the build's "not yet" does not.
///
/// Otherwise the instant is resolved first — once per execution, from the
/// caller's parameters — so a bad instant names itself before the
/// statement meets [`NOT_EXECUTABLE_YET`].
pub(crate) fn check_executable(
    query: &CypherQuery,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
    rendering_only: bool,
) -> Result<(), String> {
    let Some(context) = &query.context else {
        return Ok(());
    };
    refuse_context(query, context, graph, params, rendering_only)
}

#[cold]
#[inline(never)]
fn refuse_context(
    query: &CypherQuery,
    context: &StatementContext,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
    rendering_only: bool,
) -> Result<(), String> {
    if let Some(refusal) = &context.refusal {
        return Err(refusal.clone());
    }
    if rendering_only {
        return Ok(());
    }
    // Resolved against the endpoint index as execution will resolve it, so
    // the index is built and exercised on every storage mode before the
    // refusal below.
    let _resolved = resolve_filter(query, context, graph, params)?.resolve(graph);
    Err(NOT_EXECUTABLE_YET.to_string())
}

/// Join the top scope's template with the instant this execution resolves.
fn resolve_filter(
    query: &CypherQuery,
    context: &StatementContext,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
) -> Result<GraphFilter, String> {
    let template = query.guard.clone().unwrap_or_default();
    let value = CypherExecutor::with_params(graph, params, None)
        .evaluate_expression(&context.instant, &ResultRow::new())?;
    let instant =
        eval::parse_instant(&value).map_err(|err| format!("FOR VALID_TIME AS OF: {err}"))?;
    Ok(GraphFilter {
        template,
        selector: ValidTimeSelector::AsOf(instant),
    })
}

/// The context instant when the statement spells it as a constant — a
/// literal, or `date(…)` / `datetime(…)` of one — so planning can estimate
/// with the counts valid at it. A `$param` or `date()` (today) is known only
/// per execution, and a refused context runs nothing; neither has one, and
/// the plan keeps each label's full count.
pub(crate) fn plan_instant(query: &CypherQuery, graph: &DirGraph) -> Option<eval::Instant> {
    let context = query.context.as_ref()?;
    let constant = match &context.instant {
        Expression::Literal(_) => true,
        Expression::FunctionCall { name, args, .. } => {
            matches!(args.as_slice(), [Expression::Literal(_)])
                && (name.eq_ignore_ascii_case("date") || name.eq_ignore_ascii_case("datetime"))
        }
        _ => false,
    };
    if context.refusal.is_some() || !constant {
        return None;
    }
    let value = CypherExecutor::with_params(graph, &HashMap::new(), None)
        .evaluate_expression(&context.instant, &ResultRow::new())
        .ok()?;
    eval::parse_instant(&value).ok()
}

/// Nodes of each declared label in this scope's template valid at
/// `instant`, from the endpoint index — the planner's start-node estimate
/// under a context. `None` without a template, an instant, or any indexed
/// label.
pub(crate) fn valid_counts(
    query: &CypherQuery,
    graph: &DirGraph,
    instant: Option<eval::Instant>,
) -> Option<HashMap<String, usize>> {
    let (guard, instant) = (query.guard.as_ref()?, instant?);
    let counts: HashMap<String, usize> = guard
        .nodes
        .iter()
        .filter_map(|node| {
            let count = temporal::endpoint_index::node_count_at(graph, &node.label, instant)?;
            Some((node.label.clone(), count))
        })
        .collect();
    (!counts.is_empty()).then_some(counts)
}

/// `query` under a `FOR VALID_TIME AS OF` context at `instant` — what a
/// binding's `valid_at=` sends. The instant is written as a literal
/// (`date('…')` for a date, `datetime('…')` for a datetime or an ISO string
/// with a time) so the statement text, not a parameter, carries it. A query
/// that already has a context is refused naming both, since a statement takes
/// one; `EXPLAIN` / `PROFILE` may follow the prefix, so they need no special
/// handling.
pub fn prepend_valid_time(query: &str, instant: &Value) -> Result<String, String> {
    // chrono writes a negative year as `-0005-…`, which the ISO date parser
    // behind `date('…')` does not read back.
    let year = match instant {
        Value::DateTime(date) => Some(date.year()),
        Value::Timestamp(ts) => Some(ts.year()),
        _ => None,
    };
    if let Some(year) = year.filter(|year| *year < 0) {
        return Err(format!(
            "valid_at: year {year} is before year 0, which a FOR VALID_TIME AS OF \
             literal cannot spell"
        ));
    }
    let literal = match (instant, eval::parse_instant(instant)) {
        (Value::DateTime(date), _) => format!("date('{}')", date.format("%Y-%m-%d")),
        (Value::Timestamp(ts), _) => {
            format!("datetime('{}')", ts.format("%Y-%m-%dT%H:%M:%S%.f"))
        }
        (Value::String(text), Ok(eval::Instant::Date(_))) => format!("date('{text}')"),
        (Value::String(text), Ok(eval::Instant::Timestamp(_))) => format!("datetime('{text}')"),
        (_, Err(err)) => return Err(format!("valid_at: {err}")),
        (other, Ok(_)) => unreachable!("parse_instant accepted {other:?}"),
    };
    if carries_context(query) {
        return Err(format!(
            "the query already has a FOR … AS OF context and valid_at= adds another \
             (FOR {VALID_TIME} AS OF {literal}); a statement takes one context, so drop \
             one of them"
        ));
    }
    Ok(format!("FOR {VALID_TIME} AS OF {literal} {query}"))
}

/// Whether `query`'s prefixes include `FOR` — found through the tokenizer,
/// so a comment or string that mentions it does not count. A query that does
/// not tokenize is left for the parser to report.
fn carries_context(query: &str) -> bool {
    let Ok(tokenized) = tokenize_cypher_with_positions(query) else {
        return false;
    };
    tokenized
        .tokens
        .iter()
        .map(|(token, _)| token)
        .find(|t| !matches!(t, CypherToken::Explain | CypherToken::Profile))
        .is_some_and(|t| matches!(t, CypherToken::Identifier(w) if w.eq_ignore_ascii_case("FOR")))
}

#[cfg(test)]
#[path = "valid_time_tests.rs"]
mod tests;
