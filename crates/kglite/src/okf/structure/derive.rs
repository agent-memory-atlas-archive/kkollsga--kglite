//! Derive a note's own nodes from its block tree (VAULT.md §7.1).
//!
//! Pure: `derive(body, tree, profile)` reads no file and touches no graph, so
//! it runs inside the parallel parse pass beside the link extraction that
//! shares its tree.
//!
//! **Ids are suffixes here, not whole ids.** `resolve_ids` can still rewrite a
//! note's `concept_id` after parsing (a stem collision falls back to the
//! path), and a derived id built during the parse would then name a note that
//! no longer exists. Every node and edge therefore carries the part *after*
//! the note's id — `#A#B`, `#A#B~chunk2`, `#^block-id` — and the builder
//! prefixes the id the note ended up with.

use super::block::BlockTree;
use super::profile::{ChunkRule, SectionRule, StructureProfile};
use crate::datatypes::values::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// One node derived from a note's body.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DerivedNode {
    /// Appended to the note's id to make this node's `concept_id`.
    pub suffix: String,
    pub label: String,
    /// The enclosing section's suffix; `None` for a node that hangs off the
    /// note itself (a top-level section, a chunk above the first heading).
    pub section: Option<String>,
    /// The heading path of this node's own section — its own for a section,
    /// its container's for a chunk. `{heading_path}` in `embed_text:`, and the
    /// `path` property of a section.
    pub heading_path: Vec<String>,
    /// `{section_title}` in `embed_text:`.
    pub section_title: Option<String>,
    /// The verbatim source slice this node carries, if any.
    pub text: Option<String>,
    /// Everything else: `title`, `level`, `ordinal`, `chunk_hash`, `path`.
    pub props: Vec<(String, Value)>,
}

/// One edge between derived nodes, or from the note to one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedEdge {
    pub conn_type: String,
    /// `None` = the note itself.
    pub source: Option<String>,
    pub target: String,
}

/// What one note's body derived.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Derived {
    pub nodes: Vec<DerivedNode>,
    pub edges: Vec<DerivedEdge>,
    /// VAULT.md §9 warnings, without the note prefix the report adds.
    pub warnings: Vec<String>,
}

/// Derive every node `structure:` declares from one note's body.
pub(crate) fn derive(body: &str, tree: &BlockTree, profile: &StructureProfile) -> Derived {
    let mut out = Derived::default();
    let mut ids = IdSpace::default();
    let sections = profile
        .sections
        .as_ref()
        .map(|rule| derive_sections(body, tree, rule, &mut ids, &mut out));
    if let Some(rule) = &profile.chunks {
        derive_chunks(body, tree, rule, sections.as_deref(), &mut ids, &mut out);
    }
    out
}

/// The ids one note has already minted. A second node wanting an id takes
/// `~2`, `~3`… (VAULT.md §7.1) — the counter suffix always starts with a
/// letter, so a bare `~2` can only ever mean "the second thing that wanted
/// this id".
#[derive(Default)]
struct IdSpace(BTreeMap<String, usize>);

impl IdSpace {
    /// `(the id to use, whether it was already taken)`.
    fn claim(&mut self, wanted: &str) -> (String, bool) {
        let count = self.0.entry(wanted.to_string()).or_insert(0);
        *count += 1;
        match *count {
            1 => (wanted.to_string(), false),
            n => (format!("{wanted}~{n}"), true),
        }
    }
}

/// One node per heading, in document order (VAULT.md §7.1 `sections:`).
///
/// Returns each heading's suffix by tree index, so the chunk pass can name the
/// section a block sits in without re-deriving the paths.
fn derive_sections(
    body: &str,
    tree: &BlockTree,
    rule: &SectionRule,
    ids: &mut IdSpace,
    out: &mut Derived,
) -> Vec<String> {
    let mut suffixes: Vec<String> = Vec::with_capacity(tree.headings.len());
    // Per parent (`None` = the note), the last sibling's suffix and how many
    // there have been — `NEXT_SECTION` and `ordinal` both read it.
    let mut siblings: BTreeMap<Option<usize>, (usize, String)> = BTreeMap::new();
    for (index, heading) in tree.headings.iter().enumerate() {
        let parent = parent_of(tree, index);
        let (suffix, duplicate) = ids.claim(&format!("#{}", heading.path.join("#")));
        if duplicate {
            out.warnings.push(format!(
                "duplicate heading path `{}`: a link cannot reach the second one, which \
                 takes the id `{suffix}` — give it a `^block-id` (VAULT.md §5.7)",
                heading.path.join("#")
            ));
        }
        let entry = siblings.entry(parent).or_insert((0, String::new()));
        let ordinal = entry.0;
        let previous = (ordinal > 0).then(|| entry.1.clone());
        *entry = (ordinal + 1, suffix.clone());

        let parent_suffix = parent.map(|p| suffixes[p].clone());
        out.edges.push(DerivedEdge {
            conn_type: rule.edge.clone(),
            source: parent_suffix.clone(),
            target: suffix.clone(),
        });
        if let Some(parent_suffix) = &parent_suffix {
            out.edges.push(DerivedEdge {
                conn_type: rule.parent.clone(),
                source: Some(suffix.clone()),
                target: parent_suffix.clone(),
            });
        }
        if let Some(previous) = previous {
            out.edges.push(DerivedEdge {
                conn_type: rule.next.clone(),
                source: Some(previous),
                target: suffix.clone(),
            });
        }
        out.nodes.push(DerivedNode {
            suffix: suffix.clone(),
            label: rule.label.clone(),
            section: parent_suffix,
            heading_path: heading.path.clone(),
            section_title: Some(heading.text.clone()),
            text: Some(trimmed(body, heading.body_range.clone())),
            props: vec![
                ("title".to_string(), Value::String(heading.text.clone())),
                ("level".to_string(), Value::Int64(heading.level as i64)),
                ("ordinal".to_string(), Value::Int64(ordinal as i64)),
                (
                    "path".to_string(),
                    Value::List(
                        heading
                            .path
                            .iter()
                            .map(|p| Value::String(p.clone()))
                            .collect(),
                    ),
                ),
            ],
        });
        suffixes.push(suffix);
    }
    suffixes
}

/// The nearest preceding heading of a higher level — the one whose section
/// encloses this heading. `None` for a top-level heading.
///
/// Read from the levels rather than from `path`, so a `##` followed by a
/// `####` nests (CommonMark has no rule that levels descend one at a time).
fn parent_of(tree: &BlockTree, index: usize) -> Option<usize> {
    let level = tree.headings[index].level;
    tree.headings[..index].iter().rposition(|h| h.level < level)
}

/// Greedy paragraph packing per section (VAULT.md §7.1 `chunks:`).
fn derive_chunks(
    body: &str,
    tree: &BlockTree,
    rule: &ChunkRule,
    sections: Option<&[String]>,
    ids: &mut IdSpace,
    out: &mut Derived,
) {
    // `<n>` counts chunks under one *parent*, and the parent is the section
    // when sections are derived and the note otherwise — so a vault with
    // `chunks:` alone numbers one sequence for the whole note.
    let mut counters: BTreeMap<Option<String>, usize> = BTreeMap::new();
    for group in chunkable_groups(tree) {
        let container = sections.and_then(|s| group.heading.map(|h| s[h].clone()));
        let heading_path = group
            .heading
            .map(|h| tree.headings[h].path.clone())
            .unwrap_or_default();
        let section_title = group.heading.map(|h| tree.headings[h].text.clone());
        let mut previous: Option<String> = None;
        for packed in pack(body, tree, &group.blocks, rule) {
            let counter = counters.entry(container.clone()).or_insert(0);
            let ordinal = *counter;
            *counter += 1;
            let wanted = match &packed.block_id {
                Some(id) => format!("#^{id}"),
                None => format!(
                    "{}~chunk{}",
                    container.clone().unwrap_or_default(),
                    ordinal + 1
                ),
            };
            let (suffix, duplicate) = ids.claim(&wanted);
            if duplicate {
                out.warnings.push(format!(
                    "duplicate derived id `{wanted}`: the second one takes `{suffix}`"
                ));
            }
            let text = trimmed(body, packed.range.clone());
            out.edges.push(DerivedEdge {
                conn_type: rule.edge.clone(),
                source: container.clone(),
                target: suffix.clone(),
            });
            if let Some(previous) = previous.replace(suffix.clone()) {
                out.edges.push(DerivedEdge {
                    conn_type: rule.next.clone(),
                    source: Some(previous),
                    target: suffix.clone(),
                });
            }
            out.nodes.push(DerivedNode {
                suffix,
                label: rule.label.clone(),
                section: container.clone(),
                heading_path: heading_path.clone(),
                section_title: section_title.clone(),
                props: vec![
                    ("ordinal".to_string(), Value::Int64(ordinal as i64)),
                    ("chunk_hash".to_string(), Value::String(text_hash(&text))),
                ],
                text: Some(text),
            });
        }
    }
}

/// The blocks of one container — the note's pre-heading prose, or one
/// section's own — in document order. A section boundary always closes the
/// open chunk, which is what makes a container the unit here.
struct Group {
    heading: Option<usize>,
    blocks: Vec<usize>,
}

fn chunkable_groups(tree: &BlockTree) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (index, block) in tree.blocks.iter().enumerate() {
        // Only the outermost blocks: a paragraph inside a list or a quote is
        // that block's own content, and counting it again would duplicate the
        // text in two chunks.
        if block.inside.is_some() {
            continue;
        }
        // An own-line `^id` is an anchor, not prose (VAULT.md §5.7): it names
        // the block above it, which `pack` reads, and its own paragraph is not
        // text a reader sees.
        if is_own_line_block_id(tree, index) {
            continue;
        }
        match groups.last_mut() {
            Some(last) if last.heading == block.heading => last.blocks.push(index),
            _ => groups.push(Group {
                heading: block.heading,
                blocks: vec![index],
            }),
        }
    }
    groups
}

fn is_own_line_block_id(tree: &BlockTree, block: usize) -> bool {
    tree.block_ids.iter().any(|id| {
        id.own_line
            && id.range.start >= tree.blocks[block].range.start
            && id.range.end <= tree.blocks[block].range.end
    })
}

/// One packed chunk: the source range it covers and the block id that named
/// it, if any.
struct Packed {
    range: std::ops::Range<usize>,
    block_id: Option<String>,
}

/// Pack blocks greedily to `max_words` / `max_chars`, in document order.
///
/// A block bigger than either limit on its own is a chunk on its own, and a
/// block a `^block-id` names is a chunk of its own too — which is the author's
/// one lever over where a section divides (VAULT.md §7.1).
fn pack(body: &str, tree: &BlockTree, blocks: &[usize], rule: &ChunkRule) -> Vec<Packed> {
    let mut out: Vec<Packed> = Vec::new();
    let mut open: Option<std::ops::Range<usize>> = None;
    let mut words = 0usize;
    for &index in blocks {
        let range = tree.blocks[index].range.clone();
        let text = &body[range.clone()];
        let block_words = text.split_whitespace().count();
        if let Some(id) = block_id_of(tree, index) {
            if let Some(range) = open.take() {
                out.push(Packed {
                    range,
                    block_id: None,
                });
            }
            out.push(Packed {
                range,
                block_id: Some(id),
            });
            words = 0;
            continue;
        }
        match open.take() {
            Some(current)
                if words + block_words <= rule.max_words
                    && range.end - current.start <= rule.max_chars =>
            {
                open = Some(current.start..range.end);
                words += block_words;
            }
            Some(current) => {
                out.push(Packed {
                    range: current,
                    block_id: None,
                });
                open = Some(range);
                words = block_words;
            }
            None => {
                open = Some(range);
                words = block_words;
            }
        }
    }
    if let Some(range) = open {
        out.push(Packed {
            range,
            block_id: None,
        });
    }
    out
}

/// The `^block-id` naming this block — trailing it, or sitting on its own line
/// directly after it (VAULT.md §5.7's three placements).
fn block_id_of(tree: &BlockTree, block: usize) -> Option<String> {
    tree.block_ids
        .iter()
        .find(|id| id.attaches_to == Some(block))
        .map(|id| id.id.clone())
}

/// A derived node's verbatim slice, trailing blank lines trimmed (§7.1).
fn trimmed(body: &str, range: std::ops::Range<usize>) -> String {
    body[range].trim_end().to_string()
}

/// `chunk_hash`: the SHA-256 of the chunk's text, lowercase hex (VAULT.md
/// §7.1). It is what carries an embedding across a rebuild that only moved the
/// chunk (§12), so it hashes the text and nothing else — not the id, which is
/// exactly what moved.
fn text_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "derive_tests.rs"]
mod derive_tests;
