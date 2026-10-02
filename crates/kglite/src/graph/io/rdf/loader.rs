//! `load_rdf` entry point + the triple-fold driver (memory mode).
//!
//! Parses Turtle / N-Triples / N-Quads / TriG via the `oxttl` family,
//! folds every triple into a per-subject accumulator, then materialises
//! the accumulators into the in-memory property graph in dense intern
//! order. Targets the Default (in-memory) backend — the mapped and disk
//! backends require the columnar/CSR build pipeline and are not yet
//! supported (a single-stream RDF parse isn't a Wikidata-scale path; use
//! `load_ntriples` for that).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::BufReader;

use oxrdf::{NamedOrBlankNode, Term};
use oxttl::{NQuadsParser, NTriplesParser, TriGParser, TurtleParser};

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::{GraphRead, GraphWrite};

use super::curie::Curiefier;
use super::fold::datatype_to_value;
use super::interner::IriInterner;
use super::kg_import;
use crate::datatypes::PropMap;
use crate::graph::io::export::kg_vocab::{KG_MANIFEST, KG_NS, RDF_REIFIES};
use crate::graph::io::export::ExportManifest;

/// The `rdf:type` predicate IRI — the one predicate that sets a node's
/// type instead of becoming a property or edge.
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Default label predicate: `rdfs:label`.
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// Configuration for [`load_rdf`].
pub struct RdfConfig {
    /// Keep only literals tagged with one of these language codes. `None`
    /// keeps all literals (tagged or not). A language-tagged literal whose
    /// tag isn't in the set is dropped; untagged literals are always kept.
    pub languages: Option<HashSet<String>>,
    /// IRIs whose literal object sets the node title (first wins).
    /// Defaults to `[rdfs:label]`.
    pub label_predicates: Vec<String>,
    /// Keep full IRIs for predicate / type names instead of CURIE-compacting.
    pub keep_full_iris: bool,
    /// Node type for subjects without an `rdf:type`. Defaults to `"Resource"`.
    pub default_type: String,
    /// Stop after this many triples. `None` = no limit.
    pub max_triples: Option<u64>,
    /// Store a property whose literals carry language tags as a map
    /// `{lang: value}` instead of dropping the tags (the `languages` filter
    /// still applies first). Off keeps the untagged-value behaviour.
    pub language_maps: bool,
}

impl Default for RdfConfig {
    fn default() -> Self {
        RdfConfig {
            languages: None,
            label_predicates: vec![RDFS_LABEL.to_string()],
            keep_full_iris: false,
            default_type: "Resource".to_string(),
            max_triples: None,
            language_maps: false,
        }
    }
}

/// Counts returned after a successful load.
#[derive(Debug)]
pub struct RdfStats {
    pub nodes_created: usize,
    pub edges_created: usize,
    pub triples_processed: u64,
    /// Declarations of a `kg:manifest` that could not be applied (for
    /// example a validity interval on a type with no rows).
    pub warnings: Vec<String>,
}

/// Per-subject accumulator built during the fold, then drained into a
/// graph node at materialisation time.
#[derive(Default)]
struct NodeAcc {
    /// First `label_predicate` literal seen (first wins).
    title: Option<String>,
    /// First `rdf:type` value seen (compacted) — becomes the node type.
    node_type: Option<String>,
    /// All `rdf:type` values (compacted, deduped) — surfaced as
    /// `rdf_types` when a subject has more than one type.
    types: Vec<String>,
    /// Literal properties, keyed by compacted predicate.
    props: HashMap<String, Value>,
    /// Language-tagged literals (`language_maps`): predicate → lang → value.
    /// Boxed so an untagged document pays one pointer per node.
    langs: Option<Box<LangMaps>>,
}

/// Predicate → language tag → value.
#[derive(Default)]
struct LangMaps(BTreeMap<String, BTreeMap<String, Value>>);

impl NodeAcc {
    /// The node's property map: plain literals plus each language map. A
    /// predicate carrying both keeps the map as one more list element.
    fn into_properties(self) -> HashMap<String, Value> {
        let mut props = self.props;
        for (key, by_lang) in self.langs.into_iter().flat_map(|m| m.0) {
            let map = Value::Map(PropMap::from_pairs(by_lang.into_iter().collect()));
            insert_property(&mut props, key, map);
        }
        props
    }
}

/// Mutable fold state threaded through the per-statement processor.
struct FoldState {
    iris: IriInterner,
    curie: Curiefier,
    /// Accumulators indexed by intern id (dense, grows with `resize_with`).
    accs: Vec<NodeAcc>,
    /// Buffered edges as (source id, target id, compacted predicate).
    edges: Vec<(u32, u32, String)>,
    /// `(reifier id, source id, target id, predicate)` per
    /// `r rdf:reifies <<( s p o )>>`, in document order. A reifier's own
    /// literals (its accumulator) become the properties of the edge it reifies.
    reifiers: Vec<(u32, u32, u32, String)>,
    /// The `kg:manifest` of the document, once seen.
    manifest: Option<ExportManifest>,
}

impl FoldState {
    fn new(keep_full: bool) -> Self {
        FoldState {
            iris: IriInterner::new(),
            curie: Curiefier::new(keep_full),
            accs: Vec::new(),
            edges: Vec::new(),
            reifiers: Vec::new(),
            manifest: None,
        }
    }

    /// Intern a subject/object key and guarantee its accumulator slot
    /// exists, returning the dense id.
    fn ensure(&mut self, key: &str) -> u32 {
        let id = self.iris.get_or_intern(key);
        if self.accs.len() <= id as usize {
            self.accs.resize_with(id as usize + 1, NodeAcc::default);
        }
        id
    }
}

/// Load an RDF file into a fresh empty in-memory `graph`.
///
/// Use [`DirGraph::new`]: populated graphs, schema/index/constraint declarations,
/// identity aliases and mutation capture are refused before parsing or mutation.
/// This bootstrap loader does not append or enforce existing declarations.
/// Python and C callers already allocate a fresh destination.
///
/// Dispatches on the file extension: `.ttl` → Turtle, `.nt` → N-Triples,
/// `.nq` → N-Quads, `.trig` → TriG. Quad graph names are ignored.
pub fn load_rdf(graph: &mut DirGraph, path: &str, config: &RdfConfig) -> Result<RdfStats, String> {
    super::admission::check_target(graph)?;

    let ext = path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    // Validate the format before touching the filesystem, so an
    // unsupported extension fails fast and deterministically (regardless
    // of whether the file exists).
    if !matches!(ext.as_str(), "ttl" | "nt" | "nq" | "trig") {
        return Err(format!(
            "Unsupported RDF extension '{}': expected ttl, nt, nq, or trig",
            ext
        ));
    }

    let file = File::open(path).map_err(|e| format!("Cannot open {}: {}", path, e))?;
    let reader = BufReader::new(file);

    let mut state = FoldState::new(config.keep_full_iris);
    let mut processed: u64 = 0;

    // Each format yields either Triples or Quads; the per-statement fold
    // is identical, so a small closure adapts both shapes onto `process`.
    match ext.as_str() {
        "ttl" => {
            let mut parser = TurtleParser::new().for_reader(reader);
            while let Some(item) = parser.next() {
                let triple = item.map_err(|e| format!("Turtle parse error: {}", e))?;
                copy_prefixes(&mut state.curie, parser.prefixes());
                process(
                    &triple.subject,
                    triple.predicate.as_str(),
                    &triple.object,
                    config,
                    &mut state,
                )?;
                if hit_limit(&mut processed, config) {
                    break;
                }
            }
        }
        "nt" => {
            let parser = NTriplesParser::new().for_reader(reader);
            for item in parser {
                let triple = item.map_err(|e| format!("N-Triples parse error: {}", e))?;
                process(
                    &triple.subject,
                    triple.predicate.as_str(),
                    &triple.object,
                    config,
                    &mut state,
                )?;
                if hit_limit(&mut processed, config) {
                    break;
                }
            }
        }
        "nq" => {
            let parser = NQuadsParser::new().for_reader(reader);
            for item in parser {
                let quad = item.map_err(|e| format!("N-Quads parse error: {}", e))?;
                process(
                    &quad.subject,
                    quad.predicate.as_str(),
                    &quad.object,
                    config,
                    &mut state,
                )?;
                if hit_limit(&mut processed, config) {
                    break;
                }
            }
        }
        "trig" => {
            let mut parser = TriGParser::new().for_reader(reader);
            while let Some(item) = parser.next() {
                let quad = item.map_err(|e| format!("TriG parse error: {}", e))?;
                copy_prefixes(&mut state.curie, parser.prefixes());
                process(
                    &quad.subject,
                    quad.predicate.as_str(),
                    &quad.object,
                    config,
                    &mut state,
                )?;
                if hit_limit(&mut processed, config) {
                    break;
                }
            }
        }
        // `ext` was validated against this set before the file was opened.
        _ => unreachable!("extension validated before dispatch"),
    }

    let (nodes_created, edges_created, warnings) = materialize(graph, &mut state, config)?;
    Ok(RdfStats {
        nodes_created,
        edges_created,
        triples_processed: processed,
        warnings,
    })
}

/// Copy any newly-declared prefixes into the curiefier. Cheap — a
/// document has a handful of prefixes and `add` dedups on the IRI.
fn copy_prefixes<'a>(curie: &mut Curiefier, prefixes: impl Iterator<Item = (&'a str, &'a str)>) {
    for (name, iri) in prefixes {
        curie.add(name, iri);
    }
}

/// Bump the processed counter and report whether `max_triples` is hit.
#[inline]
fn hit_limit(processed: &mut u64, config: &RdfConfig) -> bool {
    *processed += 1;
    matches!(config.max_triples, Some(max) if *processed >= max)
}

/// The intern key for a subject/object resource: the IRI for a named
/// node, `_:<id>` for a blank node. Borrows the IRI for named nodes
/// (the common case) so the per-statement hot path allocates nothing on
/// an intern *hit*; only blank nodes need an owned `_:`-prefixed key.
fn resource_key(n: &NamedOrBlankNode) -> Cow<'_, str> {
    match n {
        NamedOrBlankNode::NamedNode(nn) => Cow::Borrowed(nn.as_str()),
        NamedOrBlankNode::BlankNode(bn) => Cow::Owned(format!("_:{}", bn.as_str())),
    }
}

/// Fold a single statement into the accumulator / edge state. Splits
/// cleanly into the statement kinds: `kg:` vocabulary, type, reification,
/// label, literal property, resource edge.
fn process(
    subject: &NamedOrBlankNode,
    predicate: &str,
    object: &Term,
    config: &RdfConfig,
    state: &mut FoldState,
) -> Result<(), String> {
    // Vocabulary statements describe the export, never a node.
    if predicate.starts_with(KG_NS) {
        return absorb_kg(predicate, object, state);
    }
    let subject_key = resource_key(subject);
    let s_id = state.ensure(subject_key.as_ref());

    // rdf:type → node type (+ multi-type tracking).
    if predicate == RDF_TYPE {
        if let Some(type_iri) = resource_iri(object) {
            let t = state.curie.compact(&type_iri);
            let acc = &mut state.accs[s_id as usize];
            if acc.node_type.is_none() {
                acc.node_type = Some(t.clone());
            }
            if !acc.types.contains(&t) {
                acc.types.push(t);
            }
        }
        return Ok(());
    }

    if predicate == RDF_REIFIES {
        fold_reifies(s_id, object, state);
        return Ok(());
    }

    match object {
        Term::Literal(lit) => {
            let lang = lit.language();
            // Drop language-tagged literals filtered out by config.
            if let (Some(lang), Some(keep)) = (lang, &config.languages) {
                if !keep.contains(lang) {
                    return Ok(());
                }
            }
            if config.label_predicates.iter().any(|p| p == predicate) {
                let acc = &mut state.accs[s_id as usize];
                if acc.title.is_none() {
                    acc.title = Some(lit.value().to_string());
                }
                // The title keeps one string; with `language_maps` the tagged
                // labels are also kept, as a map, so no tag is lost.
                if lang.is_none() || !config.language_maps {
                    return Ok(());
                }
            }
            let key = state.curie.compact(predicate);
            if let (true, Some(lang)) = (config.language_maps, lang) {
                insert_tagged(&mut state.accs[s_id as usize], key, lang, lit.value());
                return Ok(());
            }
            let val = datatype_to_value(lit.value(), lit.datatype().as_str());
            insert_property(&mut state.accs[s_id as usize].props, key, val);
        }
        // Resource object → edge. Materialise the target's slot too so a
        // node that only ever appears as an object still exists.
        Term::NamedNode(_) | Term::BlankNode(_) => {
            let object_key = term_resource_key(object);
            let o_id = state.ensure(object_key.as_ref());
            let pred = state.curie.compact(predicate);
            state.edges.push((s_id, o_id, pred));
        }
        // A triple term outside `rdf:reifies` is neither a property nor an edge.
        Term::Triple(_) => {}
    }
    Ok(())
}

/// Record `r rdf:reifies <<( s p o )>>` for an edge-shaped triple term;
/// a term whose object is a literal annotates no edge and is ignored.
fn fold_reifies(reifier: u32, object: &Term, state: &mut FoldState) {
    let Term::Triple(triple) = object else {
        return;
    };
    if matches!(triple.object, Term::Literal(_) | Term::Triple(_)) {
        return;
    }
    let source = resource_key(&triple.subject);
    let source = state.ensure(source.as_ref());
    let target = term_resource_key(&triple.object);
    let target = state.ensure(target.as_ref());
    let pred = state.curie.compact(triple.predicate.as_str());
    state.reifiers.push((reifier, source, target, pred));
}

/// Take a `kg:` statement: `kg:manifest` is the export manifest; any other
/// `kg:` term is reserved and skipped.
fn absorb_kg(predicate: &str, object: &Term, state: &mut FoldState) -> Result<(), String> {
    if predicate != KG_MANIFEST {
        return Ok(());
    }
    let Term::Literal(lit) = object else {
        return Err("kg:manifest must be a literal".to_string());
    };
    if state.manifest.is_some() {
        return Err("the document carries more than one kg:manifest".to_string());
    }
    state.manifest = Some(ExportManifest::from_json(lit.value())?);
    Ok(())
}

/// Record a language-tagged literal; a repeat of the same predicate and tag
/// becomes a list under that tag.
fn insert_tagged(acc: &mut NodeAcc, key: String, lang: &str, text: &str) {
    let by_lang = acc.langs.get_or_insert_default().0.entry(key).or_default();
    let value = Value::String(text.to_string());
    match by_lang.get_mut(lang) {
        None => {
            by_lang.insert(lang.to_string(), value);
        }
        Some(Value::List(list)) => list.push(value),
        Some(existing) => {
            let old = std::mem::replace(existing, Value::Null);
            *existing = Value::List(vec![old, value]);
        }
    }
}

/// Extract the IRI of a resource `Term` (named or blank), if it is one.
/// Used for `rdf:type` objects, which may be a named node or blank node.
/// Borrows for named nodes (no alloc).
fn resource_iri(term: &Term) -> Option<Cow<'_, str>> {
    match term {
        Term::NamedNode(nn) => Some(Cow::Borrowed(nn.as_str())),
        Term::BlankNode(bn) => Some(Cow::Owned(format!("_:{}", bn.as_str()))),
        _ => None,
    }
}

/// Intern key for a resource `Term` (assumes named/blank — literals are
/// handled before this is reached). Borrows the IRI for named nodes.
fn term_resource_key(term: &Term) -> Cow<'_, str> {
    match term {
        Term::NamedNode(nn) => Cow::Borrowed(nn.as_str()),
        Term::BlankNode(bn) => Cow::Owned(format!("_:{}", bn.as_str())),
        _ => Cow::Borrowed(""),
    }
}

/// Insert a literal property with multi-value folding: a repeated
/// predicate promotes the existing scalar to a `Value::List` and appends.
fn insert_property(props: &mut HashMap<String, Value>, key: String, val: Value) {
    match props.get_mut(&key) {
        None => {
            props.insert(key, val);
        }
        Some(Value::List(list)) => list.push(val),
        Some(existing) => {
            let old = std::mem::replace(existing, Value::Null);
            *existing = Value::List(vec![old, val]);
        }
    }
}

/// Materialise the fold state into graph nodes + edges. Returns
/// (nodes_created, edges_created, warnings). Reifier nodes are not
/// materialised: their literals become edge properties.
fn materialize(
    graph: &mut DirGraph,
    state: &mut FoldState,
    config: &RdfConfig,
) -> Result<(usize, usize, Vec<String>), String> {
    let n = state.iris.len();
    let mut idx_of: Vec<Option<petgraph::graph::NodeIndex>> = Vec::with_capacity(n);
    let reifier_ids: HashSet<u32> = state.reifiers.iter().map(|r| r.0).collect();
    let mut reifier_props: HashMap<u32, HashMap<String, Value>> = HashMap::new();
    let manifest = state.manifest.as_ref();
    let mut identities = kg_import::IdentityCheck::default();
    let mut dense: u32 = 0;

    for id in 0..n as u32 {
        let acc = std::mem::take(&mut state.accs[id as usize]);
        if reifier_ids.contains(&id) {
            reifier_props.insert(id, acc.into_properties());
            idx_of.push(None);
            continue;
        }
        let iri = state.iris.iri(id).to_string();

        let node_type = acc
            .node_type
            .clone()
            .unwrap_or_else(|| config.default_type.clone());
        let title = acc.title.clone().unwrap_or_else(|| iri.clone());
        let types = if acc.types.len() > 1 {
            Some(Value::List(
                acc.types.iter().cloned().map(Value::String).collect(),
            ))
        } else {
            None
        };

        let mut properties = acc.into_properties();
        properties.insert("uri".to_string(), Value::String(iri.clone()));
        if let Some(types) = types {
            properties.insert("rdf_types".to_string(), types);
        }

        // Dense integer id — `n.id` is an integer in every mode — unless the
        // manifest says the type's ids were something else.
        let (id_value, title_value, duplicate) =
            kg_import::node_identity(manifest, &node_type, &iri, title, dense, &mut identities)?;
        dense += 1;
        let node_data = NodeData::new(
            id_value.clone(),
            title_value,
            node_type.clone(),
            properties,
            &mut graph.interner,
        );
        let node_idx = GraphWrite::add_node(&mut graph.graph, node_data);
        graph
            .type_indices
            .entry_or_default(node_type.clone())
            .push(node_idx);
        kg_import::add_labels(graph, manifest, &node_type, node_idx);
        if !duplicate || graph.id_indices.lookup(&node_type, &id_value).is_none() {
            graph
                .id_indices
                .entry_or_default(node_type)
                .insert(id_value, node_idx);
        }
        idx_of.push(Some(node_idx));
    }

    let plans = plan_edges(state, &reifier_props);
    let edges_created = materialize_edges(graph, &plans, &idx_of);
    let warnings = match manifest {
        Some(manifest) => kg_import::apply(graph, manifest)?,
        None => Vec::new(),
    };
    Ok((dense as usize, edges_created, warnings))
}

/// One edge to create, with the properties its reifier carried.
struct EdgePlan {
    source: u32,
    target: u32,
    predicate: String,
    props: HashMap<String, Value>,
}

/// Decide the edges: every plain `s p o` statement is one edge, and the
/// reifiers of that triple supply properties in document order; reifiers
/// beyond the plain statements are edges of their own, so a triple has
/// `max(plain statements, reifiers)` parallel edges.
fn plan_edges(
    state: &FoldState,
    reifier_props: &HashMap<u32, HashMap<String, Value>>,
) -> Vec<EdgePlan> {
    let mut pending: HashMap<(u32, u32, &str), VecDeque<u32>> = HashMap::new();
    for (reifier, source, target, pred) in &state.reifiers {
        pending
            .entry((*source, *target, pred.as_str()))
            .or_default()
            .push_back(*reifier);
    }
    let props_of = |reifier: Option<u32>| {
        reifier
            .and_then(|r| reifier_props.get(&r))
            .cloned()
            .unwrap_or_default()
    };
    let mut plans = Vec::with_capacity(state.edges.len());
    for (source, target, pred) in &state.edges {
        let reifier = pending
            .get_mut(&(*source, *target, pred.as_str()))
            .and_then(VecDeque::pop_front);
        plans.push(EdgePlan {
            source: *source,
            target: *target,
            predicate: pred.clone(),
            props: props_of(reifier),
        });
    }
    for (reifier, source, target, pred) in &state.reifiers {
        let key = (*source, *target, pred.as_str());
        let Some(queue) = pending.get_mut(&key) else {
            continue;
        };
        if let Some(pos) = queue.iter().position(|r| r == reifier) {
            queue.remove(pos);
            plans.push(EdgePlan {
                source: *source,
                target: *target,
                predicate: pred.clone(),
                props: props_of(Some(*reifier)),
            });
        }
    }
    plans
}

/// Create the planned edges and record connection-type metadata,
/// mirroring `create_edges_strings`. Every endpoint is a materialised node
/// except a reifier, whose edges are dropped.
fn materialize_edges(
    graph: &mut DirGraph,
    plans: &[EdgePlan],
    idx_of: &[Option<petgraph::graph::NodeIndex>],
) -> usize {
    let mut conn_type_pairs: HashMap<String, (HashSet<String>, HashSet<String>)> = HashMap::new();
    let mut prop_types: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut created = 0;

    for plan in plans {
        let (Some(src), Some(tgt)) = (idx_of[plan.source as usize], idx_of[plan.target as usize])
        else {
            continue;
        };
        let pred = &plan.predicate;
        let types = prop_types.entry(pred.clone()).or_default();
        for (key, value) in &plan.props {
            types
                .entry(key.clone())
                .or_insert_with(|| value.type_name().to_string());
        }

        let edge_data = EdgeData::new(pred.clone(), plan.props.clone(), &mut graph.interner);

        let src_type = GraphRead::node_weight(&graph.graph, src)
            .unwrap()
            .node_type_str(&graph.interner)
            .to_string();
        let tgt_type = GraphRead::node_weight(&graph.graph, tgt)
            .unwrap()
            .node_type_str(&graph.interner)
            .to_string();
        let entry = conn_type_pairs
            .entry(pred.clone())
            .or_insert_with(|| (HashSet::new(), HashSet::new()));
        entry.0.insert(src_type);
        entry.1.insert(tgt_type);

        GraphWrite::add_edge(&mut graph.graph, src, tgt, edge_data);
        created += 1;
    }

    for (conn_type, (source_types, target_types)) in conn_type_pairs {
        let types = prop_types.remove(&conn_type).unwrap_or_default();
        for src_type in &source_types {
            for tgt_type in &target_types {
                graph.upsert_connection_type_metadata(
                    &conn_type,
                    src_type,
                    tgt_type,
                    types.clone(),
                );
            }
        }
    }
    graph.invalidate_edge_type_counts_cache();
    created
}

#[cfg(all(test, feature = "rdf"))]
mod tests {
    use super::*;
    use crate::graph::storage::GraphRead;
    use std::io::Write;

    /// Write `content` to a temp file with the given extension and load it.
    fn load_str(content: &str, ext: &str, config: &RdfConfig) -> (DirGraph, RdfStats) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("fixture.{}", ext));
        let mut f = File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        drop(f);
        let mut graph = DirGraph::new();
        let stats = load_rdf(&mut graph, path.to_str().unwrap(), config).unwrap();
        (graph, stats)
    }

    /// Find a node index whose `uri` property equals `uri`.
    fn node_idx_by_uri(graph: &DirGraph, uri: &str) -> Option<petgraph::graph::NodeIndex> {
        for idx in GraphRead::node_indices(&graph.graph) {
            let node = GraphRead::node_view(&graph.graph, idx).unwrap();
            if let Some(v) = node.get_property("uri") {
                if matches!(v.as_ref(), Value::String(s) if s == uri) {
                    return Some(idx);
                }
            }
        }
        None
    }

    /// Find a node by its `uri` property and return (node_type, props clone).
    fn find_by_uri(graph: &DirGraph, uri: &str) -> Option<(String, HashMap<String, Value>)> {
        let idx = node_idx_by_uri(graph, uri)?;
        let node = GraphRead::node_view(&graph.graph, idx).unwrap();
        let node_type = node.node_type_str(&graph.interner).to_string();
        let props = node.properties_cloned(&graph.interner);
        Some((node_type, props))
    }

    fn title_of(graph: &DirGraph, uri: &str) -> Option<String> {
        let idx = node_idx_by_uri(graph, uri)?;
        let node = GraphRead::node_view(&graph.graph, idx).unwrap();
        match node.title().into_owned() {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    const TTL_PEOPLE: &str = r#"
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ex: <http://example.org/> .

ex:alice a foaf:Person ;
    rdfs:label "Alice" ;
    foaf:knows ex:bob ;
    ex:age "30"^^xsd:integer ;
    ex:height "1.75"^^xsd:double ;
    ex:active "true"^^xsd:boolean ;
    ex:born "1990-05-01"^^xsd:date ;
    ex:lastSeen "2020-01-15T08:30:00Z"^^xsd:dateTime .

ex:bob a foaf:Person ;
    rdfs:label "Bob" .
"#;

    #[test]
    fn turtle_people_with_typed_literals() {
        let cfg = RdfConfig::default();
        let (graph, stats) = load_str(TTL_PEOPLE, "ttl", &cfg);

        assert_eq!(stats.edges_created, 1, "one foaf:knows edge");

        let (alice_type, props) =
            find_by_uri(&graph, "http://example.org/alice").expect("alice node");
        assert_eq!(alice_type, "foaf__Person");
        assert_eq!(
            title_of(&graph, "http://example.org/alice").as_deref(),
            Some("Alice")
        );
        // `ex:` is a document-declared prefix; predicates compact to
        // `ex__*` (double-underscore separator, Cypher-queryable).
        assert_eq!(props.get("ex__age"), Some(&Value::Int64(30)));
        assert_eq!(props.get("ex__height"), Some(&Value::Float64(1.75)));
        assert_eq!(props.get("ex__active"), Some(&Value::Boolean(true)));
        assert!(matches!(props.get("ex__born"), Some(Value::DateTime(_))));
        assert!(matches!(
            props.get("ex__lastSeen"),
            Some(Value::Timestamp(_))
        ));

        // Both people materialised.
        assert!(find_by_uri(&graph, "http://example.org/bob").is_some());
    }

    #[test]
    fn multiple_types_first_wins_and_rdf_types_list() {
        let ttl = r#"
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix ex: <http://example.org/> .
ex:carol rdf:type foaf:Person, foaf:Agent .
"#;
        let cfg = RdfConfig::default();
        let (graph, _) = load_str(ttl, "ttl", &cfg);
        let (node_type, props) =
            find_by_uri(&graph, "http://example.org/carol").expect("carol node");
        // First rdf:type wins as the node type.
        assert_eq!(node_type, "foaf__Person");
        match props.get("rdf_types") {
            Some(Value::List(l)) => assert_eq!(l.len(), 2),
            other => panic!("expected rdf_types list of 2, got {:?}", other),
        }
    }

    #[test]
    fn ntriples_blank_node_subject() {
        let nt = "_:b0 <http://www.w3.org/2000/01/rdf-schema#label> \"Anon\" .\n";
        let cfg = RdfConfig::default();
        let (graph, _) = load_str(nt, "nt", &cfg);

        // The blank node materialised with a `uri` starting `_:`.
        let mut found = false;
        for idx in GraphRead::node_indices(&graph.graph) {
            let node = GraphRead::node_view(&graph.graph, idx).unwrap();
            if let Some(v) = node.get_property("uri") {
                if let Value::String(s) = v.as_ref() {
                    if s.starts_with("_:") {
                        found = true;
                    }
                }
            }
        }
        assert!(found, "blank-node subject should have a _: uri property");
    }

    /// Throughput comparison: oxttl parse-only vs `load_rdf` vs the
    /// hand-tuned Wikidata `load_ntriples`, on identical Wikidata-shaped
    /// N-Triples. Ignored by default — run in release:
    /// `cargo test -p kglite --release --features rdf,okf \
    ///   bench_vs_wikidata -- --ignored --nocapture`
    #[test]
    #[ignore = "perf benchmark"]
    fn bench_vs_wikidata() {
        use crate::graph::io::ntriples::{load_ntriples, NTriplesConfig};
        use std::io::{BufWriter, Write as _};
        use std::time::Instant;

        let entities: u32 = 400_000;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench.nt");
        {
            let mut w = BufWriter::new(File::create(&path).unwrap());
            for i in 0..entities {
                let s = format!("<http://www.wikidata.org/entity/Q{i}>");
                writeln!(w, "{s} <http://www.wikidata.org/prop/direct/P31> <http://www.wikidata.org/entity/Q5> .").unwrap();
                writeln!(
                    w,
                    "{s} <http://www.w3.org/2000/01/rdf-schema#label> \"Person {i}\"@en ."
                )
                .unwrap();
                writeln!(w, "{s} <http://www.wikidata.org/prop/direct/P1082> \"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer> .").unwrap();
                writeln!(w, "{s} <http://www.wikidata.org/prop/direct/P569> \"1990-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> .").unwrap();
                writeln!(w, "{s} <http://www.wikidata.org/prop/direct/P26> <http://www.wikidata.org/entity/Q{}> .", (i + 1) % entities).unwrap();
            }
        }
        let triples = entities as u64 * 5;
        let mb = std::fs::metadata(&path).unwrap().len() as f64 / 1e6;
        eprintln!("\ncorpus: {entities} entities, {triples} triples, {mb:.1} MB\n");

        let rate = |t: f64| (triples as f64 / 1e6 / t, mb / t);

        // oxttl parse-only (no graph build).
        let t = Instant::now();
        let mut n = 0u64;
        for item in
            oxttl::NTriplesParser::new().for_reader(BufReader::new(File::open(&path).unwrap()))
        {
            item.unwrap();
            n += 1;
        }
        let (mt, mbs) = rate(t.elapsed().as_secs_f64());
        eprintln!(
            "oxttl parse-only:   {n} triples in {:.3}s = {mt:.2} M triples/s, {mbs:.0} MB/s",
            t.elapsed().as_secs_f64()
        );

        // load_rdf full build (memory).
        let mut g = DirGraph::new();
        let t = Instant::now();
        let stats = load_rdf(&mut g, path.to_str().unwrap(), &RdfConfig::default()).unwrap();
        let secs = t.elapsed().as_secs_f64();
        let (mt, mbs) = rate(secs);
        eprintln!("load_rdf (nt):      {} nodes, {} edges in {secs:.3}s = {mt:.2} M triples/s, {mbs:.0} MB/s", stats.nodes_created, stats.edges_created);

        // load_rdf on the SAME data as Turtle — exercises @prefix handling
        // (the prefix-copy path) and CURIE compaction from declared prefixes.
        let ttl_path = dir.path().join("bench.ttl");
        {
            let mut w = BufWriter::new(File::create(&ttl_path).unwrap());
            writeln!(w, "@prefix wd: <http://www.wikidata.org/entity/> .").unwrap();
            writeln!(w, "@prefix wdt: <http://www.wikidata.org/prop/direct/> .").unwrap();
            writeln!(w, "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .").unwrap();
            writeln!(w, "@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .").unwrap();
            for i in 0..entities {
                writeln!(w, "wd:Q{i} wdt:P31 wd:Q5 ; rdfs:label \"Person {i}\"@en ; wdt:P1082 \"{i}\"^^xsd:integer ; wdt:P569 \"1990-01-01T00:00:00Z\"^^xsd:dateTime ; wdt:P26 wd:Q{} .", (i + 1) % entities).unwrap();
            }
        }
        let ttl_mb = std::fs::metadata(&ttl_path).unwrap().len() as f64 / 1e6;
        let mut gt = DirGraph::new();
        let t = Instant::now();
        let statst = load_rdf(&mut gt, ttl_path.to_str().unwrap(), &RdfConfig::default()).unwrap();
        let secst = t.elapsed().as_secs_f64();
        eprintln!(
            "load_rdf (turtle):  {} nodes, {} edges in {secst:.3}s = {:.2} M triples/s, {:.0} MB/s",
            statst.nodes_created,
            statst.edges_created,
            triples as f64 / 1e6 / secst,
            ttl_mb / secst
        );

        // load_ntriples full build (memory, Wikidata-tuned path).
        let mut g2 = DirGraph::new();
        let cfg = NTriplesConfig {
            predicates: None,
            languages: None,
            node_types: HashMap::new(),
            predicate_labels: HashMap::new(),
            max_entities: None,
            max_triples: None,
            verbose: false,
            auto_type: false,
            progress: None,
        };
        let t = Instant::now();
        let stats2 = load_ntriples(&mut g2, path.to_str().unwrap(), &cfg).unwrap();
        let secs2 = t.elapsed().as_secs_f64();
        let (mt, mbs) = rate(secs2);
        eprintln!("load_ntriples (WD): {} entities, {} edges in {secs2:.3}s = {mt:.2} M triples/s, {mbs:.0} MB/s", stats2.entities_created, stats2.edges_created);
        eprintln!(
            "\nload_rdf / load_ntriples wall-clock ratio: {:.2}x\n",
            secs / secs2
        );
    }

    #[test]
    fn language_filter_keeps_only_en() {
        let ttl = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ex: <http://example.org/> .
ex:thing rdfs:label "Bonjour"@fr ;
    rdfs:label "Hello"@en .
"#;
        let mut langs = HashSet::new();
        langs.insert("en".to_string());
        let cfg = RdfConfig {
            languages: Some(langs),
            ..RdfConfig::default()
        };
        let (graph, _) = load_str(ttl, "ttl", &cfg);
        // The fr label is filtered out, so only the en label can set the title.
        assert_eq!(
            title_of(&graph, "http://example.org/thing").as_deref(),
            Some("Hello")
        );
    }
}

#[cfg(all(test, feature = "rdf"))]
#[path = "loader_tests.rs"]
mod loader_tests;
