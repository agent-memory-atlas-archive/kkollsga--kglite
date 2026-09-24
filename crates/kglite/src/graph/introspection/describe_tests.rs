//! Describe-output regression tests extracted from describe.rs.

use super::*;

#[cfg(test)]
mod declared_type_annotation_tests {
    use super::*;
    use crate::datatypes::values::Value;
    use crate::graph::property_types::DeclaredType;
    use crate::graph::schema::NodeData;
    use crate::graph::storage::GraphWrite;
    use std::collections::HashMap;

    fn person_graph() -> DirGraph {
        let mut graph = DirGraph::new();
        for (id, age) in [(1u32, 30i64), (2, 25)] {
            let node = NodeData::new(
                Value::UniqueId(id),
                Value::String(format!("p{id}")),
                "Person".to_string(),
                HashMap::from([("age".to_string(), Value::Int64(age))]),
                &mut graph.interner,
            );
            let idx = graph.graph.add_node(node);
            graph
                .type_indices
                .entry_or_default("Person".to_string())
                .push(idx);
        }
        graph
    }

    fn describe(graph: &DirGraph) -> String {
        compute_description(graph, &DescribeRequest::new(DescribeSurface::Python)).unwrap()
    }

    /// An agent planning a write needs to know the value will be rejected
    /// *before* attempting it, which is the whole reason `describe()` annotates
    /// declared constraints at all.
    #[test]
    fn describe_annotates_a_declared_property_type() {
        let mut graph = person_graph();
        assert!(
            !describe(&graph).contains("declared_type"),
            "an unconstrained property must carry no annotation"
        );

        graph
            .create_property_type_constraint("Person", "age", DeclaredType::Integer)
            .unwrap();
        let described = describe(&graph);
        assert!(
            described.contains("declared_type=\"INTEGER\""),
            "got: {described}"
        );
    }

    /// The same two facts on the connection side. An agent planning
    /// `CREATE (a)-[:KNOWS {since: …}]->(b)` needs the edge annotation for the
    /// same reason it needs the node one — and in the same vocabulary, or it
    /// has to learn two.
    #[test]
    fn describe_annotates_a_declared_relationship_constraint() {
        use crate::graph::algorithms::Interrupt;
        use crate::graph::languages::cypher::executor::write::execute_mutable;
        use crate::graph::languages::cypher::parser::parse_cypher;

        let mut graph = DirGraph::new();
        let parsed = parse_cypher(
            "CREATE (a:Person {person_id: 1})-[:KNOWS {since: 2020}]->(b:Person {person_id: 2})",
        )
        .unwrap();
        execute_mutable(&mut graph, &parsed, HashMap::new(), Interrupt::default()).unwrap();

        let with_connections = |graph: &DirGraph| {
            compute_description(
                graph,
                &DescribeRequest {
                    connections: &ConnectionDetail::Topics(vec!["KNOWS".to_string()]),
                    ..DescribeRequest::new(DescribeSurface::Python)
                },
            )
            .unwrap()
        };
        assert!(
            !with_connections(&graph).contains("constraint="),
            "an unconstrained edge property must carry no annotation"
        );

        graph
            .create_rel_not_null_constraint("KNOWS", "since", &Interrupt::default())
            .unwrap();
        graph
            .create_rel_property_type_constraint(
                "KNOWS",
                "since",
                DeclaredType::Integer,
                &Interrupt::default(),
            )
            .unwrap();
        let described = with_connections(&graph);
        assert!(
            described.contains("constraint=\"not_null\""),
            "got: {described}"
        );
        assert!(
            described.contains("declared_type=\"INTEGER\""),
            "got: {described}"
        );
    }

    /// The two facts are orthogonal — a property can be unique *and* typed — so
    /// the annotation must not replace the constraint one.
    #[test]
    fn a_typed_property_keeps_its_uniqueness_annotation() {
        let mut graph = person_graph();
        graph.create_unique_constraint("Person", &["age"]).unwrap();
        graph
            .create_property_type_constraint("Person", "age", DeclaredType::Integer)
            .unwrap();

        let described = describe(&graph);
        assert!(
            described.contains("constraint=\"unique\""),
            "got: {described}"
        );
        assert!(
            described.contains("declared_type=\"INTEGER\""),
            "got: {described}"
        );
    }
}

#[cfg(test)]
mod mcp_quickstart_tests {
    use super::mcp_quickstart;

    #[test]
    fn names_only_current_install_and_extension_contracts() {
        let quickstart = mcp_quickstart();
        for expected in [
            "pip install kglite",
            "cargo install kglite-mcp-server",
            "trust.allow_embedder: true",
            "library: sentence-transformers",
            "--features fastembed",
        ] {
            assert!(quickstart.contains(expected), "missing {expected:?}");
        }
        for retired in ["kglite[mcp]", "--embedder", "--trust-tools", "python:"] {
            assert!(
                !quickstart.contains(retired),
                "retired contract returned: {retired:?}"
            );
        }
    }
}

#[cfg(test)]
mod focused_detail_error_tests {
    use super::*;
    use crate::graph::session::{execute_mut, ExecuteOptions};
    use std::collections::HashMap;

    fn vessel_graph() -> DirGraph {
        let params = HashMap::new();
        let opts = ExecuteOptions::eager(&params);
        let mut graph = DirGraph::new();
        execute_mut(&mut graph, "CREATE (:Vessel {id: 1})", &opts).expect("seed");
        graph
    }

    /// `graph_overview(types=['vessel'])` on a graph of `Vessel`s named the
    /// available types and left the reader to spot the case difference. It is
    /// the same near-miss the MATCH warnings hint at, so it gets the same hint.
    #[test]
    fn a_near_miss_type_name_is_suggested() {
        let graph = vessel_graph();
        let error = build_focused_detail(
            &graph,
            &["vessel".to_string()],
            None,
            DescribeSurface::Python,
        )
        .expect_err("unknown type must error");
        assert!(error.contains("Did you mean 'Vessel'?"), "{error}");
        assert!(error.contains("Available: Vessel"), "{error}");
    }

    /// A name nothing is close to gets the list and no invented suggestion —
    /// `did_you_mean`'s bar is "genuinely close, or silent".
    #[test]
    fn a_far_type_name_gets_no_invented_suggestion() {
        let graph = vessel_graph();
        let error = build_focused_detail(
            &graph,
            &["Xyzzy".to_string()],
            None,
            DescribeSurface::Python,
        )
        .expect_err("unknown type must error");
        assert!(!error.contains("Did you mean"), "{error}");
    }
}

#[cfg(test)]
mod index_annotation_tests {
    use super::*;
    use crate::datatypes::values::Value;
    use crate::graph::storage::backend::GraphBackend;
    use tempfile::TempDir;

    /// Built through `add_nodes` so the type carries column metadata: the disk
    /// property index reads the `Str` column, and `describe()`'s `type_string`
    /// comes from that metadata.
    fn city_graph() -> DirGraph {
        let frame = crate::datatypes::DataFrame::from_cypher_rows(
            vec!["id".into(), "title".into(), "city".into(), "pop".into()],
            vec![
                vec![
                    Value::Int64(1),
                    Value::String("p1".into()),
                    Value::String("Oslo".into()),
                    Value::Int64(700),
                ],
                vec![
                    Value::Int64(2),
                    Value::String("p2".into()),
                    Value::String("Bergen".into()),
                    Value::Int64(280),
                ],
            ],
        )
        .unwrap();
        let mut graph = DirGraph::new();
        crate::graph::mutation::maintain::add_nodes(
            &mut graph,
            frame,
            "Person".to_string(),
            "id".to_string(),
            Some("title".to_string()),
            None,
        )
        .unwrap();
        graph
    }

    fn describe(graph: &DirGraph) -> String {
        compute_description(graph, &DescribeRequest::new(DescribeSurface::Python)).unwrap()
    }

    fn attr_of(described: &str, property: &str) -> String {
        let needle = format!("name=\"{property}\"");
        let line = described
            .lines()
            .find(|l| l.contains(&needle))
            .unwrap_or_else(|| panic!("no <prop {needle}> in: {described}"));
        line.trim().to_string()
    }

    /// The in-memory index is a value→members hash, and the memory backends
    /// inherit `lookup_by_property_prefix`'s `None` default — a `STARTS WITH`
    /// here full-scans, so advertising `prefix` points an agent at a path the
    /// engine does not have.
    #[test]
    fn a_memory_hash_index_advertises_equality_only() {
        let mut graph = city_graph();
        graph.create_index("Person", "city");
        let line = attr_of(&describe(&graph), "city");
        assert!(line.contains("indexed=\"eq\""), "got: {line}");
    }

    /// A range index serves ordered predicates, not equality — folding it into
    /// `eq` would promise an O(log N) point lookup that does not exist.
    #[test]
    fn a_range_index_is_reported_under_its_own_name() {
        let mut graph = city_graph();
        graph.create_range_index("Person", "pop");
        let line = attr_of(&describe(&graph), "pop");
        assert!(line.contains("indexed=\"range\""), "got: {line}");
    }

    /// Both structures on one property — what `CREATE RANGE INDEX` builds.
    #[test]
    fn equality_and_range_on_one_property_report_both() {
        let mut graph = city_graph();
        graph.create_index("Person", "pop");
        graph.create_range_index("Person", "pop");
        let line = attr_of(&describe(&graph), "pop");
        assert!(line.contains("indexed=\"eq,range\""), "got: {line}");
    }

    /// The disk index *is* a sorted key array, so prefix is real there — the
    /// one place `eq,prefix` is true.
    #[test]
    fn a_disk_string_index_advertises_prefix() {
        let dir = TempDir::new().unwrap();
        let mut graph = city_graph();
        graph.enable_disk_mode().unwrap();
        graph.save_disk(dir.path().to_str().unwrap()).unwrap();
        match &mut graph.graph {
            GraphBackend::Disk(disk) => {
                assert_eq!(disk.build_property_index("Person", "city").unwrap(), 2);
            }
            _ => panic!("expected disk backend"),
        }
        let line = attr_of(&describe(&graph), "city");
        assert!(line.contains("indexed=\"eq,prefix\""), "got: {line}");
    }

    /// `durable=True` and `cdc::enable` wrap the backend, and the index is
    /// still there underneath: a graph that loses its `indexed=` annotations
    /// the moment capture is switched on tells an agent to stop using an index
    /// it still has.
    #[test]
    fn capture_wrapping_does_not_hide_the_disk_index() {
        let dir = TempDir::new().unwrap();
        let mut graph = city_graph();
        graph.enable_disk_mode().unwrap();
        graph.save_disk(dir.path().to_str().unwrap()).unwrap();
        match &mut graph.graph {
            GraphBackend::Disk(disk) => {
                assert_eq!(disk.build_property_index("Person", "city").unwrap(), 2);
            }
            _ => panic!("expected disk backend"),
        }
        graph.graph.wrap_for_capture();
        assert!(
            graph.has_any_index("Person", "city"),
            "a wrapped disk graph still has its persistent index"
        );
        let line = attr_of(&describe(&graph), "city");
        assert!(line.contains("indexed=\"eq,prefix\""), "got: {line}");
    }
}

/// `describe(connections=['LINKS'])` samples edges through
/// `for_each_edge_of_conn_type`. A disk graph converted by `enable_disk_mode`
/// has its edges in the CSR with no `conn_type_index_*`, and an empty sweep
/// there reports a connection type that exists with no endpoints and no
/// samples — a wrong answer an agent plans against.
#[cfg(test)]
mod disk_connection_sampling_tests {
    use super::*;
    use crate::datatypes::{DataFrame, Value};

    fn linked_docs() -> DirGraph {
        let nodes = DataFrame::from_cypher_rows(
            vec!["id".into(), "title".into()],
            vec![
                vec![Value::Int64(1), Value::String("a".into())],
                vec![Value::Int64(2), Value::String("b".into())],
                vec![Value::Int64(3), Value::String("c".into())],
            ],
        )
        .unwrap();
        let links = DataFrame::from_cypher_rows(
            vec!["src".into(), "tgt".into()],
            vec![
                vec![Value::Int64(1), Value::Int64(3)],
                vec![Value::Int64(2), Value::Int64(3)],
            ],
        )
        .unwrap();
        let mut graph = DirGraph::new();
        crate::graph::mutation::maintain::add_nodes(
            &mut graph,
            nodes,
            "Doc".to_string(),
            "id".to_string(),
            Some("title".to_string()),
            None,
        )
        .unwrap();
        crate::graph::mutation::maintain::add_connections(
            &mut graph,
            links,
            "LINKS".to_string(),
            "Doc".to_string(),
            "src".to_string(),
            "Doc".to_string(),
            "tgt".to_string(),
            None,
            None,
            None,
        )
        .unwrap();
        graph
    }

    #[test]
    fn a_converted_disk_graph_still_samples_its_connections() {
        let mut graph = linked_docs();
        graph.enable_disk_mode().unwrap();
        assert!(
            graph
                .graph
                .as_disk()
                .expect("disk mode")
                .conn_type_index_types
                .is_empty(),
            "the conversion builds no conn-type index, or this test asserts nothing"
        );

        let acc = accumulate_connection_topic(&graph, InternedKey::from_str("LINKS"), "LINKS", 5);
        assert!(
            !acc.samples.is_empty(),
            "an index-less disk graph must still yield sample edges"
        );
        assert_eq!(
            acc.pair_counts.get(&("Doc".to_string(), "Doc".to_string())),
            Some(&2),
            "both Doc→Doc edges must be counted, got {:?}",
            acc.pair_counts
        );
    }
}

/// Every hint `describe()` emits names a callable the *reader* can actually
/// invoke.
///
/// The whole document was written for the MCP tool: a Python caller was told
/// to call `graph_overview(...)`, which is not a method on `KnowledgeGraph`,
/// and a CLI user was told the same. The surface is threaded through
/// [`DescribeRequest`] so each reader is told its own spelling — and the
/// negative half is the regression net: nothing may leak another surface's
/// name.
#[cfg(test)]
mod surface_hint_tests {
    use super::*;
    use crate::datatypes::values::Value;
    use crate::graph::schema::NodeData;
    use crate::graph::storage::GraphWrite;
    use std::collections::HashMap;

    fn two_type_graph() -> DirGraph {
        let mut graph = DirGraph::new();
        for (ty, id) in [("Person", 1u32), ("Paper", 2)] {
            let node = NodeData::new(
                Value::UniqueId(id),
                Value::String(format!("n{id}")),
                ty.to_string(),
                HashMap::from([("note".to_string(), Value::Int64(1))]),
                &mut graph.interner,
            );
            let idx = graph.graph.add_node(node);
            graph
                .type_indices
                .entry_or_default(ty.to_string())
                .push(idx);
        }
        graph
    }

    /// Renders on every axis that carries hints, so a hint added to one
    /// builder cannot escape the assertion by living in a track this test
    /// does not ask for.
    fn all_tracks(graph: &DirGraph, surface: DescribeSurface) -> String {
        let mut out = String::new();
        let requests = [
            DescribeRequest {
                ..DescribeRequest::new(surface)
            },
            DescribeRequest {
                connections: &ConnectionDetail::Overview,
                ..DescribeRequest::new(surface)
            },
            DescribeRequest {
                type_search: Some("n"),
                ..DescribeRequest::new(surface)
            },
            DescribeRequest {
                cypher: &CypherDetail::Overview,
                ..DescribeRequest::new(surface)
            },
            DescribeRequest {
                fluent: &FluentDetail::Overview,
                ..DescribeRequest::new(surface)
            },
        ];
        for request in requests {
            out.push_str(&compute_description(graph, &request).unwrap());
        }
        out
    }

    #[test]
    fn each_surface_is_told_its_own_overview_call() {
        let graph = two_type_graph();

        let python = all_tracks(&graph, DescribeSurface::Python);
        assert!(python.contains("describe(types="), "got: {python}");
        assert!(
            !python.contains("graph_overview("),
            "the Python surface must not name the MCP tool: {python}"
        );
        assert!(
            !python.contains("kglite describe"),
            "the Python surface must not name the CLI: {python}"
        );

        let mcp = all_tracks(&graph, DescribeSurface::Mcp);
        assert!(mcp.contains("graph_overview(types="), "got: {mcp}");
        assert!(
            !mcp.contains("kglite describe"),
            "the MCP surface must not name the CLI: {mcp}"
        );

        let cli = all_tracks(&graph, DescribeSurface::Cli);
        assert!(cli.contains("kglite describe GRAPH --types"), "got: {cli}");
        assert!(
            !cli.contains("graph_overview("),
            "the CLI must not name the MCP tool: {cli}"
        );
    }

    /// A hint is XML attribute content, so a surface spelling must not carry
    /// a character that would break the document an agent parses.
    #[test]
    fn no_surface_spelling_breaks_the_xml() {
        let graph = two_type_graph();
        for surface in [
            DescribeSurface::Python,
            DescribeSurface::Cli,
            DescribeSurface::Mcp,
        ] {
            let rendered = all_tracks(&graph, surface);
            for line in rendered.lines().filter(|l| l.contains("hint=\"")) {
                let hint = line.split("hint=\"").nth(1).unwrap();
                let hint = hint.split('"').next().unwrap();
                assert!(
                    !hint.contains('<') && !hint.contains('>'),
                    "hint attribute carries raw angle brackets: {line}"
                );
            }
        }
    }
}

/// A system label (`schema::SYSTEM_LABELS`) is ordinary data to Cypher but is
/// hidden from every enumerating surface. The counts rendered beside a listing
/// are part of the listing: a graph that gains a skill node must describe
/// byte-identically, or an invisible type has visibly moved the document.
#[cfg(test)]
mod system_label_tests {
    use super::*;
    use crate::datatypes::values::Value;
    use crate::graph::introspection::schema_overview::compute_schema;
    use crate::graph::introspection::{graph_scale, GraphScale};
    use crate::graph::schema::{EdgeData, NodeData};
    use crate::graph::storage::GraphWrite;
    use std::collections::HashMap;

    fn push_node(graph: &mut DirGraph, node_type: &str, id: u32, props: &[(&str, Value)]) {
        let node = NodeData::new(
            Value::UniqueId(id),
            Value::String(format!("{node_type}-{id}")),
            node_type.to_string(),
            props
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect::<HashMap<_, _>>(),
            &mut graph.interner,
        );
        let idx = graph.graph.add_node(node);
        graph
            .type_indices
            .entry_or_default(node_type.to_string())
            .push(idx);
    }

    fn person_graph() -> DirGraph {
        let mut graph = DirGraph::new();
        for id in [1u32, 2] {
            push_node(&mut graph, "Person", id, &[("age", Value::Int64(30))]);
        }
        graph
    }

    fn with_skill() -> DirGraph {
        let mut graph = person_graph();
        push_node(
            &mut graph,
            "KgliteSkill",
            9,
            &[
                ("name", Value::String("cypher_query".into())),
                ("description", Value::String("how to query".into())),
                ("body", Value::String("# Body".into())),
                (
                    "references_tools",
                    Value::List(vec![Value::String("cypher_query".into())]),
                ),
                ("delivery", Value::String("lazy".into())),
            ],
        );
        graph
    }

    fn describe(graph: &DirGraph) -> String {
        compute_description(graph, &DescribeRequest::new(DescribeSurface::Python)).unwrap()
    }

    #[test]
    fn node_type_enumeration_omits_a_system_label() {
        let graph = with_skill();
        assert!(
            !graph.get_node_types().contains(&"KgliteSkill".to_string()),
            "got: {:?}",
            graph.get_node_types()
        );
        assert!(graph.get_node_types().contains(&"Person".to_string()));
        assert!(
            graph.has_node_type("KgliteSkill"),
            "the internal predicate stays honest — four write-path guards depend on it"
        );
    }

    /// Every reserved label, not just the one the fixtures use. The membership
    /// assertion is what makes the loop non-vacuous: dropping a name from
    /// `SYSTEM_LABELS` fails here rather than quietly shrinking the loop.
    #[test]
    fn every_system_label_is_hidden_from_the_enumeration() {
        use crate::graph::schema::SYSTEM_LABELS;
        for expected in ["KgliteSkill", "KgliteRecipe"] {
            assert!(
                SYSTEM_LABELS.contains(&expected),
                "{expected} is not reserved"
            );
        }
        for label in SYSTEM_LABELS {
            let mut graph = person_graph();
            push_node(&mut graph, label, 9, &[("name", Value::String("x".into()))]);
            assert!(
                !graph.get_node_types().contains(&label.to_string()),
                "{label} leaked into the type enumeration"
            );
            assert!(
                !describe(&graph).contains(label),
                "{label} leaked into describe()"
            );
        }
    }

    /// A skill node is invisible to the description *as data* — it adds no
    /// type, no count and no sample. The one thing it adds is the `<skills>`
    /// index, so the two documents differ by exactly that block and nothing
    /// else.
    #[test]
    fn a_skill_node_changes_describe_only_by_the_skills_index() {
        let skilled = describe(&with_skill());
        let (before, rest) = skilled.split_once("  <skills ").expect("a skills index");
        let (index, after) = rest.split_once("  </skills>\n").expect("a closed index");
        assert!(index.contains("name=\"cypher_query\""), "got: {index}");
        assert_eq!(format!("{before}{after}"), describe(&person_graph()));
    }

    #[test]
    fn schema_overview_omits_a_system_label_and_its_nodes() {
        let plain = compute_schema(&person_graph());
        let skilled = compute_schema(&with_skill());
        assert!(!skilled.node_types.iter().any(|(nt, _)| nt == "KgliteSkill"));
        assert_eq!(
            skilled.node_count, plain.node_count,
            "the total beside the listing must move with the listing"
        );
    }

    #[test]
    fn graph_scale_ignores_system_labels() {
        // 15 core types is the Small/Medium boundary: a 16th *visible* type
        // flips the tier, so an invisible one must not.
        let mut graph = DirGraph::new();
        for i in 0..15u32 {
            push_node(&mut graph, &format!("T{i}"), i, &[]);
        }
        assert_eq!(graph_scale(&graph), GraphScale::Small);
        push_node(&mut graph, "KgliteSkill", 99, &[]);
        assert_eq!(graph_scale(&graph), GraphScale::Small);
    }

    #[test]
    fn a_skill_node_is_never_reported_as_a_disconnected_type() {
        // Exploration hints need >= 2 types and >= 1 edge to render at all.
        let mut graph = DirGraph::new();
        push_node(&mut graph, "Person", 1, &[]);
        push_node(&mut graph, "Person", 2, &[]);
        push_node(&mut graph, "Company", 3, &[]);
        let (a, b) = (
            graph
                .type_indices
                .get("Person")
                .unwrap()
                .iter()
                .next()
                .unwrap(),
            graph
                .type_indices
                .get("Company")
                .unwrap()
                .iter()
                .next()
                .unwrap(),
        );
        graph.graph.add_edge(
            a,
            b,
            EdgeData::new("WORKS_AT".to_string(), HashMap::new(), &mut graph.interner),
        );
        push_node(&mut graph, "KgliteSkill", 9, &[]);
        let rendered = describe(&graph);
        assert!(
            !rendered.contains("KgliteSkill"),
            "hidden type advertised as disconnected: {rendered}"
        );
    }
}

/// A sample node renders `id` and `title` as attributes from the canonical
/// projection. A stored property spelled the same way used to be appended a
/// second time, producing a duplicate XML attribute — a document no parser
/// accepts.
#[cfg(test)]
mod sample_attribute_collision_tests {
    use super::*;
    use crate::datatypes::values::{ColumnData, ColumnType, DataFrame};
    use crate::graph::mutation::maintain::add_nodes;

    fn doc_graph(id_field: &str, title_field: &str) -> DirGraph {
        let mut graph = DirGraph::new();
        let mut df = DataFrame::new(Vec::new());
        for (name, value) in [
            ("code", "c1"),
            ("id", "i1"),
            ("name", "Nan"),
            ("title", "Ann"),
            ("city", "Oslo"),
        ] {
            df.add_column(
                name.to_string(),
                ColumnType::String,
                ColumnData::String(vec![Some(value.to_string())]),
            )
            .expect("column");
        }
        add_nodes(
            &mut graph,
            df,
            "Doc".to_string(),
            id_field.to_string(),
            Some(title_field.to_string()),
            None,
        )
        .expect("add nodes");
        graph
    }

    fn sample_line(graph: &DirGraph) -> String {
        let rendered =
            compute_description(graph, &DescribeRequest::new(DescribeSurface::Python)).unwrap();
        rendered
            .lines()
            .find(|l| l.contains("<node "))
            .unwrap_or_else(|| panic!("no sample rendered in: {rendered}"))
            .to_string()
    }

    fn attribute_names(line: &str) -> Vec<&str> {
        line.split(' ')
            .filter_map(|tok| tok.split_once('='))
            .map(|(k, _)| k)
            .collect()
    }

    #[test]
    fn a_stored_title_property_does_not_duplicate_the_title_attribute() {
        let line = sample_line(&doc_graph("code", "name"));
        let names = attribute_names(&line);
        assert_eq!(
            names.iter().filter(|n| **n == "title").count(),
            1,
            "duplicate attribute in: {line}"
        );
        assert!(line.contains("title=\"Nan\""), "got: {line}");
    }

    #[test]
    fn a_stored_id_property_does_not_duplicate_the_id_attribute() {
        let line = sample_line(&doc_graph("code", "title"));
        let names = attribute_names(&line);
        assert_eq!(
            names.iter().filter(|n| **n == "id").count(),
            1,
            "duplicate attribute in: {line}"
        );
        assert!(line.contains("id=\"c1\""), "got: {line}");
    }

    /// The collision is only between the two identity attributes and stored
    /// keys spelled the same; every other property still reaches the preview.
    #[test]
    fn other_properties_still_render() {
        let line = sample_line(&doc_graph("code", "name"));
        assert!(line.contains("city=\"Oslo\""), "got: {line}");
    }
}

#[cfg(test)]
mod mixed_property_type_tests {
    //! A property written with two value types describes as `mixed`, not as
    //! the type of its last write (a blank-slate user test saw 7,000 string
    //! `context` values render `context:Int64` after one `CREATE`).
    use super::*;
    use crate::graph::algorithms::Interrupt;
    use crate::graph::languages::cypher::executor::write::execute_mutable;
    use crate::graph::languages::cypher::parser::parse_cypher;
    use std::collections::HashMap;

    fn run(graph: &mut DirGraph, query: &str) {
        let parsed = parse_cypher(query).unwrap();
        execute_mutable(graph, &parsed, HashMap::new(), Interrupt::default()).unwrap();
    }

    fn loaded() -> DirGraph {
        let mut graph = DirGraph::new();
        run(
            &mut graph,
            "CREATE (a:D {id: 1, ctx: 's'}), (b:D {id: 2, ctx: 's'}), \
             (a)-[:R {context: 'x'}]->(b), (b)-[:R {context: 'y'}]->(a)",
        );
        graph
    }

    fn describe(graph: &DirGraph, request: DescribeRequest) -> String {
        compute_description(graph, &request).unwrap()
    }

    fn overview(graph: &DirGraph) -> String {
        describe(graph, DescribeRequest::new(DescribeSurface::Python))
    }

    fn connection_detail(graph: &DirGraph) -> String {
        describe(
            graph,
            DescribeRequest {
                connections: &ConnectionDetail::Topics(vec!["R".to_string()]),
                ..DescribeRequest::new(DescribeSurface::Python)
            },
        )
    }

    fn node_detail(graph: &DirGraph) -> String {
        let types = vec!["D".to_string()];
        describe(
            graph,
            DescribeRequest {
                types: Some(&types),
                ..DescribeRequest::new(DescribeSurface::Python)
            },
        )
    }

    #[test]
    fn one_outlier_relationship_types_the_connection_property_mixed() {
        let mut graph = loaded();
        assert!(overview(&graph).contains("properties=\"context:String\""));
        run(
            &mut graph,
            "MATCH (a:D {id: 1}), (b:D {id: 2}) CREATE (a)-[:R {context: 42}]->(b)",
        );
        let described = overview(&graph);
        assert!(
            described.contains("properties=\"context:mixed\""),
            "got: {described}"
        );
        let detail = connection_detail(&graph);
        assert!(
            detail.contains("<prop name=\"context\" type=\"mixed\""),
            "got: {detail}"
        );
    }

    #[test]
    fn one_outlier_node_types_the_node_property_mixed() {
        let mut graph = loaded();
        run(&mut graph, "MATCH (n:D {id: 1}) SET n.ctx = 42");
        let detail = node_detail(&graph);
        assert!(
            detail.contains("<prop name=\"ctx\" type=\"mixed\""),
            "got: {detail}"
        );
    }

    #[test]
    fn a_column_rewritten_to_one_type_keeps_that_type() {
        let mut graph = loaded();
        run(&mut graph, "MATCH (n:D) SET n.ctx = 42");
        let detail = node_detail(&graph);
        assert!(
            detail.contains("<prop name=\"ctx\" type=\"Int64\""),
            "got: {detail}"
        );
    }
}
