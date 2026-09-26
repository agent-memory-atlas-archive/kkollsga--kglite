//! A read-only filter over a graph's elements: which nodes and relationships
//! a query may see. Valid time is its first client — a statement prefixed
//! `FOR VALID_TIME AS OF <instant>` compiles a [`GuardTemplate`] per query
//! scope at plan time, and the instant becomes a [`ValidTimeSelector`] once
//! per execution, so a cached plan never carries an instant.

use std::fmt;
use std::sync::Arc;

use crate::graph::features::temporal::eval::Instant;
use crate::graph::features::temporal::IntervalConvention;
use crate::graph::schema::{InternedKey, TemporalConfig};
use crate::graph::TemporalContext;

/// The declared validity interval of one node label a scope can reach.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NodeGuard {
    pub(crate) label: String,
    pub(crate) bounds: GuardBounds,
}

/// The declared validity interval of one relationship type (optionally only
/// from one source type) a scope can reach.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EdgeGuard {
    pub(crate) rel_type: String,
    pub(crate) rel_key: InternedKey,
    /// `None` for the unkeyed declaration, the fallback for sources that have
    /// no keyed one of their own.
    pub(crate) source_type: Option<String>,
    pub(crate) source_type_key: Option<InternedKey>,
    pub(crate) bounds: GuardBounds,
}

/// The two bound properties, pre-interned, and whether the `to` day belongs
/// to the interval.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GuardBounds {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) from_key: InternedKey,
    pub(crate) to_key: InternedKey,
    pub(crate) convention: IntervalConvention,
}

impl GuardBounds {
    pub(crate) fn of(config: &TemporalConfig) -> Self {
        GuardBounds {
            from: config.valid_from.clone(),
            to: config.valid_to.clone(),
            from_key: InternedKey::from_str(&config.valid_from),
            to_key: InternedKey::from_str(&config.valid_to),
            convention: config.convention,
        }
    }
}

/// Every declared target one query scope can reach, in the declaration
/// store's lookup order: node labels by name, then relationship types by
/// name with a type's source-keyed declarations before its unkeyed one.
///
/// The template holds no instant. It is compiled at plan time and may be
/// cached with the plan; the instant is resolved per execution into a
/// [`ValidTimeSelector`] and joined with the template in a [`GraphFilter`].
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GuardTemplate {
    pub(crate) nodes: Vec<NodeGuard>,
    pub(crate) edges: Vec<EdgeGuard>,
}

impl fmt::Display for GuardTemplate {
    /// `(:Well [vf, vt] closed), [:LICENSEE from :Field [f, t] half_open]`,
    /// or `no declared targets`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.nodes.is_empty() && self.edges.is_empty() {
            return f.write_str("no declared targets");
        }
        let bounds = |b: &GuardBounds| format!("[{}, {}] {}", b.from, b.to, b.convention.as_str());
        let nodes = self
            .nodes
            .iter()
            .map(|n| format!("(:{} {})", n.label, bounds(&n.bounds)));
        let edges = self.edges.iter().map(|e| match &e.source_type {
            Some(source) => format!("[:{} from :{} {}]", e.rel_type, source, bounds(&e.bounds)),
            None => format!("[:{} {}]", e.rel_type, bounds(&e.bounds)),
        });
        let parts: Vec<String> = nodes.chain(edges).collect();
        f.write_str(&parts.join(", "))
    }
}

/// Which instants a filter keeps: those valid at one instant, or those whose
/// interval overlaps a range (the fluent two-date form).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ValidTimeSelector {
    AsOf(Instant),
    Overlap(Instant, Instant),
}

impl TryFrom<&TemporalContext> for ValidTimeSelector {
    type Error = ();

    /// The fluent temporal context as a selector; `All` filters nothing and
    /// has none.
    fn try_from(context: &TemporalContext) -> Result<Self, ()> {
        match context {
            TemporalContext::Today => Ok(ValidTimeSelector::AsOf(Instant::Date(
                chrono::Local::now().date_naive(),
            ))),
            TemporalContext::At(d) => Ok(ValidTimeSelector::AsOf(Instant::Date(*d))),
            TemporalContext::During(a, b) => Ok(ValidTimeSelector::Overlap(
                Instant::Date(*a),
                Instant::Date(*b),
            )),
            TemporalContext::All => Err(()),
        }
    }
}

/// A compiled template joined with the selector one execution resolved.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GraphFilter {
    pub(crate) template: Arc<GuardTemplate>,
    pub(crate) selector: ValidTimeSelector,
}
