//! Re-resolve the skill layer after a workspace root activation.
//!
//! A workspace-mode server has no graph when the prompt plane freezes, so
//! every bundled skill carrying `applies_when: graph_has_node_type:` resolves
//! false at boot — including `code_graph_analysis`, `code_graph_views` and
//! `read_code_source`, whose predicate is `[Function, Class]`. Until this
//! module existed nothing ever re-evaluated them, so in every deployment that
//! builds its graph from a root (codingest-mcp's local and github workspaces)
//! the server advertised code-graph methodology it never served.
//!
//! The fix is one call, at one place: after the framework's own activation
//! tool has returned. `set_root_dir` and `repo_management` are registered by
//! mcp-methods, not here, so this wraps their routes rather than
//! re-implementing them — the wrapper keeps the framework's `Tool` attribute
//! verbatim, so the description and input schema the agent sees are the
//! framework's, unchanged.

use mcp_methods::server::McpServer;
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::model::CallToolResponse;

use crate::skills::SkillRefresher;

/// The framework tool routes that publish a new workspace graph.
///
/// `set_root_dir` for `workspace.kind: local`, `repo_management` for
/// `kind: github`; mcp-methods registers exactly one of the two per
/// deployment and neither outside a workspace, so a name that is not in the
/// router is skipped rather than being an error.
const ACTIVATION_TOOLS: [&str; 2] = ["set_root_dir", "repo_management"];

/// Make a successful activation re-resolve the skill layer.
///
/// Call after `McpServer::new` has registered the framework's routes and
/// before `apply_bundled_tool_overrides` settles their final names: the
/// wrapper replaces the route under its original name, so an operator rename
/// applied afterwards renames the wrapper, while one applied first would hide
/// the route from the lookup here.
///
/// `skills` may still be unarmed at this point — [`SkillRefresher`] is a
/// late-filled slot, and `boot_skills` arms it after every route source has
/// registered. The wrapper holds a clone of the slot, not of its contents.
pub(crate) fn refresh_skills_after_activation(server: &mut McpServer, skills: SkillRefresher) {
    for name in ACTIVATION_TOOLS {
        let existing = server.tool_router_mut().map.get(name).cloned();
        let Some(existing) = existing else {
            continue;
        };
        let attr = existing.attr.clone();
        let skills = skills.clone();
        let wrapped = ToolRoute::new_dyn(attr, move |context| {
            let call = existing.call.clone();
            let skills = skills.clone();
            Box::pin(async move {
                let outcome = (*call)(context).await;
                // Deliberately after the await, not inside the activation:
                // by here mcp-methods has released the activation and
                // `root_swap` locks and `commit_workspace_graph` has released
                // the graph's own write lock, so the recomposition can read
                // the graph that was just published. See `SkillRefresher::
                // refresh` for what the two earlier sites cost.
                if published_a_graph(&outcome) {
                    skills.refresh();
                }
                outcome
            })
        });
        server.tool_router_mut().add_route(wrapped);
    }
}

/// Whether the activation call the wrapper just awaited got far enough to be
/// worth re-resolving for.
///
/// Both tools render their own failures as ordinary prose, so a refused root
/// and a built one are both `Ok(Complete)` with `is_error` unset — this only
/// screens out the framework's own error envelope (an undeserialisable
/// argument) and the non-`Complete` responses, where no handler ran at all.
/// A recomposition after a refused activation is harmless: the graph did not
/// change, so it resolves the set that is already injected.
fn published_a_graph(outcome: &Result<CallToolResponse, rmcp::ErrorData>) -> bool {
    matches!(outcome, Ok(CallToolResponse::Complete(result)) if result.is_error != Some(true))
}
