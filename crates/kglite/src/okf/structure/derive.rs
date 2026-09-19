//! Derive a note's own nodes from its block tree (VAULT.md §7.1).
//!
//! Pure: `derive(body, tree, title, label, profile)` reads no file and touches no
//! graph, so
//! it runs inside the parallel parse pass beside the link extraction that
//! shares its tree.
//!
//! **Ids are suffixes here, not whole ids.** `resolve_ids` can still rewrite a
//! note's `concept_id` after parsing (a stem collision falls back to the
//! path), and a derived id built during the parse would then name a note that
//! no longer exists. Every node and edge therefore carries the part *after*
//! the note's id — `#A#B`, `#A#B~chunk2`, `#^block-id` — and the builder
//! prefixes the id the note ended up with.

use super::block::{BlockTree, List};
use super::constructs::{derive_callouts, derive_fences, derive_lists, Ctx};
use super::profile::{ChunkRule, KeyFromHeadingRule, SectionRule, StructureProfile};
use super::tables::derive_tables;
use crate::datatypes::values::Value;
use crate::okf::model::Link;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

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
    /// its enclosing one for everything else. `{heading_path}` in
    /// `embed_text:`, and the `path` property of a section.
    pub heading_path: Vec<String>,
    /// `{section_title}` in `embed_text:`.
    pub section_title: Option<String>,
    /// The verbatim source slice this node carries, if any.
    pub text: Option<String>,
    /// Everything else — `title`, `level`, `ordinal`, `path`, `chunk_hash`,
    /// `kind`, `fold`, `lang`, `code`, `caption`, `step_count` — as the rule
    /// that derived this node declares them.
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
    /// The edges an `edges: true` table stated (VAULT.md §7.1). They are
    /// [`Link`]s and not [`DerivedEdge`]s because their target is a *note* the
    /// resolver has yet to find: a row travels the ladder every prose link
    /// travels, stub and all. `parse_file` moves them onto the note's own
    /// links, so this is empty by the time the builder sees a `ConceptDoc`.
    pub links: Vec<Link>,
    /// The edge types an edge-table rule actually produced, for the §9 warning
    /// about a rule the vault declared and no note matched.
    pub edge_tables_hit: BTreeSet<String>,
    /// VAULT.md §9 warnings, without the note prefix the report adds.
    pub warnings: Vec<String>,
    /// Chunk boundaries the cap forced *inside* one block, summed over the
    /// note: the pieces a block was cut into, less the one it would have been.
    pub forced_splits: usize,
}

/// Derive every node `structure:` declares from one note's body.
///
/// Rule order is the order the ids are claimed in, and therefore the order a
/// collision resolves in: sections first (the only stable ids), then chunks,
/// then the constructs a section contains.
pub(crate) fn derive(
    body: &str,
    tree: &BlockTree,
    note_title: &str,
    note_label: &str,
    profile: &StructureProfile,
) -> Derived {
    let mut out = Derived::default();
    let mut ids = IdSpace::default();
    // `<!-- kglite -->` is the directive shape without the key it needs
    // (VAULT.md §5.8). It is still cut out of every text below, so saying so
    // is the only way the author learns the line did nothing.
    for _ in tree.directives.iter().filter(|d| d.key.is_empty()) {
        out.warnings.push(
            "`<!-- kglite -->` names no key; nothing was recorded (VAULT.md §5.8)".to_string(),
        );
    }
    let sections = profile
        .sections
        .as_ref()
        .map(|rule| derive_sections(body, tree, rule, &mut ids, &mut out));
    if let Some(rule) = &profile.chunks {
        derive_chunks(body, tree, rule, sections.as_deref(), &mut ids, &mut out);
    }
    let ctx = Ctx {
        body,
        tree,
        sections: sections.as_deref(),
        note_title,
    };
    if let Some(rule) = &profile.callouts {
        derive_callouts(&ctx, rule, &mut ids, &mut out);
    }
    if let Some(rule) = &profile.code_fences {
        derive_fences(&ctx, rule, &mut ids, &mut out);
    }
    if let Some(rule) = &profile.ordered_lists {
        derive_lists(&ctx, rule, &mut ids, &mut out);
    }
    if !profile.tables.is_empty() {
        derive_tables(&ctx, &profile.tables, &mut ids, &mut out);
    }
    if let Some(rule) = &profile.key_from_heading {
        let section_label = profile.sections.as_ref().map(|r| r.label.as_str());
        relabel_symbols(&mut out, rule, note_label, section_label);
    }
    out
}

/// `key_from_heading:` — a Section whose title is really a symbol name is
/// **relabelled**, not duplicated (VAULT.md §7.1).
///
/// Both declared gates plus the one the format states outright: the heading
/// must contain a `.` or a `(` whatever `when_matches:` says. On one corpus
/// the regex alone matched 1 439 headings of which 13 were symbols, and a
/// heading like `Overview` is a valid qualified name to a regex and nothing
/// else. The section's own properties and its section edges are unchanged —
/// this adds a label and two properties and takes nothing away.
fn relabel_symbols(
    out: &mut Derived,
    rule: &KeyFromHeadingRule,
    note_label: &str,
    section_label: Option<&str>,
) {
    if note_label != rule.under_label {
        return;
    }
    let Some(section_label) = section_label else {
        return;
    };
    for node in &mut out.nodes {
        if node.label != section_label {
            continue;
        }
        let Some(Value::String(title)) = node
            .props
            .iter()
            .find(|(k, _)| k == "title")
            .map(|(_, v)| v)
            .cloned()
        else {
            continue;
        };
        if !(title.contains('.') || title.contains('(')) || !rule.when_matches.is_match(&title) {
            continue;
        }
        node.label = rule.label.clone();
        let (name, signature) = split_signature(&title);
        node.props
            .push((rule.property.clone(), Value::String(name.to_string())));
        if !signature.is_empty() {
            node.props.push((
                "signature".to_string(),
                Value::String(signature.to_string()),
            ));
        }
    }
}

/// The symbol name and what follows it: everything up to the first `(` or `→`
/// is the name a query looks up, the rest is the call signature and the return
/// annotation a converter wrote into the same heading.
fn split_signature(title: &str) -> (&str, &str) {
    let cut = [title.find('('), title.find('→')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(title.len());
    (title[..cut].trim_end(), title[cut..].trim())
}

/// The ids one note has already minted. A second node wanting an id takes
/// `~2`, `~3`… (VAULT.md §7.1) — the counter suffix always starts with a
/// letter, so a bare `~2` can only ever mean "the second thing that wanted
/// this id".
#[derive(Default)]
pub(super) struct IdSpace(BTreeMap<String, usize>);

impl IdSpace {
    /// `(the id to use, whether it was already taken)`.
    pub(super) fn claim(&mut self, wanted: &str) -> (String, bool) {
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
            text: Some(trimmed(body, tree, heading.body_range.clone())),
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

/// Greedy block packing per section (VAULT.md §7.1 `chunks:`), plus the
/// boundaries the caps forced inside a block too big to be one chunk.
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
        let (packed_chunks, forced) = pack(body, tree, &group.blocks, rule);
        out.forced_splits += forced;
        for packed in packed_chunks {
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
            let text = trimmed(body, tree, packed.range.clone());
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
        // A `<!-- kglite … -->` block is metadata, not prose (VAULT.md §5.8).
        // Dropping it here keeps it from being a chunk of its own; the bytes
        // a chunk spanning *over* it would still carry are cut by `trimmed`.
        if tree.directives.iter().any(|d| d.block == index) {
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
/// A block a `^block-id` names is a chunk of its own — the author's one lever
/// over where a section divides (VAULT.md §7.1). A block that exceeds either
/// limit **on its own** is cut at its own line boundaries rather than handed
/// over whole: an index page written as one 900-line list has no paragraph
/// break to pack against, and before this it became a single 150 kB chunk
/// whatever the vault declared.
///
/// Returns the packed chunks and how many boundaries the caps forced *inside*
/// a block — `BuildReport::forced_splits`.
fn pack(body: &str, tree: &BlockTree, blocks: &[usize], rule: &ChunkRule) -> (Vec<Packed>, usize) {
    let mut out: Vec<Packed> = Vec::new();
    let mut forced = 0usize;
    let mut open: Option<std::ops::Range<usize>> = None;
    let mut words = 0usize;
    for &index in blocks {
        let range = tree.blocks[index].range.clone();
        let text = &body[range.clone()];
        let block_words = text.split_whitespace().count();
        let id = block_id_of(tree, index);
        let oversize = block_words > rule.max_words || range.len() > rule.max_chars;
        if id.is_some() || oversize {
            if let Some(range) = open.take() {
                out.push(Packed {
                    range,
                    block_id: None,
                });
            }
            words = 0;
            let pieces = if oversize {
                split_block(body, tree, index, rule)
            } else {
                vec![range]
            };
            forced += pieces.len() - 1;
            // The block id keys the *first* piece: it is the one the author's
            // `[[Note#^id]]` was pointing at when the block still fitted.
            for (nth, piece) in pieces.into_iter().enumerate() {
                out.push(Packed {
                    range: piece,
                    block_id: if nth == 0 { id.clone() } else { None },
                });
            }
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
    (out, forced)
}

/// Cut one over-cap block into pieces that tile its range, at the boundaries
/// its own kind offers (VAULT.md §7.1).
///
/// A list divides between its **top-level** items, so a nested list travels
/// with the item that introduced it; everything else divides at line ends,
/// which for a table is exactly its rows — the header row therefore stays in
/// the first piece and is not repeated, because a chunk is a range of the
/// source and not a rendering of it.
fn split_block(
    body: &str,
    tree: &BlockTree,
    index: usize,
    rule: &ChunkRule,
) -> Vec<std::ops::Range<usize>> {
    let range = tree.blocks[index].range.clone();
    let atoms = match &tree.blocks[index].kind {
        super::block::BlockKind::List(list) => item_atoms(list, &range),
        _ => line_atoms(body, &range),
    };
    let pieces = pack_atoms(body, atoms, rule);
    merge_blank(body, pieces)
}

/// The ranges between the starts of a list's top-level items, tiling the whole
/// block: an item's own trailing blank line belongs to the item above it.
fn item_atoms(list: &List, range: &std::ops::Range<usize>) -> Vec<std::ops::Range<usize>> {
    let mut bounds = vec![range.start];
    for item in &list.items {
        if item.range.start > *bounds.last().expect("seeded") && item.range.start < range.end {
            bounds.push(item.range.start);
        }
    }
    bounds.push(range.end);
    bounds.windows(2).map(|w| w[0]..w[1]).collect()
}

/// The lines of `range`, each carrying its own trailing newline.
fn line_atoms(body: &str, range: &std::ops::Range<usize>) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = range.start;
    for (offset, byte) in body.as_bytes()[range.clone()].iter().enumerate() {
        if *byte == b'\n' {
            let end = range.start + offset + 1;
            out.push(start..end);
            start = end;
        }
    }
    if start < range.end || out.is_empty() {
        out.push(start..range.end);
    }
    out
}

/// Pack a block's own atoms the way [`pack`] packs blocks. An atom that busts
/// `max_chars` by itself is refined once more — by line if it has more than
/// one, and otherwise cut at a `char` boundary, which is the only split left
/// and the only one that can land inside a word.
fn pack_atoms(
    body: &str,
    atoms: Vec<std::ops::Range<usize>>,
    rule: &ChunkRule,
) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut open: Option<std::ops::Range<usize>> = None;
    let mut words = 0usize;
    for atom in atoms {
        if atom.len() > rule.max_chars {
            if let Some(current) = open.take() {
                out.push(current);
                words = 0;
            }
            let lines = line_atoms(body, &atom);
            if lines.len() > 1 {
                out.extend(pack_atoms(body, lines, rule));
            } else {
                out.extend(hard_split(body, atom, rule.max_chars));
            }
            continue;
        }
        let atom_words = body[atom.clone()].split_whitespace().count();
        match open.take() {
            Some(current)
                if words + atom_words <= rule.max_words
                    && atom.end - current.start <= rule.max_chars =>
            {
                open = Some(current.start..atom.end);
                words += atom_words;
            }
            Some(current) => {
                out.push(current);
                open = Some(atom);
                words = atom_words;
            }
            None => {
                open = Some(atom);
                words = atom_words;
            }
        }
    }
    if let Some(current) = open {
        out.push(current);
    }
    out
}

/// Cut a single over-cap line at `char` boundaries, never mid-codepoint.
fn hard_split(
    body: &str,
    atom: std::ops::Range<usize>,
    max_chars: usize,
) -> Vec<std::ops::Range<usize>> {
    let max = max_chars.max(1);
    let mut out = Vec::new();
    let mut start = atom.start;
    while atom.end - start > max {
        let mut cut = start + max;
        while cut > start && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut == start {
            // One character is wider than the whole budget; emitting it is
            // the only alternative to an endless loop.
            cut = start + 1;
            while cut < atom.end && !body.is_char_boundary(cut) {
                cut += 1;
            }
        }
        out.push(start..cut);
        start = cut;
    }
    out.push(start..atom.end);
    out
}

/// Fold a piece that is nothing but whitespace into its neighbour: the trailing
/// newline a hard split leaves behind would otherwise become a chunk whose
/// `text` is the empty string.
fn merge_blank(body: &str, pieces: Vec<std::ops::Range<usize>>) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for piece in pieces {
        match out.last_mut() {
            Some(previous) if body[piece.clone()].trim().is_empty() => previous.end = piece.end,
            _ => out.push(piece),
        }
    }
    // A leading blank piece has no predecessor to join, so it joins forward.
    if out.len() > 1 && body[out[0].clone()].trim().is_empty() {
        let head = out.remove(0);
        out[0].start = head.start;
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

/// A derived node's verbatim slice, with every `<!-- kglite … -->` directive
/// inside it cut out and trailing blank lines trimmed (§5.8, §7.1).
///
/// The cut is pure range subtraction: a directive's own line goes, its
/// trailing newline included, and **every other byte is the author's own**.
/// So a directive written between two paragraphs leaves behind both of the
/// blank lines that separated it from them, and the text reads one blank line
/// wider than a source without the directive would have. That is the price of
/// the rule that matters more — nothing but the directive moves, so editing a
/// directive cannot change the `chunk_hash` of a passage it does not sit in.
fn trimmed(body: &str, tree: &BlockTree, range: std::ops::Range<usize>) -> String {
    let cuts: Vec<&std::ops::Range<usize>> = tree
        .directives
        .iter()
        .map(|directive| &directive.range)
        .filter(|cut| cut.start >= range.start && cut.end <= range.end)
        .collect();
    if cuts.is_empty() {
        return body[range].trim_end().to_string();
    }
    let mut out = String::with_capacity(range.len());
    let mut at = range.start;
    for cut in cuts {
        out.push_str(&body[at..cut.start]);
        at = cut.end;
    }
    out.push_str(&body[at..range.end]);
    out.trim_end().to_string()
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
