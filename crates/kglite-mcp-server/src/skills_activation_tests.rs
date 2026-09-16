//! Predicate re-resolution after a workspace root activation.
//!
//! The defect these pin: a workspace-mode server has no graph when the prompt
//! plane freezes, so `code_graph_analysis`, `code_graph_views` and
//! `read_code_source` — all gated on
//! `applies_when: graph_has_node_type: [Function, Class]` — resolve false at
//! boot, and before [`SkillRefresher`] was armed there nothing ever asked
//! again. Every codingest deployment advertised code-graph methodology it
//! never served.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use mcp_methods::server::{McpServer, ServerOptions};
use schemars::JsonSchema;
use serde::Deserialize;

use super::*;
use crate::tools::{GraphState, PeerSlot, SkillsIndexSlot};

/// The bundled skill whose predicate is the one the fix exists for.
const GATED_SKILL: &str = "code_graph_analysis";

#[derive(Default, Deserialize, JsonSchema)]
struct EmptyArgs {}

/// A producer that emits the node type the bundled code-graph skills gate on.
fn function_hooks() -> WorkspaceGraphHooks {
    WorkspaceGraphHooks {
        build: Box::new(|_request| {
            let mut graph = kglite::api::storage::new_dir_graph_in_mode(
                kglite::api::storage::StorageMode::Memory,
                None,
            )
            .map_err(|error| error.to_string())?;
            let params = HashMap::new();
            let options = kglite::api::session::ExecuteOptions::eager(&params);
            kglite::api::session::execute_mut(
                &mut graph,
                "CREATE (:Function {id: 'fixture::run'})",
                &options,
            )
            .map_err(|error| error.to_string())?;
            Ok(WorkspaceGraphResult::new(Arc::new(graph)))
        }),
        is_relevant: Box::new(|_| true),
    }
}

/// One producer record, which is what turns the skill plane on for a
/// manifest-less deployment — the codingest shape, and the only way to reach
/// the bundled layer without writing a manifest.
fn producer() -> ProducerSkills {
    ProducerSkills::build(&[SkillRecord {
        name: "fixture_methodology".to_string(),
        description: "How this builder's graphs are shaped.".to_string(),
        body: "Every graph carries `:Function` nodes keyed by `id`.\n".to_string(),
        references_tools: vec!["cypher_query".to_string()],
        delivery: kglite::api::skills::Delivery::Lazy,
    }])
    .expect("a valid producer record")
}

/// A server carrying the tools the bundled code-graph skills reference, so a
/// skill that activates has somewhere to attach.
fn server_with_graph_tools() -> McpServer {
    let mut server = McpServer::new(ServerOptions::default());
    for name in ["cypher_query", "graph_overview", "explore"] {
        server.register_typed_tool::<EmptyArgs, _>(name, "base", |_| "ok".to_string());
    }
    server
}

fn active_names(server: &McpServer) -> Vec<String> {
    server
        .active_skills()
        .into_iter()
        .map(|skill| skill.name)
        .collect()
}

/// Boot a manifest-less workspace-mode server with a producer layer, and arm
/// the refresher exactly as `boot_skills` does.
fn boot(
    server: &mut McpServer,
    mode: &Mode,
    state: &GraphState,
) -> (SkillRefresher, SkillsIndexSlot) {
    let producer = producer();
    let index = SkillsIndexSlot::default();
    let peer = PeerSlot::default();
    install_skills(server, None, &producer, mode, state, None, &index).expect("install skills");
    let refresher = SkillRefresher::default();
    refresher.arm(RefreshInputs {
        reloader: server.skill_reloader(),
        manifest: None,
        producer: &producer,
        mode,
        graph_state: state,
        recipe_catalog_summary: None,
        skills_index: &index,
        peer: &peer,
    });
    (refresher, index)
}

/// The fix, at the level of the refresher: a predicate that was false against
/// no graph is true against the one the first activation published, and the
/// rebuild is what asks again.
///
/// Mutation: narrow [`SkillRefresher::arm`]'s gate back to
/// `Mode::Graph | Mode::Watch` — the refresher stays unarmed, `refresh()`
/// returns on its `None` slot, and the second assertion fails with the skill
/// still absent.
#[test]
fn a_predicate_gated_skill_is_dead_at_workspace_boot_and_lives_after_the_first_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mode = Mode::LocalWorkspace {
        root: temp.path().to_path_buf(),
        watch: false,
    };
    let state = GraphState::new(Some(WorkspaceGraphMode::LocalWorkspace))
        .with_workspace_graph(Some(Arc::new(function_hooks())));
    let mut server = server_with_graph_tools();
    let (refresher, index) = boot(&mut server, &mode, &state);

    assert!(
        !state.has_node_type("Function"),
        "a workspace mode has no graph until a root is activated"
    );
    assert!(
        !active_names(&server).iter().any(|name| name == GATED_SKILL),
        "the gate must be closed at boot: {:?}",
        active_names(&server)
    );
    assert!(
        read_lock(&index)
            .as_deref()
            .is_none_or(|rendered| !rendered.contains(GATED_SKILL)),
        "the overview index must not advertise a suppressed skill"
    );

    state
        .build_workspace_graph(temp.path(), None)
        .expect("publish the activation graph");
    refresher.refresh();

    assert!(state.has_node_type("Function"));
    assert!(
        active_names(&server).iter().any(|name| name == GATED_SKILL),
        "the first graph must open the gate: {:?}",
        active_names(&server)
    );
    assert!(
        read_lock(&index)
            .as_deref()
            .expect("an index once skills are active")
            .contains(GATED_SKILL),
        "the overview index must move with the active set"
    );
}

/// Where the refresh runs is the whole design, so it is pinned by a test that
/// can only fail by hanging.
///
/// `Workspace::set_root_dir` holds mcp-methods' activation write lock and its
/// `root_swap` lock across the commit closure, and
/// `GraphState::commit_workspace_graph` holds the active-graph write lock
/// inside it. `SkillRefresher::refresh` recomposes the registry, which reads
/// the active graph through `with_kg` — a `std::sync::RwLock` read on a lock
/// the commit still holds for writing. Calling it from either site therefore
/// deadlocks the server permanently rather than returning a wrong answer, and
/// the only safe site is the one
/// [`crate::tools::refresh_skills_after_activation`] uses: the tool handler,
/// after the framework's own call has returned.
///
/// Mutation: move the `refresher.refresh()` below into the commit closure in
/// `activation::workspace_activation_transaction` (or to its tail, inside
/// `PreparedActivation::new`) — this test stops finishing and the watchdog
/// below fails it instead of hanging the suite.
#[test]
fn the_refresh_runs_after_set_root_dir_returns_never_inside_its_locks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let mode = Mode::LocalWorkspace {
        root: root.clone(),
        watch: false,
    };
    let state = GraphState::new(Some(WorkspaceGraphMode::LocalWorkspace))
        .with_workspace_graph(Some(Arc::new(function_hooks())));
    let mut server = server_with_graph_tools();
    let (refresher, _index) = boot(&mut server, &mode, &state);
    assert!(!active_names(&server).iter().any(|name| name == GATED_SKILL));

    let workspace = local_workspace(root.clone(), &state, None).expect("local workspace");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        // Exactly the sequence `refresh_skills_after_activation` produces: the
        // framework's handler runs to completion, then the refresh.
        let output = workspace.set_root_dir(Path::new(&root), None);
        refresher.refresh();
        let _ = done_tx.send(output);
    });
    let output = done_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the activation-then-refresh sequence deadlocked");
    worker.join().expect("activation thread");

    assert!(output.contains("Graph ready: 1 nodes"), "{output}");
    assert!(
        active_names(&server).iter().any(|name| name == GATED_SKILL),
        "a real activation must open the gate: {:?}",
        active_names(&server)
    );
}
