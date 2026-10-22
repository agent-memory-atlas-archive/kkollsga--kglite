//! What a graph says about the parts of it that are not served from a mapping.
//!
//! A mapped load serves the topology, the id and type indexes, the property
//! columns and the embedding stores from the file where it can. Each of them
//! has a rule for what it cannot take (a graph below the size floors, ids of
//! two kinds, a column of mixed values), and without a report a graph that
//! grows through writes drifts to the heap with nothing to say so. Three
//! sources feed [`GraphInfo`](super::GraphInfo):
//!
//! - the **writer** records, in the file's metadata, each part a save wrote
//!   without a mapped form and why ([`MappedFallback`]);
//! - the **loader** records why the topology is not served from the file
//!   (storage, policy, or what the file recorded);
//! - the **live graph** answers the rest (an overlay, a collapse, a copy).
//!
//! A save also parks a notice naming the parts it dropped
//! ([`DirGraph::take_save_notice`]), which a binding announces its own way.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::DirGraph;
use crate::graph::storage::GraphRead;

/// A part of a graph that is held in heap or compressed form instead of being
/// served from the file, and the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappedFallback {
    /// `topology`, `indexes:nodes`, `indexes:peers`, `indexes:id:<Type>`,
    /// `columns:<Type>.<column>` or `embeddings:<Type>.<property>`.
    pub part: String,
    pub reason: String,
}

impl MappedFallback {
    pub(crate) fn new(part: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            part: part.into(),
            reason: reason.into(),
        }
    }
}

/// Why a column is written as a compressed integer column: bounded and by
/// design, so a save does not warn about it.
pub(crate) const DELTA_COLUMN_REASON: &str =
    "integers stored delta-compressed (four times smaller); decoded at load into a temporary file";

/// Why a save recorded no topology for a graph with more than 262,144
/// relationships that carry properties; a memory-storage save does not warn
/// about it.
pub(crate) const PROPERTY_EDGE_REASON: &str =
    "more than 262,144 relationships carry properties, which are held on the heap either way";

/// What a load recorded about the graph it produced.
#[derive(Clone, Default)]
pub(crate) struct MappedOrigin {
    /// Why the topology was not served from the file: the storage mode, the
    /// load policy, or what the file recorded.
    pub(crate) topology_reason: Option<String>,
    /// Parts the file records as written without a mapped form.
    pub(crate) fallbacks: Arc<Vec<MappedFallback>>,
    /// The load served the topology from the file.
    pub(crate) was_frozen: bool,
}

/// Why a load left the topology off the file, for a graph it placed in
/// `target`: the storage asked for, the policy, or what the file recorded.
pub(crate) fn not_served_reason(
    mapped_target: bool,
    memory_policy: bool,
    recorded: &[MappedFallback],
) -> String {
    if !mapped_target {
        "memory storage".to_string()
    } else if memory_policy {
        "topology=\"memory\" decodes the compressed topology".to_string()
    } else if let Some(topology) = recorded.iter().find(|f| f.part == "topology") {
        format!("the file records no raw topology: {}", topology.reason)
    } else {
        "the file records no raw topology: a graph below the size floors (65,536 nodes and \
         262,144 relationships), layout=\"compact\", or a file written by an older kglite"
            .to_string()
    }
}

/// Fallbacks past this many are summarised, so a graph of thousands of types
/// does not write a report as large as its schema.
const MAX_LISTED: usize = 50;

/// `fallbacks` sorted, with all past [`MAX_LISTED`] folded into one entry.
pub(crate) fn finalize(mut fallbacks: Vec<MappedFallback>) -> Vec<MappedFallback> {
    fallbacks.sort_by(|a, b| a.part.cmp(&b.part).then_with(|| a.reason.cmp(&b.reason)));
    fallbacks.dedup();
    if fallbacks.len() > MAX_LISTED {
        let more = fallbacks.len() - (MAX_LISTED - 1);
        fallbacks.truncate(MAX_LISTED - 1);
        fallbacks.push(MappedFallback::new(
            "more",
            format!("{more} further parts are held the same way"),
        ));
    }
    fallbacks
}

impl DirGraph {
    /// `(status, reason)` for the topology: `mapped` (served from the file),
    /// `overlay` (served from the file with a write overlay over it), `heap`
    /// with the reason, or `disk` (the disk backend's own files).
    pub(crate) fn topology_report(&self) -> (&'static str, Option<String>) {
        if self.graph.is_disk() {
            return ("disk", None);
        }
        if self.graph.has_frozen_base() && !self.graph.is_frozen() {
            return ("overlay", None);
        }
        if self.graph.is_frozen() {
            if self.graph.topology_memory().mapped_bytes > 0 {
                return ("mapped", None);
            }
            return ("heap", Some(copied_sections_reason()));
        }
        let reason = self
            .mapped_origin
            .topology_reason
            .clone()
            .unwrap_or_else(|| {
                if self.mapped_origin.was_frozen {
                    "collapsed to the heap by a whole-graph operation such as vacuum(); a save \
                 and a mapped load serve it from the file again"
                        .to_string()
                } else if self.graph.is_mapped() {
                    "built or collapsed in this process; a save and a mapped load serve it \
                 from the file"
                        .to_string()
                } else {
                    "memory storage".to_string()
                }
            });
        ("heap", Some(reason))
    }

    /// The parts held without a mapped form: what the file records, plus
    /// the embedding stores that are on the heap now. Empty for a graph that
    /// is not in mapped storage, where everything is heap by choice.
    pub(crate) fn mapped_fallback_report(&self) -> Vec<MappedFallback> {
        if !self.graph.is_mapped() {
            return Vec::new();
        }
        let mut out: Vec<MappedFallback> = self.mapped_origin.fallbacks.as_ref().clone();
        for ((node_type, property), store) in &self.embeddings {
            if !store.is_mapped()
                && store.data.len() * std::mem::size_of::<f32>()
                    >= crate::graph::io::file::embedding_sections::WRITE_MIN_BYTES
            {
                out.push(MappedFallback::new(
                    format!("embeddings:{node_type}.{property}"),
                    "held on the heap: below the 64 MiB load floor, or copied there by a \
                     write; a save writes a store of 4 MiB or more raw, and a mapped load maps \
                     one of 64 MiB or more",
                ));
            }
        }
        finalize(out)
    }

    /// The notice the last save parked, once: the parts it wrote without a
    /// mapped form. A binding announces it (a Python warning, a stderr line).
    pub fn take_save_notice(&self) -> Option<String> {
        self.save_notice.lock().ok()?.take()
    }

    /// Forget a notice left by an earlier write, before a save starts.
    pub(crate) fn clear_save_notice(&self) {
        if let Ok(mut notice) = self.save_notice.lock() {
            *notice = None;
        }
    }

    /// Park the notice for the parts a save just wrote without a mapped form.
    /// Compressed integer columns are by design and not named.
    pub(crate) fn park_save_notice(&self, fallbacks: &[MappedFallback]) {
        let named: Vec<String> = fallbacks
            .iter()
            .filter(|f| f.reason != DELTA_COLUMN_REASON)
            // A graph in memory storage saves property-edge graphs routinely and
            // never reads the topology sections; the reason is in the file and
            // `graph_info()` for whoever loads it mapped, without a warning here.
            .filter(|f| self.graph.is_mapped() || f.reason != PROPERTY_EDGE_REASON)
            .map(|f| format!("{} ({})", f.part, f.reason))
            .collect();
        if named.is_empty() {
            return;
        }
        let text = format!(
            "save() wrote {} without a mapped form, so a mapped load of this file holds {} on the \
             heap: {}. graph_info()['mapped_fallbacks'] lists them after a load.",
            if named.len() == 1 {
                "1 part".to_string()
            } else {
                format!("{} parts", named.len())
            },
            if named.len() == 1 { "it" } else { "them" },
            named.join("; "),
        );
        if let Ok(mut notice) = self.save_notice.lock() {
            *notice = Some(text);
        }
    }
}

/// Why a topology served from sections has no mapped bytes: the load copied
/// them to the heap.
fn copied_sections_reason() -> String {
    if std::env::var("KGLITE_MMAP").is_ok_and(|value| value == "0") {
        "KGLITE_MMAP=0 copies the sections to the heap".to_string()
    } else if cfg!(windows) {
        "Windows copies the sections to the heap".to_string()
    } else {
        "the sections were copied to the heap: the file is under 64 KiB, or it is a byte \
         buffer or on a network or FUSE filesystem"
            .to_string()
    }
}
