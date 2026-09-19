//! Tests for [`super`] — registry composition and its refusals, the discovery
//! steer, the recipe-catalogue skill, the bundled skill bodies, and the
//! producer/graph skill layers.

use super::*;

#[cfg(test)]
mod registry_failure_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_missing_declared_pack_refuses_the_boot_and_names_both_spellings() {
        // Both spellings, because the operator wrote one and the filesystem
        // holds the other: a `./domain` that resolved somewhere unexpected is
        // the same typo as a `./domian` that resolved exactly where asked.
        let error = SkillError::PathNotFound {
            raw: "./domain".to_string(),
            resolved: PathBuf::from("/srv/deploy/domain"),
        };

        let message = report_registry_failure(error, Some(Path::new("/srv/deploy/graph_mcp.yaml")))
            .expect_err("a declared pack that is not there must fail boot")
            .to_string();

        assert!(message.contains("./domain"), "{message}");
        assert!(message.contains("/srv/deploy/domain"), "{message}");
        assert!(message.contains("/srv/deploy/graph_mcp.yaml"), "{message}");
    }

    #[test]
    fn a_content_fault_in_a_file_that_exists_stays_a_warning() {
        // The file is there and the log names it; refusing the whole
        // deployment over one malformed skill is the worse trade.
        let error = SkillError::MissingFrontmatter {
            path: PathBuf::from("/srv/deploy/domain/broken.md"),
        };

        assert!(
            report_registry_failure(error, Some(Path::new("/srv/deploy/graph_mcp.yaml"))).is_ok()
        );
    }
}

#[cfg(test)]
mod discovery_steer_tests {
    use super::*;
    use std::path::PathBuf;

    use mcp_methods::server::ServerOptions;

    fn ws_mode() -> Mode {
        Mode::LocalWorkspace {
            root: PathBuf::from("/tmp/ws"),
            watch: false,
        }
    }

    #[test]
    fn appends_to_workspace_modes() {
        let out = apply_discovery_steer(&ws_mode(), ServerOptions::default());
        let text = out.instructions.expect("instructions set");
        assert!(text.contains("ALWAYS registered"));
        assert!(text.contains("cypher"));
    }

    #[test]
    fn preserves_manifest_instructions() {
        let opts = ServerOptions {
            instructions: Some("Domain guidance here.".to_string()),
            ..Default::default()
        };
        let out = apply_discovery_steer(&ws_mode(), opts);
        let text = out.instructions.expect("instructions set");
        assert!(text.starts_with("Domain guidance here."));
        assert!(text.contains("ALWAYS registered"));
    }

    #[test]
    fn dedupes_when_already_present() {
        let opts = ServerOptions {
            instructions: Some(
                "graph_overview and cypher_query are ALWAYS registered.".to_string(),
            ),
            ..Default::default()
        };
        let out = apply_discovery_steer(&ws_mode(), opts);
        let text = out.instructions.expect("instructions set");
        // Only the manifest's own copy — not appended a second time.
        assert_eq!(text.matches("ALWAYS registered").count(), 1);
    }

    #[test]
    fn skips_non_workspace_modes() {
        let mode = Mode::Graph {
            path: PathBuf::from("/tmp/g.kgl"),
        };
        let out = apply_discovery_steer(&mode, ServerOptions::default());
        assert!(out.instructions.is_none());
    }
}

#[cfg(test)]
mod recipe_skill_tests {
    use super::*;

    use mcp_methods::server::{serve_prompts, McpServer, ServerOptions, SkillRegistry};

    use mcp_methods::server::{SkillSource, SkillsSource};
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Default, Deserialize, JsonSchema)]
    struct EmptyArgs {}

    fn catalog(raw: Option<&serde_json::Value>) -> recipe_queries::RecipeCatalog {
        recipe_queries::RecipeCatalog::from_manifest_value(raw).expect("valid catalog")
    }

    fn present_catalog() -> recipe_queries::RecipeCatalog {
        catalog(Some(&json!({
            "review": {
                "description": "Review operations.",
                "queries": {
                    "lookup": {
                        "description": "Look up one value.",
                        "parameters": {
                            "type": "object",
                            "properties": {},
                            "required": [],
                            "additionalProperties": false
                        },
                        "cypher": "RETURN 1 AS value"
                    }
                }
            }
        })))
    }

    fn resolved_recipe_skills(
        summary: Option<recipe_queries::CatalogSummary>,
        source: &SkillsSource,
    ) -> mcp_methods::server::ResolvedRegistry {
        add_recipe_query_skill(SkillRegistry::new(), summary)
            .layer_dirs(source, std::path::Path::new("manifest.yaml"))
            .expect("configure skill layers")
            .finalise()
            .expect("resolve recipe skill")
    }

    fn bundled_source() -> SkillsSource {
        SkillsSource::Sources(vec![SkillSource::Bundled])
    }

    #[test]
    fn recipe_skill_is_conditionally_bundled_for_present_catalog_only() {
        let source = bundled_source();
        let absent = catalog(None);
        let empty_value = json!({});
        let empty = catalog(Some(&empty_value));
        let present = present_catalog();

        for summary in [absent.discovery_summary(), empty.discovery_summary()] {
            let registry = resolved_recipe_skills(summary, &source);
            assert!(!registry
                .skill_names()
                .iter()
                .any(|name| name == "recipe_queries"));
        }
        let registry = resolved_recipe_skills(present.discovery_summary(), &source);
        assert!(registry
            .skill_names()
            .iter()
            .any(|name| name == "recipe_queries"));

        let disabled = resolved_recipe_skills(present.discovery_summary(), &SkillsSource::Disabled);
        assert!(
            disabled.is_empty(),
            "a present catalog must not bypass the manifest skills opt-in"
        );
    }

    #[test]
    fn recipe_skill_contract_names_direct_discovery_preflight_and_raw_fallback() {
        let source = bundled_source();
        let registry = resolved_recipe_skills(present_catalog().discovery_summary(), &source);
        let skill = registry.get("recipe_queries").expect("recipe skill");

        assert_eq!(
            skill.frontmatter.references_tools,
            ["list_recipe_queries", "run_recipe_query", "cypher_query"]
        );
        let applies = skill
            .frontmatter
            .applies_when
            .as_ref()
            .expect("tool registration gate");
        assert_eq!(
            applies.tool_registered.as_deref(),
            Some(recipe_queries::RUN_RECIPE_QUERY_TOOL)
        );
        assert_eq!(applies.extension_enabled, None);
        let body = skill.body.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(body.contains("Do not call `list_recipe_queries` first"));
        // A query with its own route is the cheapest path of all, and the
        // block marks which ones have one.
        assert!(body.contains("The query has its own tool: call it directly"));
        assert!(body.contains("marked `→ tool: <name>`"));
        assert!(body.contains("domain skill already selected the"));
        // The discovery step is the catalogue block the run tool now
        // publishes, not a listing round trip.
        assert!(body.contains("catalogue block in `run_recipe_query`'s own description"));
        assert!(body.contains("`list_recipe_queries(recipe=...)` only when"));
        assert!(body.contains("exactly matches the requested scope"));
        assert!(body.contains("mandatory `resolve_*` preflight"));
        assert!(body.contains("fall back to raw `cypher_query`"));
    }

    #[test]
    fn tool_registered_gate_beats_truthy_empty_extension_and_injects_all_references() {
        let source = bundled_source();
        let registry = resolved_recipe_skills(present_catalog().discovery_summary(), &source);
        let mut without_route = McpServer::new(ServerOptions {
            extensions: serde_json::Map::from_iter([("cypher_recipes".to_string(), json!({}))]),
            ..ServerOptions::default()
        });
        serve_prompts(&registry, &mut without_route);
        assert!(
            without_route
                .prompt_router_mut()
                .list_all()
                .iter()
                .all(|prompt| prompt.name != "recipe_queries"),
            "an empty extension mapping is truthy but must not activate the skill"
        );

        let registry = resolved_recipe_skills(present_catalog().discovery_summary(), &source);
        let mut with_routes = McpServer::new(ServerOptions::default());
        for name in [
            recipe_queries::LIST_RECIPE_QUERIES_TOOL,
            recipe_queries::RUN_RECIPE_QUERY_TOOL,
            "cypher_query",
        ] {
            with_routes.register_typed_tool::<EmptyArgs, _>(name, "base", |_| "ok".to_string());
        }
        serve_prompts(&registry, &mut with_routes);

        assert!(with_routes
            .prompt_router_mut()
            .list_all()
            .iter()
            .any(|prompt| prompt.name == "recipe_queries"));
        for name in [
            recipe_queries::LIST_RECIPE_QUERIES_TOOL,
            recipe_queries::RUN_RECIPE_QUERY_TOOL,
            "cypher_query",
        ] {
            let router = with_routes.tool_router_mut();
            let description = router
                .get(name)
                .and_then(|tool| tool.description.as_deref())
                .expect("tool description");
            assert!(
                description.contains("mcp-skill:recipe_queries"),
                "{name} missing recipe methodology injection"
            );
        }
    }
}

#[cfg(test)]
mod bundled_skill_body_tests {
    /// The unconditionally bundled skills ship to every deployment — shipping, legal, maritime — with no `applies_when` gate,
    /// because a graph and a Cypher query are what they are about. Code-graph
    /// *methodology* is not: `cypher_query` opened with the four-step
    /// code-graph workflow and "Never `grep` for a definition", ~1.5k tokens
    /// of instruction about a codebase delivered verbatim to a graph of
    /// vessels, and `graph_overview` carried the same preamble until the
    /// vault skill needed the bytes. That content already lives in
    /// `code_graph_analysis`, `explore` and `read_code_source`, each gated on
    /// `graph_has_node_type: [Function, Class]`, so every reader it is for
    /// still gets it.
    #[test]
    fn the_always_bundled_skills_carry_no_code_graph_preamble() {
        for (name, body) in [
            ("cypher_query", include_str!("../skills/cypher_query.md")),
            (
                "graph_overview",
                include_str!("../skills/graph_overview.md"),
            ),
            ("save_graph", include_str!("../skills/save_graph.md")),
            ("fetch_images", include_str!("../skills/fetch_images.md")),
        ] {
            for marker in [
                "Never `grep`",
                "Code-graph workflow",
                "read_code_source(qualified_name=…)",
            ] {
                assert!(
                    !body.contains(marker),
                    "{name}.md still carries code-graph methodology: {marker:?}"
                );
            }
        }
        // The gated skill is where it belongs, and still has it.
        let gated = include_str!("../skills/code_graph_analysis.md");
        assert!(gated.contains("Never `grep`"));
        assert!(gated.contains("Code-graph workflow"));
    }

    /// Two things the skill previously got wrong about its own tool: it said
    /// `$name` parameters "aren't currently exposed" (they are, via `params`),
    /// and it presented `FORMAT CSV` as the way to get a large result out
    /// while the inline body is capped at 200 rows.
    #[test]
    fn the_generic_cypher_skill_states_the_current_tool_contract() {
        let body = include_str!("../skills/cypher_query.md");
        assert!(!body.contains("aren't currently exposed"));
        assert!(body.contains("params="));
        assert!(body.contains("200 data rows"));
        assert!(body.contains("openCypher"));
    }

    /// **Delivery tiers are a shipped contract, not a preference.** Since
    /// mcp-methods 0.4.11 an absent `delivery:` key means `lazy`: the body
    /// stays out of `tools/list` and reaches the agent through `skill(name)`.
    /// `cypher_query` is the one exception — its body shapes the first call's
    /// `query` argument, which is what the eager tier is reserved for. Every
    /// other bundled skill here routes a call the agent can already make, so
    /// marking one eager silently puts its whole body back into every
    /// deployment's tool list; this is the test that goes red instead.
    #[test]
    fn only_the_cypher_skill_ships_on_the_eager_tier() {
        let eager = "delivery: eager";
        assert!(
            include_str!("../skills/cypher_query.md").contains(eager),
            "cypher_query must stay eager — its body shapes the first query"
        );
        for (name, body) in [
            (
                "graph_overview",
                include_str!("../skills/graph_overview.md"),
            ),
            ("save_graph", include_str!("../skills/save_graph.md")),
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
            (
                "recipe_queries",
                include_str!("../skills/recipe_queries.md"),
            ),
        ] {
            let frontmatter = body.split("\n---").next().unwrap_or_default();
            assert!(
                !frontmatter.contains(eager),
                "{name} declares `delivery: eager`: that returns its whole body to \
                 every tool description. Argue the first-call-parameter case here first."
            );
            let declared = frontmatter
                .lines()
                .find(|line| line.starts_with("delivery:"));
            assert!(
                declared.is_none_or(|line| line.trim_end() == "delivery: lazy"),
                "{name} declares an unrecognised delivery tier: {declared:?}"
            );
        }
    }

    /// The bundled examples are copied into live tool descriptions, where an
    /// invalid call shape sends agents to the wrong route without a compiler
    /// error. Keep the examples aligned with the registered zero-argument save
    /// and Cypher-reference overview forms.
    #[test]
    fn bundled_lifecycle_skill_examples_match_registered_call_shapes() {
        let overview = include_str!("../skills/graph_overview.md");
        assert!(overview.contains("graph_overview(cypher=['MATCH', 'WHERE'])"));
        assert!(overview.contains("cypher_query(query='MATCH"));
        assert!(!overview.contains("graph_overview(cypher='MATCH"));

        let save = include_str!("../skills/save_graph.md");
        assert!(save.contains("`save_graph()`"));
        assert!(save.contains("`save_graph_as`"));
        assert!(!save.contains("save_graph(to_path"));
    }
}

#[cfg(test)]
mod skill_layer_tests {
    use super::*;

    use std::path::Path;

    use kglite::api::skills::Delivery;
    use kglite::api::storage::StorageMode;
    use mcp_methods::server::{serve_prompts, McpServer, ServerOptions};
    use schemars::JsonSchema;
    use serde::Deserialize;

    #[derive(Default, Deserialize, JsonSchema)]
    struct EmptyArgs {}

    fn record(name: &str, description: &str, body: &str, tools: &[&str]) -> SkillRecord {
        SkillRecord {
            name: name.to_string(),
            description: description.to_string(),
            body: body.to_string(),
            references_tools: tools.iter().map(|t| (*t).to_string()).collect(),
            delivery: Delivery::Lazy,
        }
    }

    fn bundled_source() -> SkillsSource {
        SkillsSource::Sources(vec![SkillSource::Bundled])
    }

    /// The real builder in the same order [`install_skills`] uses: this
    /// crate's bundled `cypher_query`, then the graph layer, then the project
    /// layer auto-detected beside `manifest`.
    fn resolve(
        records: Vec<SkillRecord>,
        source: &SkillsSource,
        manifest: &Path,
    ) -> (ResolvedRegistry, GraphSkillStats) {
        let (layer, stats) = graph_skill_layer(records);
        let registry = SkillRegistry::new()
            .add_bundled(BundledSkill {
                name: "cypher_query",
                body: include_str!("../skills/cypher_query.md"),
            })
            .add_layer(layer, SkillProvenance::Owned(GRAPH_LAYER_LABEL.to_string()));
        let resolved = registry
            .auto_detect_project_layer(manifest)
            .layer_dirs(source, manifest)
            .expect("configure skill layers")
            .finalise()
            .expect("resolve registry");
        (resolved, stats)
    }

    /// A booted server with `tools` registered and `registry` served.
    struct Served {
        prompts: Vec<String>,
        descriptions: std::collections::HashMap<String, String>,
        active: Vec<ActiveSkill>,
        server: McpServer,
    }

    impl Served {
        fn description(&self, tool: &str) -> &str {
            self.descriptions
                .get(tool)
                .map(String::as_str)
                .unwrap_or_default()
        }

        /// Tool names the client would see in `tools/list` — `list_all` skips
        /// disabled routes, which is what an allowlist leaves behind.
        fn listed_tools(&mut self) -> Vec<String> {
            self.server
                .tool_router_mut()
                .list_all()
                .iter()
                .map(|tool| tool.name.to_string())
                .collect()
        }
    }

    fn served_with_tools(registry: &ResolvedRegistry, tools: &[&'static str]) -> Served {
        let mut server = McpServer::new(ServerOptions::default());
        for name in tools {
            server.register_typed_tool::<EmptyArgs, _>(name, "base", |_| "ok".to_string());
        }
        let active = serve_prompts(registry, &mut server);
        let prompts = server
            .prompt_router_mut()
            .list_all()
            .iter()
            .map(|prompt| prompt.name.to_string())
            .collect();
        let descriptions = server
            .tool_router_mut()
            .list_all()
            .iter()
            .map(|tool| {
                (
                    tool.name.to_string(),
                    tool.description.as_deref().unwrap_or_default().to_string(),
                )
            })
            .collect();
        Served {
            prompts,
            descriptions,
            active,
            server,
        }
    }

    #[test]
    fn a_graph_skill_reaches_prompts_and_the_tool_it_references() {
        // `delivery: eager` because this test asserts the *body* lands in the
        // target tool's description, which only the eager tier does since
        // mcp-methods 0.4.11 made lazy the default.
        let (registry, stats) = resolve(
            vec![SkillRecord {
                delivery: Delivery::Eager,
                ..record(
                    "wells",
                    "Well methodology.",
                    "# Wells\n\nMatch on `Well`.\n",
                    &["cypher_query"],
                )
            }],
            &bundled_source(),
            Path::new("graph_mcp.yaml"),
        );
        assert_eq!(stats.served, 1);
        assert_eq!(stats.body_bytes, "# Wells\n\nMatch on `Well`.\n".len());
        assert!(stats.skipped.is_empty());

        let served = served_with_tools(&registry, &["cypher_query"]);
        assert!(
            served.prompts.iter().any(|name| name == "wells"),
            "{:?}",
            served.prompts
        );
        let description = served.description("cypher_query");
        assert!(description.contains("mcp-skill:wells"), "{description}");
        assert!(description.contains("Match on `Well`."), "{description}");
        // No nudge is possible for an eager skill — the body is already there.
        assert!(!description.contains("skill(\"wells\")"), "{description}");
    }

    /// The default tier since mcp-methods 0.4.11: the routing text and a
    /// pointer at the loader land in the target tool's description, and the
    /// body does not. A `tools/list` that shipped every graph skill's body was
    /// the cost this change exists to remove, so the absent body is the
    /// assertion that matters.
    #[test]
    fn a_lazy_graph_skill_points_at_the_loader_instead_of_injecting_its_body() {
        let (registry, stats) = resolve(
            vec![record(
                "wells",
                "Well methodology.",
                "# Wells\n\nGRAPH-BODY-MARKER\n",
                &["cypher_query"],
            )],
            &bundled_source(),
            Path::new("graph_mcp.yaml"),
        );
        assert_eq!(stats.served, 1);

        let mut served = served_with_tools(&registry, &["cypher_query"]);
        let description = served.description("cypher_query").to_string();
        assert!(description.contains("mcp-skill:wells"), "{description}");
        assert!(description.contains("## When to use"), "{description}");
        assert!(description.contains("Well methodology."), "{description}");
        assert!(description.contains("skill(\"wells\")"), "{description}");
        assert!(
            !description.contains("GRAPH-BODY-MARKER"),
            "a lazy skill must not ship its body in a tool description: {description}"
        );
        // The pointer has somewhere to point.
        assert!(
            served
                .listed_tools()
                .iter()
                .any(|name| name == mcp_methods::server::SKILL_TOOL_NAME),
            "{:?}",
            served.listed_tools()
        );
        assert_eq!(
            served.active.iter().map(|s| s.delivery).collect::<Vec<_>>(),
            [
                mcp_methods::server::Delivery::Eager,
                mcp_methods::server::Delivery::Lazy
            ],
            "cypher_query ships eager; the graph skill defaults to lazy"
        );
    }

    /// The loader is the framework's, not ours, and it only exists when there
    /// are skills to load. A registry the opt-in suppressed must not grow one.
    #[test]
    fn the_skill_loader_appears_only_when_skills_resolve() {
        let (registry, _) = resolve(
            vec![record("wells", "Well methodology.", "Body.\n", &[])],
            &SkillsSource::Disabled,
            Path::new("graph_mcp.yaml"),
        );
        let mut served = served_with_tools(&registry, &["cypher_query"]);
        assert!(
            !served
                .listed_tools()
                .iter()
                .any(|name| name == mcp_methods::server::SKILL_TOOL_NAME),
            "{:?}",
            served.listed_tools()
        );
    }

    /// `install_skills` never registers a route called `skill`: a downstream
    /// tool of that name makes the framework abandon lazy delivery for the
    /// whole session and inject every body eagerly, with a warning nobody
    /// reads. This walks the real registration path rather than grepping.
    #[test]
    fn this_binary_registers_no_tool_named_skill() {
        let mut server = McpServer::new(ServerOptions::default());
        crate::tools::register(
            &mut server,
            GraphState::new(None),
            crate::tools::Builtins {
                writable: true,
                save_graph: true,
                ..Default::default()
            },
            crate::tools::OverviewDecorations::default(),
            Arc::new(crate::csv_http::CsvHttpState::Off),
            SkillRefresher::default(),
        );
        crate::tools::register_graph_mode_tools(
            &mut server,
            GraphState::new(None),
            SkillRefresher::default(),
        );
        assert!(
            !server
                .tool_router_mut()
                .has_route(mcp_methods::server::SKILL_TOOL_NAME),
            "a kglite route named `skill` would disable lazy delivery framework-wide"
        );
    }

    #[test]
    fn a_graph_skill_with_no_referenced_tools_still_parses() {
        // A skill that names no tool renders `references_tools:` with nothing
        // after it — a YAML *null*, not an empty sequence, and
        // `#[serde(default)]` covers only an absent key. It survives because
        // serde_yaml reads that null as an empty sequence. Nothing else
        // asserts it, and a frontmatter blob the registry refuses does not
        // cost one skill: it fails `finalise` and takes the session's whole
        // skill surface with it.
        let (registry, stats) = resolve(
            vec![record("standalone", "Standalone.", "Body.\n", &[])],
            &bundled_source(),
            Path::new("graph_mcp.yaml"),
        );
        assert_eq!(stats.served, 1, "{:?}", stats.skipped);
        assert!(registry.get("standalone").is_some());
    }

    #[test]
    fn graph_skills_do_not_bypass_the_manifest_skills_opt_in() {
        let (registry, stats) = resolve(
            vec![record(
                "wells",
                "Well methodology.",
                "Body.",
                &["cypher_query"],
            )],
            &SkillsSource::Disabled,
            Path::new("graph_mcp.yaml"),
        );
        // The layer was still built — the opt-in is enforced by the registry,
        // not by the reader — but nothing it produced is served.
        assert_eq!(stats.served, 1);
        assert!(registry.is_empty(), "{:?}", registry.skill_names());
        let served = served_with_tools(&registry, &["cypher_query"]);
        assert!(served.prompts.is_empty(), "{:?}", served.prompts);
    }

    /// **Precedence pin (D8).** The order
    /// `bundled < owned < inline < declared dirs < <basename>.skills/` is an
    /// mcp-methods 0.4.11 contract, documented on `Registry::add_layer`, and
    /// no longer an inference from how `finalise` happens to dedupe — the
    /// stopgap this pin was written against is gone with the `Box::leak` it
    /// rode on. It stays because a regression of that contract is invisible
    /// from here: the graph layer would silently sink below bundled, every
    /// graph override would stop working, and the build would stay green.
    /// This is what goes red instead.
    #[test]
    fn graph_layer_beats_bundled_and_loses_to_the_project_layer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = temp.path().join("graph_mcp.yaml");
        let override_record = || {
            record(
                "cypher_query",
                "Graph override.",
                "GRAPH BODY\n",
                &["cypher_query"],
            )
        };

        let graph_only = resolve(vec![override_record()], &bundled_source(), &manifest).0;
        let resolved = graph_only.get("cypher_query").expect("cypher_query");
        assert!(resolved.body.contains("GRAPH BODY"), "{}", resolved.body);
        assert!(
            !resolved.body.contains("200 data rows"),
            "the bundled cypher_query body must not survive a graph override"
        );

        let project = temp.path().join("graph_mcp.skills");
        std::fs::create_dir(&project).expect("project layer");
        std::fs::write(
            project.join("cypher_query.md"),
            "---\nname: cypher_query\ndescription: Operator override.\n\
             references_tools: [\"cypher_query\"]\n---\n\nFILE BODY\n",
        )
        .expect("write project skill");
        let with_file = resolve(vec![override_record()], &bundled_source(), &manifest).0;
        let resolved = with_file.get("cypher_query").expect("cypher_query");
        assert!(resolved.body.contains("FILE BODY"), "{}", resolved.body);
        assert!(!resolved.body.contains("GRAPH BODY"), "{}", resolved.body);
    }

    #[test]
    fn a_malformed_record_is_skipped_while_its_siblings_load() {
        let oversize = "x".repeat(kglite::api::skills::MAX_BODY_BYTES + 1);
        let (registry, stats) = resolve(
            vec![
                record("blank", "   ", "Body.", &[]),
                record("huge", "Too big.", &oversize, &[]),
                record("good", "Good methodology.", "Body.\n", &["cypher_query"]),
            ],
            &bundled_source(),
            Path::new("graph_mcp.yaml"),
        );
        assert_eq!(stats.served, 1);
        assert_eq!(stats.skipped.len(), 2, "{:?}", stats.skipped);
        assert!(
            stats.skipped[0].starts_with("blank: ") && stats.skipped[0].contains("description"),
            "{:?}",
            stats.skipped
        );
        assert!(
            stats.skipped[1].starts_with("huge: ") && stats.skipped[1].contains("body"),
            "{:?}",
            stats.skipped
        );
        assert!(registry.get("good").is_some());
        assert!(registry.get("blank").is_none());
        assert!(registry.get("huge").is_none());
        // The whole registry survives — including the bundled layer, which a
        // malformed blob reaching `finalise` would have taken with it.
        assert!(registry.get("cypher_query").is_some());
    }

    #[test]
    fn the_boot_summary_names_the_graph_layer_only_when_there_is_one() {
        assert_eq!(GraphSkillStats::default().summary(), None);
        let stats = GraphSkillStats {
            served: 2,
            body_bytes: 640,
            skipped: vec!["blank: bad".to_string()],
            active: None,
        };
        let summary = stats.summary().expect("summary");
        assert!(
            summary.starts_with("graph skills: 2 served (640 B)"),
            "{summary}"
        );
        assert!(summary.contains("1 skipped: blank: bad"), "{summary}");
        assert!(
            !summary.contains("active as"),
            "nothing resolved yet — the line must not claim an active count: {summary}"
        );
    }

    /// `served` counts what the layer *handed over*; `active` is what won
    /// resolution and is reachable. An operator file that overrides a graph
    /// skill takes the difference, and 0.4.11's `ActiveSkill::provenance` is
    /// the first thing that can tell them apart.
    #[test]
    fn the_boot_summary_attributes_the_graph_layer_from_the_active_set() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = temp.path().join("graph_mcp.yaml");
        let project = temp.path().join("graph_mcp.skills");
        std::fs::create_dir(&project).expect("project layer");
        std::fs::write(
            project.join("wells.md"),
            "---\nname: wells\ndescription: Operator override.\n\
             references_tools: [\"cypher_query\"]\n---\n\nFILE BODY\n",
        )
        .expect("write project skill");

        let (registry, mut stats) = resolve(
            vec![
                record("wells", "Well methodology.", "Body.\n", &["cypher_query"]),
                record("cores", "Core methodology.", "Body.\n", &["cypher_query"]),
            ],
            &bundled_source(),
            &manifest,
        );
        let served = served_with_tools(&registry, &["cypher_query"]);
        stats.attribute(&served.active);

        assert_eq!(stats.served, 2);
        assert_eq!(
            stats.active,
            Some(1),
            "the operator's `wells.md` outranks the graph's: {:?}",
            served.active
        );
        let summary = stats.summary().expect("summary");
        assert!(summary.contains("1 active as owned:graph"), "{summary}");
        assert!(
            served.active.iter().any(|skill| skill.name == "wells"
                && matches!(skill.provenance, SkillProvenance::Project)),
            "{:?}",
            served.active
        );
    }

    // ── Reading the layer out of a live graph ──────────────────────────────

    fn state_with_skill(dir: &Path, record: &SkillRecord) -> GraphState {
        let state = GraphState::new(None);
        state
            .create_in_mode(&dir.join("skills.kgl"), StorageMode::Memory)
            .expect("activate graph");
        state
            .with_active_mut(|active| {
                let graph = kglite::api::make_dir_graph_mut(active.kg.dir_mut());
                kglite::api::skills::set(graph, record).expect("write skill")
            })
            .expect("active graph");
        state
    }

    #[test]
    fn the_graph_layer_is_read_in_graph_watch_and_vault_modes_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = state_with_skill(
            temp.path(),
            &record("wells", "Well methodology.", "Body.\n", &["cypher_query"]),
        );

        for mode in [
            Mode::Graph {
                path: temp.path().join("skills.kgl"),
            },
            Mode::Watch {
                dir: temp.path().to_path_buf(),
            },
            // Vault mode carries `.kglite/skills/*.md` into the graph at
            // build time; missing it here is one of the three silent
            // failures the alias exists to prevent.
            Mode::Vault {
                dir: temp.path().to_path_buf(),
            },
        ] {
            let records = read_graph_skills(&mode, &state);
            assert_eq!(records.len(), 1, "{mode:?}");
            assert_eq!(records[0].name, "wells");
            assert_eq!(records[0].body, "Body.\n", "the body must come with it");
            assert_eq!(records[0].references_tools, ["cypher_query"]);
        }

        for mode in [
            Mode::Bare,
            Mode::SourceRoot {
                dir: temp.path().to_path_buf(),
            },
            Mode::LocalWorkspace {
                root: temp.path().to_path_buf(),
                watch: false,
            },
            Mode::Workspace {
                dir: temp.path().to_path_buf(),
            },
        ] {
            assert!(
                read_graph_skills(&mode, &state).is_empty(),
                "{mode:?} has no graph at boot and must contribute no layer"
            );
        }
    }

    /// A `KgliteSkill` node is invisible to every type enumeration, so a skill
    /// that gated on it would be gating on a shape no agent can discover — and
    /// the gate would be true exactly when the layer had already loaded it.
    #[test]
    fn a_system_label_never_satisfies_the_graph_has_node_type_predicate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = state_with_skill(
            temp.path(),
            &record("wells", "Well methodology.", "Body.\n", &[]),
        );
        let evaluator = KglitePredicateEvaluator {
            state: state.clone(),
        };

        let skill_label = [kglite::api::skills::SKILL_LABEL.to_string()];
        assert_eq!(
            evaluator.evaluate(&PredicateClause::GraphHasNodeType(&skill_label)),
            Some(false)
        );
        // The node really is there — the guard is a policy, not an absence.
        assert_eq!(
            state.with_kg(|kg| kglite::api::skills::list(kg.dir()).len()),
            Some(1)
        );

        state
            .with_active_mut(|active| {
                let graph = kglite::api::make_dir_graph_mut(active.kg.dir_mut());
                let params = std::collections::HashMap::new();
                kglite::api::session::execute_mut(
                    graph,
                    "CREATE (w:Well {id: 1, title: 'A'})",
                    &kglite::api::session::ExecuteOptions::eager(&params),
                )
                .expect("create well")
            })
            .expect("active graph");
        let well = ["Well".to_string()];
        assert_eq!(
            evaluator.evaluate(&PredicateClause::GraphHasNodeType(&well)),
            Some(true),
            "an ordinary label must still activate"
        );
    }

    // ── Rebuilding the layer after a graph swap ────────────────────────────

    /// `reload_graph` / `load_graph` / `create_graph` replace the data the
    /// graph layer was read from, so the skills injected at boot describe a
    /// graph the server no longer serves. Before mcp-methods 0.4.11 there was
    /// no way to fix that after `serve` and the staleness was documented; this
    /// pins the rebuild that replaced the documentation.
    #[test]
    fn a_rebuild_swaps_the_old_graphs_skills_for_the_new_ones() {
        let (before, _) = resolve(
            vec![record(
                "wells",
                "Well methodology.",
                "Body.\n",
                &["cypher_query"],
            )],
            &bundled_source(),
            Path::new("graph_mcp.yaml"),
        );
        let served = served_with_tools(&before, &["cypher_query"]);
        assert!(served
            .description("cypher_query")
            .contains("mcp-skill:wells"));
        let index = render_skills_index(&served.active).expect("an index");
        assert!(index.contains("wells [lazy]"), "{index}");

        let (after, _) = resolve(
            vec![record(
                "cores",
                "Core methodology.",
                "Body.\n",
                &["cypher_query"],
            )],
            &bundled_source(),
            Path::new("graph_mcp.yaml"),
        );
        let mut server = served.server;
        let active = server.reinject_skills(&after).expect("rebuild");

        let router = server.tool_router_mut();
        let description = router
            .get("cypher_query")
            .and_then(|tool| tool.description.as_deref())
            .expect("cypher_query description")
            .to_string();
        drop(router);
        assert!(
            !description.contains("mcp-skill:wells"),
            "the replaced graph's skill must be stripped: {description}"
        );
        assert!(description.contains("mcp-skill:cores"), "{description}");
        // And the index the bare overview serves moves with it — an index
        // still naming `wells` would point `skill("wells")` at a refusal.
        let index = render_skills_index(&active).expect("an index");
        assert!(index.contains("cores [lazy]"), "{index}");
        assert!(!index.contains("wells"), "{index}");
    }

    /// `extensions.tools_allow` closes the tool surface before
    /// `install_skills` runs, so the framework's `skill` loader is registered
    /// after the allowlist has been applied and is never matched against it.
    /// That exemption is correct and deliberate: the loader is a read-only
    /// fetch of methodology this deployment already chose to serve, and an
    /// allowlist that hid it would leave every lazy skill's
    /// `skill("<name>")` pointer aimed at a tool the agent cannot call. This
    /// pins the ordering — a future `install_skills` that ran *before* the
    /// allowlist would silently strip the loader.
    #[test]
    fn the_skill_loader_survives_a_tools_allow_that_does_not_name_it() {
        let (registry, _) = resolve(
            vec![record(
                "wells",
                "Well methodology.",
                "Body.\n",
                &["cypher_query"],
            )],
            &bundled_source(),
            Path::new("graph_mcp.yaml"),
        );
        let mut server = McpServer::new(ServerOptions::default());
        server.register_typed_tool::<EmptyArgs, _>("cypher_query", "base", |_| "ok".to_string());
        server.register_typed_tool::<EmptyArgs, _>("unwanted", "base", |_| "ok".to_string());
        crate::tools_allow::apply_tool_allowlist(
            &mut server,
            &["cypher_query".to_string()],
            /* recipes_configured */ false,
        )
        .expect("apply allowlist");
        serve_prompts(&registry, &mut server);

        let listed: Vec<String> = server
            .tool_router_mut()
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(
            listed.iter().any(|name| name == "cypher_query"),
            "{listed:?}"
        );
        assert!(
            !listed.iter().any(|name| name == "unwanted"),
            "the allowlist must still close the surface: {listed:?}"
        );
        assert!(
            listed
                .iter()
                .any(|name| name == mcp_methods::server::SKILL_TOOL_NAME),
            "the lazy-skill loader must survive the allowlist: {listed:?}"
        );
    }

    // ── The overview index ─────────────────────────────────────────────────

    #[test]
    fn the_index_lists_only_the_skills_the_session_actually_serves() {
        let (registry, _) = resolve(
            vec![
                record(
                    "wells",
                    "Well methodology. A second sentence that must not appear.",
                    "Body.\n",
                    &["cypher_query"],
                ),
                record(
                    "gated",
                    "Only on a code graph.",
                    "Body.\n",
                    &["cypher_query"],
                ),
            ],
            &bundled_source(),
            Path::new("graph_mcp.yaml"),
        );
        let served = served_with_tools(&registry, &["cypher_query"]);
        let index = render_skills_index(&served.active).expect("an index");

        assert!(index.starts_with("<skills count=\"3\""), "{index}");
        assert!(index.ends_with("</skills>"), "{index}");
        // The tier, not just the name: a `[lazy]` line is an instruction to
        // call `skill(name)`, and an agent that cannot tell the two apart
        // either re-fetches what it has or never fetches what it needs.
        assert!(
            index.contains("wells [lazy] — Well methodology."),
            "{index}"
        );
        assert!(index.contains("cypher_query [eager] — "), "{index}");
        assert!(index.contains("get-via=\"skill(name)\""), "{index}");
        assert!(
            !index.contains("second sentence"),
            "only the first sentence belongs in the index: {index}"
        );
        let names: Vec<&str> = index
            .lines()
            .filter(|line| line.contains(" — "))
            .map(|line| line.split(" [").next().unwrap_or_default())
            .collect();
        assert_eq!(names, ["cypher_query", "gated", "wells"]);
    }

    #[test]
    fn an_applies_when_gate_keeps_a_suppressed_skill_out_of_the_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = temp.path().join("graph_mcp.yaml");
        let project = temp.path().join("graph_mcp.skills");
        std::fs::create_dir(&project).expect("project layer");
        std::fs::write(
            project.join("needs_route.md"),
            "---\nname: needs_route\ndescription: Needs a route.\n\
             applies_when:\n  tool_registered: absent_tool\n---\n\nBody.\n",
        )
        .expect("write project skill");

        let (registry, _) = resolve(
            vec![record("wells", "Well methodology.", "Body.\n", &[])],
            &bundled_source(),
            &manifest,
        );
        assert!(
            registry.get("needs_route").is_some(),
            "the gated skill must be resolved — the index filters it, not the registry"
        );
        let served = served_with_tools(&registry, &["cypher_query"]);
        let index = render_skills_index(&served.active).expect("an index");
        assert!(index.contains("wells"), "{index}");
        assert!(
            !index.contains("needs_route"),
            "a skill suppressed from prompts/list must be absent from the index: {index}"
        );
        assert!(index.starts_with("<skills count=\"2\""), "{index}");
    }

    #[test]
    fn an_empty_registry_renders_no_index_at_all() {
        let (registry, _) = resolve(
            Vec::new(),
            &SkillsSource::Disabled,
            Path::new("graph_mcp.yaml"),
        );
        let served = served_with_tools(&registry, &[]);
        assert_eq!(render_skills_index(&served.active), None);
    }

    #[test]
    fn a_long_description_is_cut_at_a_char_boundary_with_an_ellipsis() {
        assert_eq!(skill_summary("One. Two."), "One.");
        assert_eq!(skill_summary("  Wrapped\ntext.  "), "Wrapped text.");
        assert_eq!(skill_summary("No sentence end"), "No sentence end");
        let long = "æ".repeat(200);
        let cut = skill_summary(&long);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(cut.len() <= 164, "{}", cut.len());
    }
    // ── The producer layer (`ServerExtensions::with_skills`) ───────────────

    /// A manifest loaded through the real loader, because
    /// [`manifest_declares_skills`] reads the file back and a struct literal
    /// would have no file to read.
    fn manifest_file(dir: &Path, body: &str) -> Manifest {
        let path = dir.join("producer_mcp.yaml");
        std::fs::write(&path, body).expect("write manifest");
        mcp_methods::server::load_manifest(&path).expect("manifest loads")
    }

    /// The production composition with a producer layer in it.
    fn compose_with_producer(
        manifest: Option<&Manifest>,
        producer: &[SkillRecord],
        mode: &Mode,
        state: &GraphState,
    ) -> (ResolvedRegistry, ProducerSkills) {
        let producer = ProducerSkills::build(producer).expect("valid producer records");
        let (result, _) = compose_registry(manifest, &producer.layer, mode, state, None);
        (result.expect("resolve registry"), producer)
    }

    fn producer_record() -> SkillRecord {
        SkillRecord {
            delivery: Delivery::Eager,
            ..record(
                "cypher_query",
                "How this builder's graphs are shaped.",
                "PRODUCER BODY\n",
                &["cypher_query"],
            )
        }
    }

    /// The whole point of the layer: it belongs to the *binary*, not to any one
    /// graph, so the two workspace modes — where no graph exists when the
    /// prompt plane freezes, and where the graph layer is therefore always
    /// empty — must carry it exactly as `--graph` does.
    #[test]
    fn the_producer_layer_is_served_in_every_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_file(temp.path(), "name: Producer\nskills: true\n");
        let state = GraphState::new(None);

        for mode in [
            Mode::Graph {
                path: temp.path().join("g.kgl"),
            },
            Mode::Watch {
                dir: temp.path().to_path_buf(),
            },
            Mode::LocalWorkspace {
                root: temp.path().to_path_buf(),
                watch: false,
            },
            Mode::Workspace {
                dir: temp.path().to_path_buf(),
            },
        ] {
            let (registry, producer) =
                compose_with_producer(Some(&manifest), &[producer_record()], &mode, &state);
            assert_eq!(producer.stats.served, 1, "{mode:?}");
            let served = served_with_tools(&registry, &["cypher_query"]);
            assert!(
                served.description("cypher_query").contains("PRODUCER BODY"),
                "{mode:?}: {}",
                served.description("cypher_query")
            );
        }
    }

    /// The exact codingest shape: `run_with_extensions` with a workspace-graph
    /// producer and no manifest anywhere. Before `with_skills` was its own
    /// opt-in this deployment served no skills at all, so the layer would have
    /// shipped inert for the binary that asked for it.
    #[test]
    fn a_manifest_less_boot_serves_the_bundled_set_and_the_producer_layer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = GraphState::new(None);
        let mode = Mode::LocalWorkspace {
            root: temp.path().to_path_buf(),
            watch: false,
        };

        let (registry, _) = compose_with_producer(
            None,
            &[record(
                "codingest_review",
                "How to review code in this graph.",
                "Start from `Function`.\n",
                &["cypher_query"],
            )],
            &mode,
            &state,
        );

        let served = served_with_tools(&registry, &["cypher_query", "graph_overview"]);
        assert!(
            served.prompts.iter().any(|p| p == "codingest_review"),
            "{:?}",
            served.prompts
        );
        // The bundled set comes with it — the synthesised source is the `true`
        // marker, which switches on every layer this binary holds.
        assert!(
            served.prompts.iter().any(|p| p == "cypher_query"),
            "{:?}",
            served.prompts
        );
    }

    /// The operator's last word. `skills: false` is a refusal, and a refusal
    /// silences the producer's layer with everything else — unlike a manifest
    /// that simply never mentioned skills.
    #[test]
    fn an_explicit_skills_false_silences_the_producer_layer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = GraphState::new(None);
        let mode = Mode::Bare;

        let refused = manifest_file(temp.path(), "name: Producer\nskills: false\n");
        let (registry, producer) =
            compose_with_producer(Some(&refused), &[producer_record()], &mode, &state);
        let mut stats = producer.stats.clone();
        let served = served_with_tools(&registry, &["cypher_query"]);
        stats.attribute(&served.active);
        assert!(served.prompts.is_empty(), "{:?}", served.prompts);
        assert_eq!(stats.served, 1, "the records were still handed over");
        assert_eq!(stats.active, Some(0), "and none of them surfaced");
        assert!(
            stats
                .summary()
                .is_some_and(|line| line.contains("0 active as owned:producer")),
            "{:?}",
            stats.summary()
        );

        // Control: the same manifest without the `skills:` line. Silence is not
        // a refusal, so here the producer turns skills on.
        let silent = manifest_file(temp.path(), "name: Producer\n");
        let (registry, _) =
            compose_with_producer(Some(&silent), &[producer_record()], &mode, &state);
        let served = served_with_tools(&registry, &["cypher_query"]);
        assert!(
            served.prompts.iter().any(|p| p == "cypher_query"),
            "{:?}",
            served.prompts
        );
    }

    /// Without a producer layer an undeclared `skills:` keeps meaning "off" —
    /// the opt-in belongs to `with_skills`, and every manifest that never
    /// mentioned skills must keep serving none.
    #[test]
    fn an_undeclared_skills_key_stays_off_without_a_producer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_file(temp.path(), "name: Quiet\n");
        let state = GraphState::new(None);

        let (registry, _) = compose_with_producer(Some(&manifest), &[], &Mode::Bare, &state);

        let served = served_with_tools(&registry, &["cypher_query"]);
        assert!(served.prompts.is_empty(), "{:?}", served.prompts);
    }

    /// **Precedence pin (D2).** `bundled < producer < graph < project layer`,
    /// asserted on one name so each step is the *observable* difference between
    /// two bodies. A regression here is invisible otherwise: the layers would
    /// silently reorder and every override would resolve to the wrong body with
    /// the build still green.
    #[test]
    fn the_producer_layer_sits_between_bundled_and_the_graph() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_file(temp.path(), "name: Layers\nskills: true\n");
        let mode = Mode::Graph {
            path: temp.path().join("skills.kgl"),
        };

        // 1. producer beats bundled.
        let empty = GraphState::new(None);
        let (registry, _) =
            compose_with_producer(Some(&manifest), &[producer_record()], &mode, &empty);
        let resolved = registry.get("cypher_query").expect("cypher_query");
        assert!(resolved.body.contains("PRODUCER BODY"), "{}", resolved.body);
        assert!(
            !resolved.body.contains("200 data rows"),
            "the bundled cypher_query body must not survive a producer override"
        );

        // 2. the graph beats the producer.
        let state = state_with_skill(
            temp.path(),
            &SkillRecord {
                delivery: Delivery::Eager,
                ..record(
                    "cypher_query",
                    "This graph's own correction.",
                    "GRAPH BODY\n",
                    &["cypher_query"],
                )
            },
        );
        let (registry, _) =
            compose_with_producer(Some(&manifest), &[producer_record()], &mode, &state);
        let resolved = registry.get("cypher_query").expect("cypher_query");
        assert!(resolved.body.contains("GRAPH BODY"), "{}", resolved.body);
        assert!(
            !resolved.body.contains("PRODUCER BODY"),
            "{}",
            resolved.body
        );

        // 3. the operator's file beats both.
        let project = temp.path().join("producer_mcp.skills");
        std::fs::create_dir(&project).expect("project layer");
        std::fs::write(
            project.join("cypher_query.md"),
            "---\nname: cypher_query\ndescription: The operator's own.\ndelivery: eager\n---\n\nFILE BODY\n",
        )
        .expect("write project skill");
        let (registry, _) =
            compose_with_producer(Some(&manifest), &[producer_record()], &mode, &state);
        let resolved = registry.get("cypher_query").expect("cypher_query");
        assert!(resolved.body.contains("FILE BODY"), "{}", resolved.body);
    }

    /// A producer record is the embedder's own code, not graph data, so the
    /// honest report for a bad one is a refusal that names it — not the
    /// skip-and-warn a hand-written `CREATE` gets.
    #[test]
    fn an_invalid_producer_record_fails_the_boot_and_names_it() {
        let error = ProducerSkills::build(&[record(
            "has spaces",
            "A name the registry cannot key on.",
            "Body.\n",
            &[],
        )])
        .expect_err("an invalid producer record must fail the boot");
        let message = error.to_string();
        assert!(message.contains("has spaces"), "{message}");
        assert!(message.contains("with_skills"), "{message}");

        // Control: the sibling shape loads, so the refusal is about the record
        // and not about producer layers in general.
        ProducerSkills::build(&[record("fine", "A fine skill.", "Body.\n", &[])])
            .expect("a valid record builds");
    }

    /// Provenance is what makes the layer's contribution reportable at all —
    /// the boot line, and the index an agent reads out of `graph_overview`.
    #[test]
    fn the_producer_layer_is_attributed_and_appears_in_the_overview_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_file(temp.path(), "name: Producer\nskills: true\n");
        let state = GraphState::new(None);

        let (registry, producer) = compose_with_producer(
            Some(&manifest),
            &[record(
                "builder_methodology",
                "How this builder's graphs are shaped.",
                "Body.\n",
                &["cypher_query"],
            )],
            &Mode::Bare,
            &state,
        );
        let served = served_with_tools(&registry, &["cypher_query"]);
        let mut stats = producer.stats.clone();
        stats.attribute(&served.active);

        assert!(
            served
                .active
                .iter()
                .any(|skill| skill.name == "builder_methodology"
                    && matches!(&skill.provenance, SkillProvenance::Owned(label)
                    if label == PRODUCER_LAYER_LABEL)),
            "{:?}",
            served.active
        );
        assert_eq!(stats.active, Some(1));
        let summary = stats.summary().expect("a contributed layer reports itself");
        assert!(summary.contains("producer skills: 1 served"), "{summary}");
        assert!(summary.contains("1 active as owned:producer"), "{summary}");

        let index = render_skills_index(&served.active).expect("a non-empty index");
        assert!(index.contains("builder_methodology"), "{index}");

        // Control: no producer records, no line.
        assert_eq!(ProducerSkillStats::default().summary(), None);
    }
}
