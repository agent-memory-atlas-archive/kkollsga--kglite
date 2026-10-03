//! Mandatory lowering of a statement's `FOR <axis> AS OF <instant>` context.
//!
//! Lowering runs on every optimize, outside `PASSES`, so `disabled_passes`
//! can never change what a context means. A statement with a context skips
//! `fold_constant_inline_maps`, so lowering sees every inline-map value as
//! written. It walks every scope itself — the top level, each UNION arm and
//! each `CALL { }` body — because the recursion inside `PASSES`
//! (`optimize_nested_queries`) can be disabled. Each scope gets its own
//! [`GuardTemplate`]: the declared targets its patterns can reach. The
//! instant is not in the plan; it is evaluated once per execution.
//!
//! A statement lowering refuses keeps its context with the refusal on it,
//! raised before execution and before EXPLAIN renders a plan (see
//! [`check_executable`]). One that lowers executes under the filter
//! [`execution_filter`] resolves.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Datelike;

use super::ast::{
    CallClause, Clause, ContextInstant, ContextOrigin, CypherQuery, Expression, StatementContext,
};
use super::executor::{
    is_context_free_procedure, is_mask_routed_procedure, is_mutation_query,
    is_view_routed_procedure, CypherExecutor,
};
use super::parameter_presence::{walk_query, AstSink};
use super::result::{ResultRow, TemporalDiagnostics};
use super::tokenizer::{tokenize_cypher_with_positions, CypherToken};
use crate::datatypes::values::Value;
use crate::graph::core::graph_filter::{
    EdgeGuard, ElementFilter, GraphFilter, GuardTemplate, TemplateScope, ValidTimeSelector,
};
use crate::graph::core::pattern_matching::{Pattern, PatternElement};
use crate::graph::features::temporal::declarations::TemporalTarget;
use crate::graph::features::temporal::{self, eval, ValidTimeDefault};
use crate::graph::schema::DirGraph;

/// The one axis that lowers.
const VALID_TIME: &str = "VALID_TIME";

/// What a refused template names a statement's context as.
const CONTEXT_SURFACE: &str = "FOR VALID_TIME AS OF";

/// The same, for the context lowering adds: the user wrote none, so the
/// refusal also names the way to read every version.
const DEFAULT_SURFACE: &str =
    "the default valid-time context (or prefix the statement with FOR VALID_TIME ALL)";

/// The one context-free statement the default leaves alone, as the reason the
/// echo gives: it writes, calls a procedure that is not valid-time aware, or
/// names a valid-time function itself.
const SKIP_WRITE: &str = "write";
const SKIP_PROCEDURE: &str = "procedure";
const SKIP_VALID_AT: &str = "valid_at";

/// Give a statement that spells no context the one the graph's declarations
/// imply: the graph's [`ValidTimeDefault`] — as of today (UTC) unless a
/// runtime or manifest setting says `all` or a fixed day — resolved per
/// execution so a cached plan never freezes the date. A graph with no
/// declaration is left alone (one field test). Under `today` or a fixed day,
/// statements the default must not govern get an `ALL` context whose origin
/// records why, so the echo can say so:
/// - statements that write (their reads see every version),
/// - procedures that are neither metadata nor routed (`refresh_stats`,
///   `duplicate_id`, the `*_violation` audits, ...), which an explicit prefix
///   refuses,
/// - statements calling `valid_at()` / `valid_during()`, which set their own
///   instants.
///
/// Under `all` nothing is skipped: every statement already reads every
/// version.
///
/// Top level only: UNION arms and `CALL { }` bodies are lowered under the
/// statement's context.
pub(crate) fn apply_default(query: &mut CypherQuery, graph: &DirGraph) {
    if query.context.is_some() || query.suppress_default || graph.temporal.is_empty() {
        return;
    }
    let (instant, origin) = match graph.valid_time_default {
        ValidTimeDefault::All => (ContextInstant::All, ContextOrigin::Default),
        configured => match default_skip_reason(query) {
            Some(reason) => (ContextInstant::All, ContextOrigin::Skipped(reason)),
            None => (
                ContextInstant::AsOf(default_instant_expression(configured)),
                ContextOrigin::Default,
            ),
        },
    };
    query.context = Some(StatementContext {
        axis: VALID_TIME.to_string(),
        instant,
        origin,
        refusal: None,
        body_start: 0,
    });
}

/// `date()` for `today`, `date('YYYY-MM-DD')` for a fixed day.
fn default_instant_expression(default: ValidTimeDefault) -> Expression {
    let args = match default {
        ValidTimeDefault::Date(day) => vec![Expression::Literal(Value::String(
            day.format("%Y-%m-%d").to_string(),
        ))],
        _ => Vec::new(),
    };
    Expression::FunctionCall {
        name: "date".to_string(),
        args,
        distinct: false,
    }
}

#[cold]
#[inline(never)]
fn default_skip_reason(query: &CypherQuery) -> Option<&'static str> {
    if is_mutation_query(query) {
        return Some(SKIP_WRITE);
    }
    let mut finds = SkipScan::default();
    walk_query(query, &mut finds);
    if finds.valid_at_function {
        Some(SKIP_VALID_AT)
    } else if finds.unaware_procedure {
        Some(SKIP_PROCEDURE)
    } else {
        None
    }
}

#[derive(Default)]
struct SkipScan {
    valid_at_function: bool,
    unaware_procedure: bool,
}

impl AstSink for SkipScan {
    fn parameter(&mut self, _name: &str) {}

    fn function(&mut self, name: &str) {
        if name.eq_ignore_ascii_case("valid_at") || name.eq_ignore_ascii_case("valid_during") {
            self.valid_at_function = true;
        }
    }

    fn procedure(&mut self, call: &CallClause) {
        let lowered = call.procedure_name.to_ascii_lowercase();
        let name = lowered.strip_prefix("kglite.").unwrap_or(&lowered);
        if !(is_context_free_procedure(name)
            || is_view_routed_procedure(name)
            || is_mask_routed_procedure(name))
        {
            self.unaware_procedure = true;
        }
    }
}

/// Compile `query`'s context into a guard template per scope, or record why
/// the statement is refused. Without a context this returns on its first
/// line; nested scopes re-enter with none, so a context lowers once.
pub(crate) fn lower(query: &mut CypherQuery, graph: &DirGraph) {
    if query.context.is_none() {
        return;
    }
    lower_context(query, graph);
}

/// Whether `context` asks for every version (`FOR VALID_TIME ALL`, or a
/// statement the default skipped): no templates, no filter.
fn is_all(context: &StatementContext) -> bool {
    matches!(context.instant, ContextInstant::All)
}

/// Whether `query` runs under a filter at some instant — a context that is
/// not `ALL`. The planner keeps the statement's inline-map expressions for it.
pub(crate) fn has_instant_context(query: &CypherQuery) -> bool {
    query.context.as_ref().is_some_and(|c| !is_all(c))
}

#[cold]
#[inline(never)]
fn lower_context(query: &mut CypherQuery, graph: &DirGraph) {
    let declarations = temporal::declared(graph);
    let refusal = statement_refusal(query, declarations.is_empty()).or_else(|| {
        let context = query.context.as_ref()?;
        let origin = context.origin.clone();
        if is_all(context) {
            return None;
        }
        attach_templates(query, graph, &declarations, &origin).err()
    });
    if refusal.is_some() {
        clear_templates(query);
    }
    if let Some(context) = query.context.as_mut() {
        context.refusal = refusal;
    }
}

/// Refusals that hold for the whole statement, whatever its scopes reach.
fn statement_refusal(query: &CypherQuery, no_declarations: bool) -> Option<String> {
    let context = query.context.as_ref()?;
    let axis = &context.axis;
    if !axis.eq_ignore_ascii_case(VALID_TIME) {
        return Some(format!(
            "axis {axis} is not supported on this graph; the supported axis is {VALID_TIME}"
        ));
    }
    if is_all(query.context.as_ref()?) {
        return None;
    }
    if no_declarations {
        return Some(NO_DECLARATION.to_string());
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
    origin: &ContextOrigin,
) -> Result<(), String> {
    let mut reach = Reach::default();
    walk_query(query, &mut reach);
    if let Some(name) = reach.refused_function {
        return Err(refused_function_message(&name, origin));
    }
    if let Some(name) = reach.refused_procedure {
        return Err(format!(
            "procedure {name} enumerates graph elements and is not valid-time aware, so it \
             cannot run under FOR VALID_TIME AS OF; metadata procedures such as db.labels \
             and db.temporal.declarations can, and so can the graph algorithms (pagerank, \
             louvain, connected_components, …) and the embedding queries"
        ));
    }
    let surface = match origin {
        ContextOrigin::Default => DEFAULT_SURFACE,
        _ => CONTEXT_SURFACE,
    };
    let template = GuardTemplate::for_scope(graph, declarations, &reach.scope, surface)?;
    query.guard = Some(Arc::new(template));
    for clause in &mut query.clauses {
        match clause {
            Clause::Union(arm) => attach_templates(&mut arm.query, graph, declarations, origin)?,
            Clause::CallSubquery { body, .. } => {
                attach_templates(body, graph, declarations, origin)?
            }
            _ => {}
        }
    }
    Ok(())
}

/// The refusal for a scalar function that reads relationships outside the
/// matcher. Under the default the user never wrote a context, so the message
/// names both ways out.
fn refused_function_message(name: &str, origin: &ContextOrigin) -> String {
    match origin {
        ContextOrigin::Default => format!(
            "{name}() counts or walks a node's relationships outside the pattern matcher, \
             so it cannot run under the default valid-time context (valid today); count \
             relationships with COUNT {{ (n)--() }}, which respects the context, or prefix \
             the statement with FOR VALID_TIME ALL to count every version"
        ),
        _ => format!(
            "{name}() counts or walks a node's relationships outside the pattern matcher, \
             so it is not available under a valid-time context; count relationships with \
             COUNT {{ (n)--() }}, or bind MATCH p = shortestPath(...) and read length(p)"
        ),
    }
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
    /// `any_node`: a node pattern with no label (or a label bound from a
    /// parameter), or the intermediate nodes of a multi-hop segment.
    /// `any_rel`: a relationship pattern with no type (or a type from a
    /// parameter).
    scope: TemplateScope,
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
                    self.scope.labels.extend(labels.cloned());
                    let labelled =
                        !node.label_alternatives().is_empty() || !node.extra_labels.is_empty();
                    if !labelled || !node.label_params.is_empty() {
                        self.scope.any_node = true;
                    }
                }
                PatternElement::Edge(edge) => {
                    // A segment of two or more hops passes through anonymous,
                    // untyped nodes, which can carry any declared label.
                    if edge.var_length.is_some_and(|(_, max)| max > 1) {
                        self.scope.any_node = true;
                    }
                    let types = match (&edge.connection_types, &edge.connection_type) {
                        (Some(types), _) => types.as_slice(),
                        (None, Some(single)) => std::slice::from_ref(single),
                        (None, None) => &[],
                    };
                    if types.is_empty() || !edge.type_params.is_empty() {
                        self.scope.any_rel = true;
                    }
                    self.scope.rel_types.extend(types.iter().cloned());
                }
            }
        }
    }

    /// A context-free procedure reaches nothing. A routed one — an
    /// algorithm on the valid slice, an embedding query testing each
    /// candidate — reaches every element, so the scope's template holds every
    /// declared target; whether its slice fits is decided at execution. Any
    /// other procedure is refused.
    fn procedure(&mut self, call: &CallClause) {
        let lowered = call.procedure_name.to_ascii_lowercase();
        let name = lowered.strip_prefix("kglite.").unwrap_or(&lowered);
        if is_context_free_procedure(name) {
            return;
        }
        if is_view_routed_procedure(name) || is_mask_routed_procedure(name) {
            self.scope.any_node = true;
            self.scope.any_rel = true;
            return;
        }
        if self.refused_procedure.is_none() {
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

/// Raise the refusal lowering recorded on `query`'s context, if any. Called
/// before a plan is cached, before EXPLAIN renders it, and at execution.
pub(crate) fn check_executable(query: &CypherQuery) -> Result<(), String> {
    match query.context.as_ref().and_then(|c| c.refusal.as_ref()) {
        Some(refusal) => Err(refusal.clone()),
        None => Ok(()),
    }
}

/// The filter `query` executes under: the template of every scope joined
/// with the instant this execution resolves, from the caller's parameters,
/// against the graph's endpoint indexes. `None` without a context, and when
/// the filter removes nothing at the instant (every target timeless).
pub(crate) fn execution_filter(
    query: &CypherQuery,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
) -> Result<Option<Arc<ElementFilter>>, String> {
    let Some(context) = &query.context else {
        return Ok(None);
    };
    if is_all(context) {
        return Ok(None);
    }
    resolve_execution_filter(query, context, graph, params)
}

#[cold]
#[inline(never)]
fn resolve_execution_filter(
    query: &CypherQuery,
    context: &StatementContext,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
) -> Result<Option<Arc<ElementFilter>>, String> {
    let mut template = GuardTemplate::default();
    merge_scope_templates(query, &mut template);
    let filter = GraphFilter {
        template: Arc::new(template),
        selector: ValidTimeSelector::AsOf(resolve_instant(context, graph, params)?),
    };
    let resolved = filter.resolve(graph);
    Ok(ElementFilter::new(&filter, resolved).map(Arc::new))
}

/// Every scope's template in one: the executor resolves one filter per
/// statement, and a UNION arm or `CALL { }` body may reach targets the top
/// scope does not. Guarding a target a scope cannot reach changes nothing.
fn merge_scope_templates(query: &CypherQuery, into: &mut GuardTemplate) {
    if let Some(guard) = &query.guard {
        for node in &guard.nodes {
            if !into.nodes.iter().any(|n| n.label == node.label) {
                into.nodes.push(node.clone());
            }
        }
        for edge in &guard.edges {
            let same =
                |e: &EdgeGuard| e.rel_type == edge.rel_type && e.source_type == edge.source_type;
            if !into.edges.iter().any(same) {
                into.edges.push(edge.clone());
            }
        }
    }
    for clause in &query.clauses {
        match clause {
            Clause::Union(arm) => merge_scope_templates(&arm.query, into),
            Clause::CallSubquery { body, .. } => merge_scope_templates(body, into),
            _ => {}
        }
    }
}

/// The context's instant, evaluated once per execution. Only an
/// `AS OF` context has one.
fn resolve_instant(
    context: &StatementContext,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
) -> Result<eval::Instant, String> {
    let ContextInstant::AsOf(expression) = &context.instant else {
        return Err("FOR VALID_TIME ALL has no instant".to_string());
    };
    let value = CypherExecutor::with_params(graph, params, None)
        .evaluate_expression(expression, &ResultRow::new())?;
    eval::parse_instant(&value).map_err(|err| format!("FOR VALID_TIME AS OF: {err}"))
}

/// How the echo names where the statement's context came from.
fn echo_source(context: &StatementContext) -> String {
    match (&context.origin, &context.instant) {
        (ContextOrigin::Default, _) => "default".to_string(),
        (ContextOrigin::Skipped(reason), _) => format!("skipped:{reason}"),
        (ContextOrigin::Explicit, ContextInstant::All) => "all".to_string(),
        (ContextOrigin::Explicit, ContextInstant::AsOf(_)) => "explicit".to_string(),
    }
}

/// A relationship target as the echo names it: `[:LICENSEE]` or
/// `[:LICENSEE from :Field]`.
fn target_name(rel_type: &str, source_type: Option<&str>) -> String {
    match source_type {
        Some(source) => format!("[:{rel_type} from :{source}]"),
        None => format!("[:{rel_type}]"),
    }
}

/// The valid-time echo for `query` on this execution — the instant it
/// resolves to and the declared targets its scopes reach — answered by
/// `route`. `None` without a context, for `FOR VALID_TIME ALL` on a graph with
/// no declaration (a no-op there), and when the instant does not resolve
/// (execution reports that itself).
pub(crate) fn temporal_echo(
    query: &CypherQuery,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
    route: &str,
) -> Option<TemporalDiagnostics> {
    let context = query.context.as_ref()?;
    if is_all(context) {
        return all_echo(context, graph);
    }
    let instant = resolve_instant(context, graph, params).ok()?;
    let mut template = GuardTemplate::default();
    merge_scope_templates(query, &mut template);
    let nodes = template.nodes.iter().map(|n| format!("(:{})", n.label));
    let edges = template
        .edges
        .iter()
        .map(|e| target_name(&e.rel_type, e.source_type.as_deref()));
    let counts = temporal::endpoint_index::filtered_counts(graph, &template, instant);
    let hidden = counts
        .hidden
        .into_iter()
        .map(|(target, count)| {
            let name = match target {
                TemporalTarget::Node(label) => format!("(:{label})"),
                TemporalTarget::Relationship {
                    rel_type,
                    source_type,
                } => target_name(&rel_type, source_type.as_deref()),
            };
            (name, count)
        })
        .collect();
    let targets: Vec<String> = nodes.chain(edges).collect();
    // A statement that reaches no declared target is filtered by nothing.
    let route = if targets.is_empty() { "plain" } else { route };
    Some(TemporalDiagnostics {
        axis: context.axis.to_ascii_uppercase(),
        source: echo_source(context),
        instant: match instant {
            eval::Instant::Date(date) => date.format("%Y-%m-%d").to_string(),
            eval::Instant::Timestamp(ts) => ts.format("%Y-%m-%dT%H:%M:%S%.f").to_string(),
        },
        targets,
        hidden,
        endpoint_invalid: counts.endpoint_invalid,
        route: route.to_string(),
        retrieval: None,
        slice: false,
        session_version: graph.version(),
    })
}

/// The echo of a statement that reads every version: `instant` is `all`, and
/// nothing is filtered or counted. A graph with no declaration has no valid
/// time to report.
fn all_echo(context: &StatementContext, graph: &DirGraph) -> Option<TemporalDiagnostics> {
    if graph.temporal.is_empty() {
        return None;
    }
    Some(TemporalDiagnostics {
        axis: context.axis.to_ascii_uppercase(),
        source: echo_source(context),
        instant: "all".to_string(),
        route: "plain".to_string(),
        session_version: graph.version(),
        ..TemporalDiagnostics::default()
    })
}

/// The session's plain-plan exit: `query`'s text without its context prefix
/// (`PROFILE ` kept), when every declared target of the graph is indexed and
/// timeless at the instant this execution resolves — the filter would remove
/// nothing, so the unguarded plan, with every fused route, gives the same
/// rows. All declarations count, not only those the statement names: a label
/// the query never spells can still govern the nodes it reaches. Decided per
/// execution (the instant may be a parameter or today) and never cached.
pub(crate) fn timeless_plain_text(
    text: &str,
    query: &CypherQuery,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
) -> Option<String> {
    let context = query.context.as_ref()?;
    if query.explain || context.refusal.is_some() || is_all(context) {
        return None;
    }
    plain_text_if_timeless(text, query, context, graph, params)
}

#[cold]
#[inline(never)]
fn plain_text_if_timeless(
    text: &str,
    query: &CypherQuery,
    context: &StatementContext,
    graph: &DirGraph,
    params: &HashMap<String, Value>,
) -> Option<String> {
    let instant = resolve_instant(context, graph, params).ok()?;
    let template = declared_template(graph).ok()?;
    if !temporal::endpoint_index::template_timeless_at(graph, &template, instant) {
        return None;
    }
    // A default context has no prefix to strip, and the text keeps its own
    // `PROFILE`.
    if context.origin == ContextOrigin::Default {
        return Some(text.to_string());
    }
    let body_start = text
        .char_indices()
        .nth(context.body_start)
        .map_or(text.len(), |(byte, _)| byte);
    let prefix = if query.profile { "PROFILE " } else { "" };
    Some(format!("{prefix}{}", &text[body_start..]))
}

/// Every declared target of `graph`, as one template: what a scope that can
/// reach any node and any relationship compiles to. Refused when a
/// relationship type holds several unkeyed declarations.
pub(crate) fn declared_template(graph: &DirGraph) -> Result<GuardTemplate, String> {
    let every_target = TemplateScope {
        any_node: true,
        any_rel: true,
        ..TemplateScope::default()
    };
    GuardTemplate::for_scope(
        graph,
        &temporal::declared(graph),
        &every_target,
        CONTEXT_SURFACE,
    )
}

/// The targets a retrieval index's documents are judged by: every declared
/// node label and, for a relationship index, its own type's declarations — a
/// document's validity depends on nothing else, so another relationship
/// type's ambiguous declarations do not refuse it.
pub(crate) fn retrieval_template(
    graph: &DirGraph,
    rel_type: Option<&str>,
) -> Result<GuardTemplate, String> {
    let scope = TemplateScope {
        any_node: true,
        rel_types: rel_type.into_iter().map(str::to_string).collect(),
        ..TemplateScope::default()
    };
    GuardTemplate::for_scope(graph, &temporal::declared(graph), &scope, CONTEXT_SURFACE)
}

/// The refusal for a context on a graph with no validity declaration.
pub(crate) const NO_DECLARATION: &str = "FOR VALID_TIME AS OF needs a validity declaration, \
     and this graph has none; declare one with CALL db.temporal.declare(...)";

/// The context instant when the statement spells it as a constant — a
/// literal, or `date(…)` / `datetime(…)` of one — so planning can estimate
/// with the counts valid at it. A `$param` or `date()` (today) is known only
/// per execution, and a refused context runs nothing; neither has one, and
/// the plan keeps each label's full count.
pub(crate) fn plan_instant(query: &CypherQuery, graph: &DirGraph) -> Option<eval::Instant> {
    let context = query.context.as_ref()?;
    let ContextInstant::AsOf(instant) = &context.instant else {
        return None;
    };
    let constant = match instant {
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
        .evaluate_expression(instant, &ResultRow::new())
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

/// Why [`prepend_valid_time`] or a valid-time view refused a query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrependError {
    /// The instant is not a date, a datetime or an ISO date/datetime string,
    /// or it is one a `FOR VALID_TIME AS OF` literal cannot spell.
    BadInstant(String),
    /// The query already carries a `FOR … AS OF` context and `valid_at=`
    /// would add a second; `literal` is the one `valid_at=` spelled.
    DoubledContext { literal: String },
    /// The query carries a `FOR … AS OF` context, or `valid_at=` asks for
    /// one, on a view that is already as of `literal`.
    ViewAlreadyAsOf { literal: String },
}

impl std::fmt::Display for PrependError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrependError::BadInstant(message) => f.write_str(message),
            PrependError::DoubledContext { literal } => {
                let added = if literal == ALL_LITERAL {
                    format!("FOR {VALID_TIME} ALL")
                } else {
                    format!("FOR {VALID_TIME} AS OF {literal}")
                };
                write!(
                    f,
                    "the query already has a FOR … context and valid_at= adds another \
                     ({added}); a statement takes one context, so drop one of them"
                )
            }
            PrependError::ViewAlreadyAsOf { literal } => write!(
                f,
                "this view is already as of {literal}, and the query asks for another \
                 instant (its own FOR … AS OF context, or valid_at=); drop that, or take \
                 a fresh view (freeze(valid_at=…)) at the other instant"
            ),
        }
    }
}

impl std::error::Error for PrependError {}

/// `instant` as a `FOR VALID_TIME AS OF` literal: `date('…')` for a date,
/// `datetime('…')` for a datetime or an ISO string with a time.
pub(crate) fn instant_literal(instant: &Value) -> Result<String, PrependError> {
    // chrono writes a negative year as `-0005-…`, which the ISO date parser
    // behind `date('…')` does not read back.
    let year = match instant {
        Value::DateTime(date) => Some(date.year()),
        Value::Timestamp(ts) => Some(ts.year()),
        _ => None,
    };
    if let Some(year) = year.filter(|year| *year < 0) {
        return Err(PrependError::BadInstant(format!(
            "valid_at: year {year} is before year 0, which a FOR VALID_TIME AS OF \
             literal cannot spell"
        )));
    }
    match (instant, eval::parse_instant(instant)) {
        (Value::DateTime(date), _) => Ok(format!("date('{}')", date.format("%Y-%m-%d"))),
        (Value::Timestamp(ts), _) => {
            Ok(format!("datetime('{}')", ts.format("%Y-%m-%dT%H:%M:%S%.f")))
        }
        (Value::String(text), Ok(eval::Instant::Date(_))) => Ok(format!("date('{text}')")),
        (Value::String(text), Ok(eval::Instant::Timestamp(_))) => Ok(format!("datetime('{text}')")),
        (_, Err(err)) => Err(PrependError::BadInstant(format!("valid_at: {err}"))),
        (other, Ok(_)) => unreachable!("parse_instant accepted {other:?}"),
    }
}

/// What a binding's `valid_at=` asks for: one instant, or every version.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValidAt<'a> {
    /// `FOR VALID_TIME AS OF <instant>`.
    At(&'a Value),
    /// `FOR VALID_TIME ALL`: no valid-time filtering, whatever the graph's
    /// default says.
    All,
}

impl<'a> ValidAt<'a> {
    /// A `valid_at=` argument as decoded from the binding: the string `all`
    /// (any case) asks for every version — no instant spells that way — and
    /// anything else is an instant.
    pub fn from_value(value: &'a Value) -> Self {
        match value {
            Value::String(text) if text.trim().eq_ignore_ascii_case("all") => ValidAt::All,
            other => ValidAt::At(other),
        }
    }
}

/// `query` under a `FOR VALID_TIME AS OF` context at `valid_at`'s instant,
/// or under `FOR VALID_TIME ALL` — what a binding's `valid_at=` sends. The
/// instant is written as a literal (`date('…')` for a date, `datetime('…')`
/// for a datetime or an ISO string with a time) so the statement text, not a
/// parameter, carries it. A query that already has a context is refused
/// naming both ([`PrependError::DoubledContext`]), since a statement takes
/// one; `EXPLAIN` / `PROFILE` may follow the prefix, so they need no special
/// handling.
pub fn prepend_valid_time(query: &str, valid_at: ValidAt<'_>) -> Result<String, PrependError> {
    let ValidAt::At(instant) = valid_at else {
        if carries_valid_time_context(query) {
            return Err(PrependError::DoubledContext {
                literal: ALL_LITERAL.to_string(),
            });
        }
        return Ok(format!("FOR {VALID_TIME} ALL {query}"));
    };
    let literal = instant_literal(instant)?;
    if carries_valid_time_context(query) {
        return Err(PrependError::DoubledContext { literal });
    }
    Ok(prefixed(&literal, query))
}

/// What a doubled `ALL` prefix names in [`PrependError::DoubledContext`].
const ALL_LITERAL: &str = "ALL";

/// `query` behind the prefix for an already-rendered `literal`.
pub(crate) fn prefixed(literal: &str, query: &str) -> String {
    format!("FOR {VALID_TIME} AS OF {literal} {query}")
}

/// Whether `query`'s prefixes include `FOR` — a `FOR … AS OF` context —
/// found through the tokenizer, so a comment or string that mentions it does
/// not count. A query that does not tokenize is left for the parser to
/// report, and answers `false`.
pub fn carries_valid_time_context(query: &str) -> bool {
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
