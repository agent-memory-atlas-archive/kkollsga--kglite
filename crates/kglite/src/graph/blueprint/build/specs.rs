//! Spec flattening: one `FlatSpec` per node type, with each spec's
//! `sub_nodes` lifted out into `FlatSpec`s of their own.

use super::super::schema::NodeSpec;
use indexmap::IndexMap;

/// Flattened view of one node spec with parent info carried along.
pub struct FlatSpec {
    pub node_type: String,
    pub spec: NodeSpec,
    pub parent: Option<String>,
    /// The parent type's `pk` is `"auto"`; set by [`mark_auto_pk_parents`].
    pub(crate) parent_pk_auto: bool,
    pub is_manual: bool,
    /// Name of the input this spec's rows come from, as looked up in the
    /// build's `InputRegistry`: the `files` entry the spec names, or the `csv`
    /// shorthand, which the registry declares under the path string itself.
    /// This is the single place those two keys are read — the load phases
    /// resolve a *name*, never a path, so a future non-file format needs no
    /// change below `build/`.
    pub input: Option<String>,
}

pub(super) fn collect_specs(nodes: &IndexMap<String, NodeSpec>) -> (Vec<FlatSpec>, Vec<FlatSpec>) {
    let mut core = Vec::new();
    let mut subs = Vec::new();
    for (name, spec) in nodes {
        let input = spec.input_name().map(str::to_string);
        // A node type with no input at all is the manual form, synthesised
        // from the FK values that refer to it.
        let is_manual = input.is_none();
        core.push(FlatSpec {
            node_type: name.clone(),
            spec: clone_without_subs(spec),
            parent: None,
            parent_pk_auto: false,
            is_manual,
            input,
        });
        for (sub_name, sub_spec) in &spec.sub_nodes {
            // Sub-nodes keep their raw `parent` field untouched; the
            // enclosing type name is recorded on `FlatSpec.parent`, which
            // `set_parent_type` and the implicit `OF_<PARENT>` edge for a
            // `parent_fk` (see `parent_link::parent_link`) both read.
            let sub_clone = clone_without_subs(sub_spec);
            subs.push(FlatSpec {
                node_type: sub_name.clone(),
                spec: sub_clone,
                parent: Some(name.clone()),
                parent_pk_auto: false,
                is_manual: false,
                input: sub_spec.input_name().map(str::to_string),
            });
        }
    }
    (core, subs)
}

/// Records on each spec whether its parent type declares `pk: "auto"`.
/// Separate from [`collect_specs`] because a `parent` key can name any type.
pub(super) fn mark_auto_pk_parents(core: &mut [FlatSpec], subs: &mut [FlatSpec]) {
    let auto: std::collections::HashSet<String> = core
        .iter()
        .chain(subs.iter())
        .filter(|s| s.spec.pk.as_deref() == Some("auto"))
        .map(|s| s.node_type.clone())
        .collect();
    for spec in core.iter_mut().chain(subs.iter_mut()) {
        let parent = spec.spec.parent.as_ref().or(spec.parent.as_ref());
        spec.parent_pk_auto = parent.is_some_and(|p| auto.contains(p));
    }
}

/// The flattening pass's per-type copy: everything the spec declares except
/// its `sub_nodes`, which are flattened into their own `FlatSpec`s.
///
/// Struct-update syntax on purpose — a field-by-field copy silently drops any
/// field added to `NodeSpec` later, and the loss shows up as a directive the
/// blueprint declares and the build ignores.
fn clone_without_subs(spec: &NodeSpec) -> NodeSpec {
    NodeSpec {
        sub_nodes: IndexMap::new(),
        ..spec.clone()
    }
}

#[cfg(test)]
#[path = "../build_spec_clone_tests.rs"]
mod spec_clone_tests;
