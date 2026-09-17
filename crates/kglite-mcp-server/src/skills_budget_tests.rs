//! The session skill budget (mcp-methods `SESSION_TOTAL_LIMIT_BYTES`).
//!
//! The framework charges the budget at *resolve* time, over every resolved
//! body, before any `applies_when:` gate is evaluated — so a skill that can
//! never activate here still costs its bytes. These pin what each deployment
//! shape resolves; [`super::compose_registry`] is what keeps them true by not
//! bundling a skill whose gate it can already read.

use super::*;
use mcp_methods::server::library_bundled_skills;
use mcp_methods::server::skills::parse_skill;
use std::path::{Path, PathBuf};

/// Bundled unconditionally — every deployment pays for these.
const ALWAYS: [(&str, &str); 4] = [
    ("cypher_query", include_str!("../skills/cypher_query.md")),
    (
        "graph_overview",
        include_str!("../skills/graph_overview.md"),
    ),
    ("save_graph", include_str!("../skills/save_graph.md")),
    ("fetch_images", include_str!("../skills/fetch_images.md")),
];

/// Bundled only where the graph carries `Function` / `Class`.
const CODE: [(&str, &str); 4] = [
    (
        "read_code_source",
        include_str!("../skills/read_code_source.md"),
    ),
    ("explore", include_str!("../skills/explore.md")),
    (
        "code_graph_analysis",
        include_str!("../skills/code_graph_analysis.md"),
    ),
    (
        "code_graph_views",
        include_str!("../skills/code_graph_views.md"),
    ),
];

const VAULT: &str = include_str!("../skills/vault_authoring.md");
const RECIPES: &str = include_str!("../skills/recipe_queries.md");

fn body_bytes(name: &str, text: &str) -> usize {
    parse_skill(text, Path::new(name))
        .unwrap_or_else(|e| panic!("bundled skill `{name}` does not parse: {e}"))
        .1
        .len()
}

fn group(skills: &[(&str, &str)]) -> usize {
    skills
        .iter()
        .map(|(name, body)| body_bytes(name, body))
        .sum()
}

/// What the framework charges before this crate adds a byte — read out of
/// mcp-methods rather than pinned, because a dependency bump is how this
/// number moves without anyone editing a skill here.
fn framework_bytes() -> usize {
    library_bundled_skills()
        .iter()
        .map(|skill| body_bytes(skill.name, skill.body))
        .sum()
}

/// The bytes a deployment of this shape resolves.
fn shape(code: bool, vault: bool, recipes: bool) -> usize {
    framework_bytes()
        + group(&ALWAYS)
        + if code { group(&CODE) } else { 0 }
        + if vault {
            body_bytes("vault_authoring", VAULT)
        } else {
            0
        }
        + if recipes {
            body_bytes("recipe_queries", RECIPES)
        } else {
            0
        }
}

/// The composition a deployment actually resolves, through the real
/// builder — the arithmetic above only matches because
/// [`compose_registry`] bundles by the same gates.
fn composed_bytes(mode: &Mode) -> usize {
    let state = GraphState::new(None);
    let (registry, _stats) = compose_registry(None, &[], mode, &state, None);
    registry
        .expect("the bundled layers resolve with no manifest")
        .total_body_bytes()
}

/// The document shapes this binary ships for — a `--graph` server over a
/// vault-built or domain graph, and `--vault` itself — each leaving the
/// operator's own `skills:` directory room in the same budget.
///
/// These are the shapes that were *over* the limit: a plain Petrel
/// `--graph` manifest resolved 67 624 B against 65 536 B, because every
/// resolved body is charged whatever `applies_when:` later says.
///
/// 8 KiB free is two operator skills at the framework's 4 KiB soft
/// per-skill limit — the smallest margin that still lets an operator add
/// anything without going over.
///
/// Mutation: paste any bundled skill's body into another one.
#[test]
fn a_document_deployment_keeps_headroom() {
    let ceiling = SESSION_TOTAL_LIMIT_BYTES - 8 * 1024;
    for (name, arithmetic) in [
        ("a --graph deployment", shape(false, false, false)),
        (
            "a --graph deployment carrying recipes",
            shape(false, false, true),
        ),
        ("a --vault deployment", shape(false, true, false)),
        (
            "a --vault deployment carrying recipes",
            shape(false, true, true),
        ),
    ] {
        assert!(
            arithmetic <= ceiling,
            "{name} resolves {arithmetic} B, leaving less than 8 KiB under the \
             {SESSION_TOTAL_LIMIT_BYTES} B session limit for the operator's own skills"
        );
    }
    // And the real builder agrees with the arithmetic, so the shapes above
    // are the shapes that ship.
    assert_eq!(
        composed_bytes(&Mode::Graph {
            path: PathBuf::from("graph.kgl"),
        }),
        shape(false, false, false)
    );
    assert_eq!(
        composed_bytes(&Mode::Vault {
            dir: PathBuf::from("vault"),
        }),
        shape(false, true, false)
    );
}

/// A code deployment carries four more skills and still fits.
#[test]
fn a_code_deployment_fits_the_session_budget() {
    let total = shape(true, false, false);
    assert!(
        total < SESSION_TOTAL_LIMIT_BYTES,
        "a code deployment resolves {total} B against the {SESSION_TOTAL_LIMIT_BYTES} B \
         session limit ({} B of it from mcp-methods)",
        framework_bytes()
    );
}

/// The fullest composition — a vault whose graph also carries code types,
/// *and* a recipe catalog — is still over the limit, and this pins it so
/// it cannot grow. Nothing is dropped over the limit and nothing is
/// truncated (see [`warn_if_over_session_budget`]); the cost is context.
///
/// Red if it grows, and red if it goes *under*: at that point the shape
/// belongs in the tests above and this one should be deleted.
#[test]
fn the_fullest_composition_is_over_the_limit_and_pinned_there() {
    let total = shape(true, true, true);
    assert!(
        total > SESSION_TOTAL_LIMIT_BYTES,
        "{total} B now fits the {SESSION_TOTAL_LIMIT_BYTES} B budget — fold this shape \
         into `a_code_deployment_fits_the_session_budget` and delete this test"
    );
    let ratchet = SESSION_TOTAL_LIMIT_BYTES * 21 / 20;
    assert!(
        total <= ratchet,
        "{total} B is more than 5 % over the {SESSION_TOTAL_LIMIT_BYTES} B budget \
         ({ratchet} B). A code deployment carrying recipes is the reachable half of \
         this shape; trim a skill rather than widen the band."
    );
}
