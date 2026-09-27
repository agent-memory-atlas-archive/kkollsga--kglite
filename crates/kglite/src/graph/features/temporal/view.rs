//! A valid-time view: a graph as of one instant, with no copy.
//!
//! [`view_at`] holds the base graph's `Arc` and the instant. A query on the
//! view is the base query behind the view's `FOR VALID_TIME AS OF` prefix
//! ([`ValidTimeView::cypher_text`]), so it runs through the guarded matcher on
//! the base: `id(n)` is the user id and `elementId(n)` the base's physical
//! identity, exactly as a prefixed query on the base answers.
//!
//! On creation the view resolves the filter over **every** declared target
//! once and pins the masks that gives: it holds their `Arc`, so the mask LRU
//! cannot drop them from under a live view, and a query on the view — whose
//! key covers only the targets it reaches — is served the pinned masks
//! instead of building its own (see `endpoint_index::pin_masks`). Targets
//! the endpoint index cannot cover (Disk mode, an unreadable bound, the byte
//! cap) keep their property guards; there is nothing to pin for them.
//!
//! A consumer that needs a materialised graph asks for the view's
//! [`ValidSlice`] ([`ValidTimeView::slice`]), built on first use and cached
//! per segment beside the masks.

// `view_at` and `slice` report through `KgError`, as every core api entry a
// binding maps does; its structured variants trip `result_large_err` alike.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use super::endpoint_index::{self, ElementMasks};
use super::eval::{self, Instant};
use super::slice::{slice_at, SliceCaps, SliceKey};
pub use super::slice::{ValidSlice, DISK_SLICE_ELEMENT_CAP, SLICE_BYTE_CAP};
use crate::datatypes::values::Value;
use crate::error::KgError;
use crate::graph::core::graph_filter::{ElementFilter, GraphFilter, ValidTimeSelector};
use crate::graph::dir_graph::DirGraph;
use crate::graph::languages::cypher::valid_time::{
    carries_valid_time_context, declared_template, instant_literal, prefixed, PrependError,
    NO_DECLARATION,
};

/// A graph as of one valid-time instant; see the module docs.
pub struct ValidTimeView {
    base: Arc<DirGraph>,
    literal: String,
    instant: Instant,
    filter: GraphFilter,
    /// Keeps the resolved masks alive for the view's lifetime; the cache
    /// holds only a weak reference to them.
    _pinned: Option<Arc<ElementMasks>>,
}

impl std::fmt::Debug for ValidTimeView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidTimeView")
            .field("as_of", &self.literal)
            .field("pinned", &self._pinned.is_some())
            .finish_non_exhaustive()
    }
}

/// A view of `graph` as of `instant` — a date, a datetime, or an ISO
/// date/datetime string. Refused when the instant is not one of those, when
/// the graph has no validity declaration, and when a relationship type holds
/// several unkeyed declarations (which one applies would depend on
/// declaration order).
pub fn view_at(graph: Arc<DirGraph>, instant: &Value) -> Result<ValidTimeView, KgError> {
    let literal = instant_literal(instant).map_err(|e| KgError::Argument(e.to_string()))?;
    let parsed = eval::parse_instant(instant)
        .map_err(|err| KgError::Argument(format!("valid_at: {err}")))?;
    if super::declared(&graph).is_empty() {
        return Err(KgError::Argument(NO_DECLARATION.to_string()));
    }
    let template = declared_template(&graph).map_err(KgError::Argument)?;
    let filter = GraphFilter {
        template: Arc::new(template),
        selector: ValidTimeSelector::AsOf(parsed),
    };
    let resolved = filter.resolve(&graph);
    if let Some(masks) = &resolved.masks {
        endpoint_index::pin_masks(&graph, &resolved.key, masks);
    }
    Ok(ValidTimeView {
        literal,
        instant: parsed,
        filter,
        _pinned: resolved.masks,
        base: graph,
    })
}

impl ValidTimeView {
    /// The graph the view reads, unchanged.
    pub fn base(&self) -> &Arc<DirGraph> {
        &self.base
    }

    /// The instant as the view's prefix spells it: `date('…')` or
    /// `datetime('…')`.
    pub fn as_of(&self) -> &str {
        &self.literal
    }

    /// `query` behind the view's `FOR VALID_TIME AS OF` prefix — the text to
    /// run on [`Self::base`]. A query that carries a context of its own is
    /// refused ([`PrependError::ViewAlreadyAsOf`]): the view already fixes
    /// the instant. `EXPLAIN` / `PROFILE` may lead the query.
    pub fn cypher_text(&self, query: &str) -> Result<String, PrependError> {
        if carries_valid_time_context(query) {
            return Err(PrependError::ViewAlreadyAsOf {
                literal: self.literal.clone(),
            });
        }
        Ok(prefixed(&self.literal, query))
    }

    /// The view's materialised [`ValidSlice`], from the graph's slice cache or
    /// built now and cached. Refused over the slice caps (see
    /// [`SLICE_BYTE_CAP`] and, in Disk mode, [`DISK_SLICE_ELEMENT_CAP`]) and
    /// on a bound the validity evaluator cannot read.
    pub fn slice(&self) -> Result<Arc<ValidSlice>, KgError> {
        let resolved = self.filter.resolve(&self.base);
        let key = SliceKey {
            segments: resolved.key.clone(),
            instant: (!resolved.guarded.is_empty()).then_some(self.instant),
        };
        if let Some(slice) = endpoint_index::cached_slice(&self.base, &key) {
            return Ok(slice);
        }
        let filter = ElementFilter::new(&self.filter, resolved);
        let caps = SliceCaps::for_graph(&self.base);
        let slice =
            Arc::new(slice_at(&self.base, filter.as_ref(), caps).map_err(KgError::Argument)?);
        endpoint_index::store_slice(&self.base, key, &slice, caps.bytes);
        Ok(slice)
    }

    /// The masks the view pinned (tests).
    #[cfg(test)]
    pub(crate) fn pinned(&self) -> Option<&Arc<ElementMasks>> {
        self._pinned.as_ref()
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
