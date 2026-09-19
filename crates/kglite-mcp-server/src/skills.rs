//! Skill and discovery-steer wiring: the lazy-discovery instruction fold,
//! the conditionally bundled recipe-query skill, and the graph-aware
//! predicate evaluator that gates skills on the active graph's shape.

use std::sync::{Arc, RwLock};

use anyhow::{bail, Result};
use kglite::api::skills::{self as graph_skills, SkillRecord};
use mcp_methods::server::skills::SESSION_TOTAL_LIMIT_BYTES;
use mcp_methods::server::{
    notify_skills_changed, serve_prompts, ActiveSkill, BundledSkill, Manifest, McpServer,
    OwnedSkill, PredicateClause, ResolvedRegistry, ServerOptions, SkillError,
    SkillPredicateEvaluator, SkillProvenance, SkillRegistry, SkillReloader, SkillSource,
    SkillsSource,
};

use crate::tools::{read_lock, write_lock, GraphState, PeerSlot, SkillsIndexSlot};
use crate::*;

/// The label this binary gives the served graph's owned skill layer, rendered
/// by mcp-methods as `owned:graph` wherever provenance is shown.
pub(crate) const GRAPH_LAYER_LABEL: &str = "graph";

/// The label for the embedding binary's own layer
/// ([`ServerExtensions::with_skills`]), rendered as `owned:producer`.
///
/// Two owned layers, ordered by how close they are to the operator: the
/// producer describes the shapes its builder always emits, the graph describes
/// itself, and a graph wins a name collision because it is the more specific
/// statement. Both lose to the operator's own files.
pub(crate) const PRODUCER_LAYER_LABEL: &str = "producer";

/// Client-side tool-discovery steer, folded into workspace-mode
/// `instructions` so every `--workspace` / `workspace.kind: local`
/// deployment emits it on `initialize` without copy-pasting it into each
/// manifest. It complements the 0.12.6 in-band steering (graph-over-grep
/// vocabulary in tool descriptions, the activation mini-map, the result
/// footer) by making the *"search the registry"* instruction explicit for
/// lazy-tool-discovery clients (Codex / code_mode / tool-search), which can
/// surface only `grep`/`read_source` on a broad first query and miss the
/// always-registered graph tools. Skipped when the manifest already carries
/// equivalent guidance (see the dedup check in `run_async`).
pub(crate) const DISCOVERY_STEER: &str = "Tool discovery: graph_overview and cypher_query are ALWAYS registered. \
If a broad first tool-search surfaces only grep/read_source, search your tool registry for 'cypher' or \
'graph_overview' and load those before falling back to grep — a discovery miss does not mean the graph \
path is unavailable.";

pub(crate) const RECIPE_QUERIES_SKILL: &str = include_str!("../skills/recipe_queries.md");

/// Compose the skill registry for this deployment.
///
/// Bundled methodology for KGLite's custom tools, the optional recipe catalog,
/// framework defaults, the embedder's own producer layer and the graph's own
/// `KgliteSkill` records are composed with the operator-side project layer and
/// any operator-declared domain skill packs. The predicate evaluator gates
/// `read_code_source` on `graph_has_node_type: [Function, Class]` so it stays
/// out of prompts/list when the active graph isn't a code-tree (legal-corpus /
/// o&g / etc. deployments).
///
/// `manifest` is `None` for a deployment that has no manifest at all — legal
/// since the producer layer became its own opt-in (see [`skills_source`]);
/// without one there is no declared root layer and no project directory to
/// auto-detect, so the composition is the binary's own layers alone.
///
/// Separate from [`install_skills`] because the composition is re-run against
/// whatever graph is active *now* whenever the served graph is swapped — see
/// [`SkillRefresher`]. Boot and reload therefore build the registry the same
/// way rather than drifting apart. The producer layer arrives pre-rendered
/// because it is validated once, at boot, where a bad record can still fail
/// the boot; a refresh must never be able to fail on it.
fn compose_registry(
    manifest: Option<&Manifest>,
    producer_layer: &[OwnedSkill],
    mode: &Mode,
    graph_state: &GraphState,
    recipe_catalog_summary: Option<crate::recipe_queries::CatalogSummary>,
) -> (Result<ResolvedRegistry, SkillError>, GraphSkillStats) {
    // Skill `.md` bodies live at `crates/kglite-mcp-server/skills/` — the
    // single canonical home. `cargo publish` only packages files inside
    // the crate dir, so they must live here (not behind a
    // `../../../kglite/...` `include_str!` path).
    //
    // A gated skill is bundled only where its gate can be true. mcp-methods
    // charges its session budget at *resolve* time — `Registry::finalise`
    // sums every resolved body, whatever `applies_when:` later says — so a
    // skill that cannot activate here still costs its bytes, and the bundled
    // set overran the 64 KiB limit on a plain `--graph` deployment. Where the
    // gate is a fact this composition already knows (the mode, the active
    // graph's node types), the same answer is given by not bundling it, and
    // the *active* set is unchanged either way: what moves is the budget. The
    // composition re-runs on every graph swap ([`SkillRefresher`]), so a gate
    // that becomes true later still lands its skill.
    let mut bundled = vec![
        BundledSkill {
            name: "cypher_query",
            body: include_str!("../skills/cypher_query.md"),
        },
        BundledSkill {
            name: "graph_overview",
            body: include_str!("../skills/graph_overview.md"),
        },
        BundledSkill {
            name: "save_graph",
            body: include_str!("../skills/save_graph.md"),
        },
        // Gated on `tool_registered: fetch_images`, which every mode
        // registers but only a mode with a source root leaves enabled — a
        // disabled route is absent from the router's visible set, so the
        // predicate is false exactly where the bytes are unreachable. Not
        // decidable here: route disabling happens after this composition.
        BundledSkill {
            name: "fetch_images",
            body: include_str!("../skills/fetch_images.md"),
        },
    ];
    // Gated on `tool_registered: rebuild_graph`, which `server_run` registers
    // for `Mode::Vault` and nothing else — the format rules are noise on a
    // server that does not build its graph from markdown the agent can edit.
    if matches!(mode, Mode::Vault { .. }) {
        bundled.push(BundledSkill {
            name: "vault_authoring",
            body: include_str!("../skills/vault_authoring.md"),
        });
    }
    // The code-graph four, gated on `graph_has_node_type: [Function, Class]`.
    // `explore` and `read_code_source` name a tool; the other two are
    // cross-tool skills that attach via `references_tools` and lead with the
    // `description` routing (the serve_prompts injection of mcp-methods
    // 0.3.42). A graph with no code in it activates none of them.
    if serves_a_code_graph(graph_state) {
        bundled.extend([
            BundledSkill {
                name: "read_code_source",
                body: include_str!("../skills/read_code_source.md"),
            },
            BundledSkill {
                name: "explore",
                body: include_str!("../skills/explore.md"),
            },
            BundledSkill {
                name: "code_graph_analysis",
                body: include_str!("../skills/code_graph_analysis.md"),
            },
            BundledSkill {
                name: "code_graph_views",
                body: include_str!("../skills/code_graph_views.md"),
            },
        ]);
    }
    let registry = bundled
        .into_iter()
        .fold(SkillRegistry::new(), SkillRegistry::add_bundled);
    let registry =
        add_recipe_query_skill(registry, recipe_catalog_summary).merge_framework_defaults();

    // Two *owned* layers (mcp-methods 0.4.11), which the framework slots
    // between the compile-time bundled skills — this crate's and its own — and
    // the operator's file layers. Later `add_layer` calls override earlier
    // ones, so the producer goes in first and the graph second: a graph skill
    // beats a producer skill beats a bundled one of the same name, while a
    // declared pack or `<basename>.skills/` still beats both. Pinned by
    // `graph_layer_beats_bundled_and_loses_to_the_project_layer` and
    // `the_producer_layer_sits_between_bundled_and_the_graph`.
    let registry = registry.add_layer(
        producer_layer.to_vec(),
        SkillProvenance::Owned(PRODUCER_LAYER_LABEL.to_string()),
    );
    let (graph_layer, graph_stats) = graph_skill_layer(read_graph_skills(mode, graph_state));
    let registry = registry.add_layer(
        graph_layer,
        SkillProvenance::Owned(GRAPH_LAYER_LABEL.to_string()),
    );

    let (source, yaml_path) = skills_source(manifest, !producer_layer.is_empty());
    let registry = match manifest {
        // `<basename>.skills/` is an operator directory sitting next to their
        // YAML, so it is detected whenever there is a YAML to sit next to —
        // including a manifest that never declared `skills:` and had the
        // producer turn them on. Dropping it there would let a producer skill
        // shadow a file the operator wrote, which is the one thing every layer
        // rule in this program exists to prevent.
        Some(manifest) => registry.auto_detect_project_layer(&manifest.yaml_path),
        None => registry,
    };
    let registry_result = registry.layer_dirs(&source, yaml_path).and_then(|r| {
        r.with_predicate_evaluator(KglitePredicateEvaluator {
            state: graph_state.clone(),
        })
        .finalise()
    });
    (registry_result, graph_stats)
}

/// The node types the four code-graph skills gate on (`applies_when:
/// graph_has_node_type: [Function, Class]`), asked of the graph this
/// composition is for.
///
/// False with no graph published yet — a workspace mode before its first
/// activation — which is the same answer the predicate gives there, and the
/// refresh after the activation asks again.
fn serves_a_code_graph(graph_state: &GraphState) -> bool {
    ["Function", "Class"]
        .iter()
        .any(|node_type| graph_state.has_node_type(node_type))
}

/// The `skills:` declaration this composition obeys, and the path its relative
/// entries resolve against.
///
/// Without a producer layer this is simply the manifest's own declaration — an
/// operator who never wrote `skills:` gets no skills, exactly as before.
///
/// `has_producer` is what changes it, and only in the undeclared case.
/// `Manifest.skills` is `SkillsSource::Disabled` for both "the operator wrote
/// `skills: false`" and "the operator never mentioned skills", and
/// [`ServerExtensions::with_skills`] has to tell those apart: a refusal is the
/// operator's last word and silences everything, while silence is not a
/// refusal. So a producer deployment whose manifest never mentions `skills:` —
/// and a manifest-less one, which has no `skills:` to mention — is composed as
/// if it read `skills: true`: the binary's own layers, and nothing the operator
/// did not write.
///
/// The path is only ever used to resolve a declared *directory*, and the
/// synthesised source declares none; `Path::new(".")` stands in for the
/// manifest that is not there.
fn skills_source(
    manifest: Option<&Manifest>,
    has_producer: bool,
) -> (SkillsSource, &std::path::Path) {
    match manifest {
        Some(manifest) if !has_producer || manifest_declares_skills(manifest) => {
            (manifest.skills.clone(), manifest.yaml_path.as_path())
        }
        Some(manifest) => (bundled_only(), manifest.yaml_path.as_path()),
        None => (bundled_only(), std::path::Path::new(".")),
    }
}

/// The `skills: true` source: switch on the layers this binary already holds,
/// declare no directory of the operator's own.
fn bundled_only() -> SkillsSource {
    SkillsSource::Sources(vec![SkillSource::Bundled])
}

/// Does the manifest YAML carry a top-level `skills:` key at all?
///
/// mcp-methods parses both "absent" and "`skills: false`" into
/// `SkillsSource::Disabled`, and exposes no "was it declared?" flag, so the
/// only remaining primitive is the file. Read at boot, once.
///
/// A top-level block-mapping key is the sole YAML construct that can begin a
/// line at column 0 with `skills:` — nested keys are indented, block-scalar
/// content is indented, and a comment starts with `#` — so a mention inside
/// `instructions:` cannot be mistaken for a declaration. The construct this
/// does miss is a whole-document *flow* mapping (`{name: x, skills: false}`),
/// which would be read as undeclared and let the producer layer surface
/// against the operator's wish; no manifest in this project, its tests or
/// mcp-methods' own examples is written that way, and an unreadable file is
/// likewise treated as undeclared because the manifest that failed to load is
/// not the one that will be served.
fn manifest_declares_skills(manifest: &Manifest) -> bool {
    let Ok(text) = std::fs::read_to_string(&manifest.yaml_path) else {
        return false;
    };
    text.lines().any(|line| {
        line.strip_prefix("skills")
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    })
}

/// Compose, serve and index the skill registry at boot.
///
/// A registry that fails to build disables skills for the session rather than
/// failing boot — except for the one failure an operator can fix by reading
/// the message; see [`report_registry_failure`].
///
/// Fills `skills_index` with the bare-`graph_overview` index of everything
/// this session actually serves, and returns what the two owned layers
/// contributed so the boot summary can name them.
pub(crate) fn install_skills(
    server: &mut McpServer,
    manifest: Option<&Manifest>,
    producer: &ProducerSkills,
    mode: &Mode,
    graph_state: &GraphState,
    recipe_catalog_summary: Option<crate::recipe_queries::CatalogSummary>,
    skills_index: &SkillsIndexSlot,
) -> Result<SkillLayerStats> {
    let (registry_result, graph) = compose_registry(
        manifest,
        &producer.layer,
        mode,
        graph_state,
        recipe_catalog_summary,
    );
    let mut stats = SkillLayerStats {
        graph,
        producer: producer.stats.clone(),
    };
    match registry_result {
        Ok(registry) => {
            log_parse_warnings(&registry);
            warn_if_over_session_budget(&registry);
            let active = serve_prompts(&registry, server);
            stats.graph.attribute(&active);
            stats.producer.attribute(&active);
            *write_lock(skills_index) = render_skills_index(&active);
            Ok(stats)
        }
        Err(e) => {
            report_registry_failure(e, manifest.map(|m| m.yaml_path.as_path())).map(|()| stats)
        }
    }
}

/// Everything the skill plane is assembled from at boot.
///
/// A struct rather than a parameter list for the same reason `KgliteToolParams`
/// is one: the set is the boot wiring and grows with it.
pub(crate) struct SkillBootParams<'a> {
    /// The embedder's records, straight off [`ServerExtensions::with_skills`]
    /// and not yet validated — validating them here is what lets a bad one
    /// fail the boot.
    pub(crate) producer_records: Vec<SkillRecord>,
    pub(crate) manifest: Option<&'a Manifest>,
    pub(crate) mode: &'a Mode,
    pub(crate) graph_state: &'a GraphState,
    pub(crate) recipe_catalog_summary: Option<crate::recipe_queries::CatalogSummary>,
    pub(crate) skills_index: &'a SkillsIndexSlot,
    pub(crate) refresher: &'a SkillRefresher,
    pub(crate) peer: &'a PeerSlot,
}

/// Build, serve and arm the whole skill plane.
///
/// **Two ways in.** A manifest is one — the `skills:` declaration lives there,
/// and an operator who wrote `skills: false` gets exactly that. The other is
/// the embedder: [`ServerExtensions::with_skills`] is its own opt-in, so a
/// manifest-less producer deployment serves the bundled set plus its own layer
/// rather than nothing at all. Neither present: no skills, which is what a
/// plain bare server has always served.
pub(crate) fn boot_skills(
    server: &mut McpServer,
    params: SkillBootParams<'_>,
) -> Result<SkillLayerStats> {
    let SkillBootParams {
        producer_records,
        manifest,
        mode,
        graph_state,
        recipe_catalog_summary,
        skills_index,
        refresher,
        peer,
    } = params;
    // Before anything the embedder cannot fix later: a malformed record is a
    // bug in the binary, and a boot that refuses naming it is a better report
    // than a server serving a silently incomplete methodology.
    let producer = ProducerSkills::build(&producer_records)?;
    // A manifest, an embedder's own records — or a vault. The first two are
    // the long-standing rule ("an operator who never wrote `skills:` gets no
    // skills"), and a vault is the case that rule was never written for: the
    // common `--vault DIR` invocation carries no manifest at all, and the
    // skills it would serve are not the operator's files but content *inside
    // the served directory* — the same standing as an embedder's producer
    // layer, which is why it switches the plane on the same way.
    //
    // Deliberately only `Vault`. A manifest-less `--graph` server whose `.kgl`
    // carries `KgliteSkill` records is the same shape and stays off, because
    // turning it on would change what existing deployments serve.
    let enabled = manifest.is_some() || !producer.is_empty() || matches!(mode, Mode::Vault { .. });
    if !enabled {
        return Ok(SkillLayerStats::default());
    }
    let stats = install_skills(
        server,
        manifest,
        &producer,
        mode,
        graph_state,
        recipe_catalog_summary,
        skills_index,
    )?;
    if stats.producer.served > 0 && stats.producer.active == Some(0) {
        // The one outcome an embedder cannot see from the outside: its records
        // validated, were handed over, and nothing surfaced. Named here because
        // the boot line reports the count without the cause.
        tracing::info!(
            skills = stats.producer.served,
            "producer skills served none — the manifest's `skills:` declaration excludes the \
             binary's own layers (`skills: false`, or a list without `true`), or a graph or \
             operator layer took every name"
        );
    }
    // After `install_skills`, because the reloader's captured `ServerOptions`
    // snapshot must be the final one and the composition it re-runs is the one
    // that just ran.
    refresher.arm(RefreshInputs {
        reloader: server.skill_reloader(),
        manifest,
        producer: &producer,
        mode,
        graph_state,
        recipe_catalog_summary,
        skills_index,
        peer,
    });
    Ok(stats)
}

/// What this boot's two owned layers contributed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SkillLayerStats {
    pub(crate) graph: GraphSkillStats,
    pub(crate) producer: ProducerSkillStats,
}

/// The embedder's skill layer: validated, rendered and counted once, at boot.
///
/// Held as rendered [`OwnedSkill`] bodies rather than as `SkillRecord`s
/// because every later consumer — the boot composition and every
/// [`SkillRefresher`] rebuild — wants the same bytes, and because validation
/// belongs where it can still fail the boot. A producer record is the
/// embedder's code, not graph data: a fault in it is a bug in the binary, and
/// the honest report is a refusal that names the record rather than a warning
/// nobody reads.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProducerSkills {
    layer: Vec<OwnedSkill>,
    stats: ProducerSkillStats,
}

impl ProducerSkills {
    /// Validate and render the records [`ServerExtensions::with_skills`]
    /// collected. An invalid record fails the boot, naming itself.
    pub(crate) fn build(records: &[SkillRecord]) -> Result<Self> {
        let mut layer = Vec::with_capacity(records.len());
        let mut stats = ProducerSkillStats::default();
        for record in records {
            if let Err(error) = graph_skills::validate(record) {
                bail!(
                    "producer skill {:?} (ServerExtensions::with_skills) is invalid: {error}",
                    record.name
                );
            }
            let rendered = graph_skills::render_markdown(record);
            // Second gate, as the graph layer does: the registry parses this
            // blob's frontmatter, and proving it reads here names the record
            // instead of surfacing as an anonymous `ParseWarning` later.
            if let Err(error) = mcp_methods::server::skills::parse_skill(
                &rendered,
                std::path::Path::new("<producer>"),
            ) {
                bail!(
                    "producer skill {:?} (ServerExtensions::with_skills) is invalid: {error}",
                    record.name
                );
            }
            stats.served += 1;
            stats.body_bytes += record.body.len();
            layer.push(OwnedSkill {
                name: record.name.clone(),
                body: rendered,
            });
        }
        Ok(Self { layer, stats })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.layer.is_empty()
    }

    fn layer(&self) -> Vec<OwnedSkill> {
        self.layer.clone()
    }
}

/// What the embedder's own skill layer contributed to this boot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProducerSkillStats {
    /// Records handed to the registry. No `skipped` twin: an invalid producer
    /// record never reaches this struct, it fails the boot.
    pub(crate) served: usize,
    /// Sum of their body bytes — the same bound the graph layer's line reports,
    /// for the same reason: this is text injected into every `tools/list`.
    pub(crate) body_bytes: usize,
    /// How many reached the **active** set under `owned:producer`. `Some(0)`
    /// with `served > 0` is the operator having declared `skills: false`, or a
    /// closer layer having taken every name.
    pub(crate) active: Option<usize>,
}

impl ProducerSkillStats {
    fn attribute(&mut self, active: &[ActiveSkill]) {
        self.active = Some(
            active
                .iter()
                .filter(|skill| {
                    matches!(&skill.provenance,
                        SkillProvenance::Owned(label) if label == PRODUCER_LAYER_LABEL)
                })
                .count(),
        );
    }

    /// Boot-summary fragment, or `None` when the embedder contributed nothing.
    pub(crate) fn summary(&self) -> Option<String> {
        if self.served == 0 {
            return None;
        }
        let mut text = format!(
            "producer skills: {} served ({} B)",
            self.served, self.body_bytes
        );
        if let Some(active) = self.active {
            text.push_str(&format!(
                ", {active} active as owned:{PRODUCER_LAYER_LABEL}"
            ));
        }
        Some(text)
    }
}

/// Say what an over-budget skill set *does*, not just that it is over.
///
/// mcp-methods logs the numbers at `Registry::finalise`
/// (`server/skills.rs:1569`) and stops there, which leaves the operator unable
/// to tell whether skills were dropped, truncated or served — the three have
/// very different answers. They are served: `SESSION_TOTAL_LIMIT_BYTES` is an
/// advisory sum over the resolved set (`ResolvedRegistry::total_body_bytes`),
/// nothing is removed and nothing is cut short. What the overrun costs is
/// context, on every `tools/list`, for every client.
fn warn_if_over_session_budget(registry: &ResolvedRegistry) {
    let total = registry.total_body_bytes();
    if total <= SESSION_TOTAL_LIMIT_BYTES {
        return;
    }
    tracing::warn!(
        total_bytes = total,
        limit = SESSION_TOTAL_LIMIT_BYTES,
        over_by = total - SESSION_TOTAL_LIMIT_BYTES,
        "skill bodies exceed the session budget: nothing is dropped and nothing is \
         truncated — every skill is still served, and the overrun is context every \
         client pays for on every tools/list. Trim a skill, or narrow an \
         `applies_when:` gate, to get under it"
    );
}

/// Surface the registry's own per-entry complaints.
///
/// Owned-layer entries that the framework refuses — a body over its 16 KiB
/// hard limit, frontmatter whose `name` disagrees with the record's — are
/// `ParseWarning`s rather than errors, so nothing else in the session says the
/// skill is missing. [`graph_skill_layer`] catches the same classes first and
/// reports them on the boot line; this is the second net, and it also covers
/// the operator's file layers.
fn log_parse_warnings(registry: &ResolvedRegistry) {
    for warning in registry.parse_warnings() {
        tracing::warn!(
            path = %warning.path.display(),
            error = %warning.error,
            "skill entry skipped"
        );
    }
}

/// What the graph's own skill records contributed to this boot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GraphSkillStats {
    /// Records that validated and were handed to the registry.
    pub(crate) served: usize,
    /// Sum of their body bytes — the text that lands in `tools/list` for every
    /// client, every boot, and the only bound on it this side of the registry
    /// (mcp-methods enforces its per-skill ceiling on *file* loads only).
    pub(crate) body_bytes: usize,
    /// One `name: reason` per record that was skipped, in graph order.
    pub(crate) skipped: Vec<String>,
    /// How many of the served records reached the **active** set under the
    /// `owned:graph` provenance — i.e. won their name against every other
    /// layer and passed `applies_when:`. `None` until the registry has been
    /// resolved and served; `Some(n)` with `n < served` means an operator file
    /// or an `applies_when:` gate took the difference.
    pub(crate) active: Option<usize>,
}

impl GraphSkillStats {
    /// Read the post-activation truth back out of the resolved set.
    ///
    /// [`graph_skill_layer`] only knows what was *handed* to the registry;
    /// which of those the agent can actually reach is settled by resolution,
    /// and mcp-methods 0.4.11's `ActiveSkill::provenance` is what makes the
    /// graph's contribution distinguishable from the bundled and file layers.
    fn attribute(&mut self, active: &[ActiveSkill]) {
        self.active = Some(
            active
                .iter()
                .filter(|skill| is_graph_provenance(&skill.provenance))
                .count(),
        );
    }

    /// Boot-summary fragment, or `None` when the graph carried nothing —
    /// silence is the right report for the overwhelmingly common case.
    pub(crate) fn summary(&self) -> Option<String> {
        if self.served == 0 && self.skipped.is_empty() {
            return None;
        }
        let mut text = format!(
            "graph skills: {} served ({} B)",
            self.served, self.body_bytes
        );
        if let Some(active) = self.active {
            text.push_str(&format!(", {active} active as owned:{GRAPH_LAYER_LABEL}"));
        }
        if !self.skipped.is_empty() {
            text.push_str(&format!(
                ", {} skipped: {}",
                self.skipped.len(),
                self.skipped.join("; ")
            ));
        }
        Some(text)
    }
}

/// Whether a resolved skill came from this binary's graph layer.
fn is_graph_provenance(provenance: &SkillProvenance) -> bool {
    matches!(provenance, SkillProvenance::Owned(label) if label == GRAPH_LAYER_LABEL)
}

/// Read the active graph's `KgliteSkill` records, bodies included.
///
/// **Graph, watch and vault modes only.** Those are the modes whose graph is
/// already open when skills are installed (`bind_mode` opens it at boot); the
/// workspace modes build their graph on first activation, long after the
/// prompt plane is frozen, and the source-root and bare modes have no graph at
/// all. Returning nothing there is the honest answer rather than a layer that
/// works in a third of the deployments.
pub(crate) fn read_graph_skills(mode: &Mode, graph_state: &GraphState) -> Vec<SkillRecord> {
    if !matches!(
        mode,
        Mode::Graph { .. } | Mode::Watch { .. } | Mode::Vault { .. }
    ) {
        return Vec::new();
    }
    graph_state
        .with_kg(|kg| {
            let dir = kg.dir();
            graph_skills::list(dir)
                .into_iter()
                .filter_map(|summary| graph_skills::get(dir, &summary.name).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Turn skill records into the [`OwnedSkill`] entries `add_layer` takes.
///
/// Validation is **per record**: anything a hand-written `CREATE` could put in
/// the graph that the registry would choke on is dropped here with a warning
/// naming the skill and the rule, and its siblings still load. The framework
/// would demote the same faults to `ParseWarning`s in the owned layer, but it
/// reports them by *path*, and a graph record has none — a skipped record must
/// name itself on the boot line, which is where an operator looks.
fn graph_skill_layer(records: Vec<SkillRecord>) -> (Vec<OwnedSkill>, GraphSkillStats) {
    let mut layer = Vec::with_capacity(records.len());
    let mut stats = GraphSkillStats::default();
    for record in records {
        if let Err(error) = graph_skills::validate(&record) {
            tracing::warn!(skill = %record.name, %error, "graph-carried skill skipped");
            stats.skipped.push(format!("{}: {error}", record.name));
            continue;
        }
        let rendered = graph_skills::render_markdown(&record);
        // Second gate: the registry parses this blob's frontmatter and
        // hard-errors on anything it cannot read, so prove it reads *before*
        // handing it over rather than discovering it inside `finalise`.
        if let Err(error) =
            mcp_methods::server::skills::parse_skill(&rendered, std::path::Path::new("<graph>"))
        {
            tracing::warn!(skill = %record.name, %error, "graph-carried skill skipped");
            stats.skipped.push(format!("{}: {error}", record.name));
            continue;
        }
        stats.served += 1;
        stats.body_bytes += record.body.len();
        layer.push(OwnedSkill {
            name: record.name,
            body: rendered,
        });
    }
    (layer, stats)
}

/// Render the bare-`graph_overview` skills index: one
/// `name [tier] — summary` line per skill this session actually serves,
/// sorted by name.
///
/// Built from the active set mcp-methods 0.4.11 returns from `serve_prompts`
/// (and keeps behind `McpServer::active_skills`), not from a second
/// activation pass of our own: a skill is listed here for exactly the reason
/// it is in `prompts/list`, and the two cannot answer differently.
///
/// The tier is load-bearing for the reader. A `[lazy]` skill's body is *not*
/// in its target tools' descriptions, so an agent that reads this index has to
/// know the line is an invitation to call `skill(name)` and not a summary of
/// something it already has. `None` when nothing is active, which leaves the
/// overview byte-identical to a deployment that never opted in.
fn render_skills_index(active: &[ActiveSkill]) -> Option<String> {
    let lines: Vec<String> = active
        .iter()
        .map(|skill| {
            format!(
                "{} [{}] \u{2014} {}",
                skill.name,
                skill.delivery,
                skill_summary(&skill.description)
            )
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<skills count=\"{}\" get-via=\"skill(name)\">\n{}\n</skills>",
        lines.len(),
        lines.join("\n")
    ))
}

/// Rebuilds the skill layer against whatever graph is active *now*.
///
/// Two things go stale when the served graph changes. A graph swap
/// (`reload_graph`, `load_graph`, `create_graph`) replaces the data the graph
/// layer was **read** from, so the skills resolved at boot describe a graph
/// the server no longer serves. A workspace root activation (`set_root_dir`,
/// `repo_management`) publishes the first graph there has ever been, so every
/// `applies_when: graph_has_node_type:` predicate that resolved false against
/// no graph at boot has to be **re-evaluated**. mcp-methods 0.4.11 is the
/// first cut that can fix either after `serve`:
/// [`SkillReloader::reinject_skills`] strips the previous injection and
/// re-runs the pass from `&self`.
///
/// Armed **after** `install_skills` — the composition it re-runs needs the
/// closed tool surface — in the five modes whose graph can change after boot:
/// graph, watch and vault, whose graph layer is read at boot and is replaced
/// by a swap, and the two workspace modes, whose graph does not exist at boot
/// at all. In source-root and bare modes there is no graph either way, so a
/// rebuild would recompose a byte-identical registry and spend a
/// `tools/list_changed` on nothing. The recipe catalogue is deliberately
/// *not* rebuilt: its route names — the fixed pair and every `tool:` a query
/// declared — are settled before the allowlist, and the catalogue is
/// documented immutable after boot.
///
/// **One swap path does not refresh.** The per-call freshness re-read
/// (`GraphState::ensure_graph_fresh`, which re-opens the served file when the
/// bytes on disk change under a `--graph` server) runs from inside the
/// graph's own write path, where re-reading the skill records would take the
/// read lock the swap still holds; a `--graph` server whose file is rewritten
/// under it therefore keeps the skills it booted with until something calls
/// `reload_graph`.
///
/// The watcher's lazy workspace rebuild used to be in the same list, and was
/// a real defect once a vault could carry `.kglite/skills/*.md`: editing a
/// skill rebuilt the graph and served the old skill set until restart. It now
/// refreshes through `GraphState::after_rebuild`, which
/// `ensure_workspace_graph_fresh` fires outside every lock the rebuild took.
#[derive(Clone, Default)]
pub(crate) struct SkillRefresher {
    inner: Arc<RwLock<Option<Box<RefreshInner>>>>,
}

struct RefreshInner {
    reloader: SkillReloader,
    manifest: Option<Manifest>,
    producer_layer: Vec<OwnedSkill>,
    mode: Mode,
    graph_state: GraphState,
    recipe_catalog_summary: Option<crate::recipe_queries::CatalogSummary>,
    skills_index: SkillsIndexSlot,
    peer: PeerSlot,
}

/// The boot state [`SkillRefresher::arm`] captures for a later rebuild.
///
/// A struct rather than a parameter list for the same reason
/// `KgliteToolParams` is one: the set is the boot wiring, it grows with it,
/// and named fields at the single call site read as that wiring rather than
/// as a positional sequence.
pub(crate) struct RefreshInputs<'a> {
    pub(crate) reloader: SkillReloader,
    pub(crate) manifest: Option<&'a Manifest>,
    /// The rendered producer layer, carried so a rebuild recomposes the same
    /// registry without re-validating records that already passed at boot.
    pub(crate) producer: &'a ProducerSkills,
    pub(crate) mode: &'a Mode,
    pub(crate) graph_state: &'a GraphState,
    /// Dimensions of the catalogue actually served, carried so a rebuild
    /// re-renders the same overview hint the boot pass did.
    pub(crate) recipe_catalog_summary: Option<crate::recipe_queries::CatalogSummary>,
    pub(crate) skills_index: &'a SkillsIndexSlot,
    pub(crate) peer: &'a PeerSlot,
}

impl SkillRefresher {
    /// Fill the slot the graph-swap handlers already hold a clone of.
    pub(crate) fn arm(&self, inputs: RefreshInputs<'_>) {
        let RefreshInputs {
            reloader,
            manifest,
            producer,
            mode,
            graph_state,
            recipe_catalog_summary,
            skills_index,
            peer,
        } = inputs;
        if !matches!(
            mode,
            Mode::Graph { .. }
                | Mode::Watch { .. }
                | Mode::Vault { .. }
                | Mode::LocalWorkspace { .. }
                | Mode::Workspace { .. }
        ) {
            return;
        }
        *write_lock(&self.inner) = Some(Box::new(RefreshInner {
            reloader,
            manifest: manifest.cloned(),
            producer_layer: producer.layer(),
            mode: mode.clone(),
            graph_state: graph_state.clone(),
            recipe_catalog_summary,
            skills_index: skills_index.clone(),
            peer: peer.clone(),
        }));
    }

    /// Re-resolve and re-inject. Called from a tool handler that has just
    /// changed which graph is served — a graph swap, or a workspace root
    /// activation — **after** that call has returned, with every lock it took
    /// released. A no-op on an unarmed refresher.
    ///
    /// The call site matters and is not interchangeable. Running this from
    /// inside `GraphState::commit_workspace_graph` self-deadlocks: the
    /// recomposition reads the active graph through `with_kg`, and the commit
    /// holds that same `RwLock` for writing. Running it at the tail of the
    /// activation commit closure instead takes the framework's skill lock
    /// while mcp-methods still holds the activation and `root_swap` write
    /// locks, inverting the order against every tool handler that reaches a
    /// `Workspace` accessor. Outside the whole activation, in the handler, is
    /// the only site that holds neither.
    ///
    /// Failures are logged, never returned: the swap the caller performed did
    /// succeed, and turning a stale skill layer into a failed `reload_graph`
    /// would be a worse answer than a warning in the log.
    pub(crate) fn refresh(&self) {
        let guard = read_lock(&self.inner);
        let Some(inner) = guard.as_deref() else {
            return;
        };
        let (registry_result, stats) = compose_registry(
            inner.manifest.as_ref(),
            &inner.producer_layer,
            &inner.mode,
            &inner.graph_state,
            inner.recipe_catalog_summary,
        );
        let registry = match registry_result {
            Ok(registry) => registry,
            Err(error) => {
                tracing::warn!(%error, "skill layer not rebuilt after the graph change");
                return;
            }
        };
        log_parse_warnings(&registry);
        let active = match inner.reloader.reinject_skills(&registry) {
            Ok(active) => active,
            Err(refusal) => {
                tracing::warn!("{refusal}");
                return;
            }
        };
        *write_lock(&inner.skills_index) = render_skills_index(&active);
        tracing::info!(
            skills = active.len(),
            graph_skills = stats.served,
            "skill layer rebuilt after the graph change"
        );
        notify_peer(&inner.peer);
    }
}

/// Send `tools/list_changed` + `prompts/list_changed` for a rebuilt layer.
///
/// The peer is the one thing `reinject_skills` cannot do for us: it stores no
/// peers, and a dynamically registered tool handler is a plain
/// `Fn(Args) -> Result<String, String>` with no `RequestContext` in reach. So
/// the peer is captured from the `RunningService` `serve` returns and
/// published into [`PeerSlot`], which this reads. The notification itself is
/// `async`, and the handler is not, so it goes onto the ambient runtime — the
/// handler is dispatched from one. A client that never sees the notification
/// keeps serving its cached `tools/list` until it re-lists, which is why this
/// logs rather than failing quietly.
fn notify_peer(peer: &PeerSlot) {
    let Some(peer) = read_lock(peer).clone() else {
        tracing::warn!("skills rebuilt before the client connected; tools/list_changed not sent");
        return;
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                if let Err(error) = notify_skills_changed(&peer).await {
                    tracing::warn!(%error, "tools/list_changed notification failed");
                }
            });
        }
        Err(_) => tracing::warn!("no async runtime in reach; tools/list_changed not sent"),
    }
}

/// The one line an agent reads about a skill in the overview index: the
/// description's first sentence, or its first 160 bytes when no sentence ends
/// inside them. Newlines collapse so one skill is always one line.
fn skill_summary(description: &str) -> String {
    const MAX: usize = 160;
    let text = description.replace(['\n', '\r'], " ");
    let text = text.trim();
    let sentence_end = text.char_indices().find_map(|(index, ch)| {
        let after = index + ch.len_utf8();
        (ch == '.' && text[after..].chars().next().is_none_or(char::is_whitespace)).then_some(after)
    });
    match sentence_end {
        Some(end) if end <= MAX => text[..end].to_string(),
        _ if text.len() <= MAX => text.to_string(),
        _ => {
            let mut cut = MAX;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}\u{2026}", text[..cut].trim_end())
        }
    }
}

/// Decide what a failed registry build costs: the boot, or a warning.
///
/// A declared pack directory that is not there is an operator typo, and one
/// bad entry fails the *whole* build — the bundled methodology goes with it.
/// Nothing else in the session says so: the graph tools still answer, and
/// `--selftest` used to print PASSED over a server with every skill silently
/// gone. `source_root:` reports its own version of this state; skills had no
/// equivalent, so a missing pack refuses the boot and names both spellings of
/// the path.
///
/// Everything else — an unparseable skill file, one over the size limit — stays
/// a warning: those are content faults in files that do exist, they name
/// themselves in the log, and taking a deployment down for one of them is a
/// worse trade than serving it without skills.
fn report_registry_failure(error: SkillError, yaml_path: Option<&std::path::Path>) -> Result<()> {
    match error {
        // `PathNotFound` can only come from a manifest `skills:` entry, so the
        // path is always there when this arm is reached; the fallback exists
        // because the type cannot say so.
        SkillError::PathNotFound { .. } => bail!(
            "{error}. Declared by `skills:` in {}. Create the directory, or drop the entry \
             — a skills path that is not there disables every skill in the session, \
             bundled ones included.",
            yaml_path
                .unwrap_or(std::path::Path::new("<no manifest>"))
                .display()
        ),
        other => {
            tracing::warn!(error = %other, "skills registry build failed; skills disabled for this session");
            Ok(())
        }
    }
}

/// Add recipe methodology only when the validated catalog will register its
/// fixed routes. The skill's `tool_registered: run_recipe_query` predicate is
/// a second guard evaluated against the final visible tool set.
pub(crate) fn add_recipe_query_skill(
    registry: SkillRegistry,
    catalog_summary: Option<recipe_queries::CatalogSummary>,
) -> SkillRegistry {
    if catalog_summary.is_some() {
        registry.add_bundled(BundledSkill {
            name: "recipe_queries",
            body: RECIPE_QUERIES_SKILL,
        })
    } else {
        registry
    }
}

/// Fold [`DISCOVERY_STEER`] into `options.instructions` for the two
/// workspace modes. Appends (preserving any manifest `instructions:`) or
/// sets it when none exists; bails when the text already mentions the
/// always-registered graph tools so an opted-in manifest isn't duplicated.
pub(crate) fn apply_discovery_steer(mode: &Mode, mut options: ServerOptions) -> ServerOptions {
    if !matches!(mode, Mode::Workspace { .. } | Mode::LocalWorkspace { .. }) {
        return options;
    }
    let already = options
        .instructions
        .as_deref()
        .is_some_and(|s| s.to_lowercase().contains("always registered"));
    if already {
        return options;
    }
    options.instructions = Some(match options.instructions.take() {
        Some(existing) if !existing.trim().is_empty() => format!("{existing}\n\n{DISCOVERY_STEER}"),
        _ => DISCOVERY_STEER.to_string(),
    });
    options
}

/// Evaluates `applies_when:` predicates that depend on kglite's
/// runtime graph state. The framework dispatches `tool_registered:`
/// and `extension_enabled:` itself; this evaluator only handles the
/// two domain predicates that require knowing what node types and
/// properties the active graph carries.
///
/// Unknown `applies_when` keys are rejected while the skill file is
/// parsed. Returning `None` here handles a recognized clause that this
/// domain evaluator cannot answer; the framework records it as `Unknown`
/// and suppresses the skill.
pub(crate) struct KglitePredicateEvaluator {
    pub(crate) state: GraphState,
}

impl SkillPredicateEvaluator for KglitePredicateEvaluator {
    fn evaluate(&self, clause: &PredicateClause<'_>) -> Option<bool> {
        match clause {
            PredicateClause::GraphHasNodeType(types) => {
                Some(types.iter().any(|t| self.state.has_node_type(t)))
            }
            PredicateClause::GraphHasProperty {
                node_type,
                prop_name,
            } => Some(self.state.has_property(node_type, prop_name)),
            // Framework-internal predicates — `tool_registered` and
            // `extension_enabled` are dispatched against ServerOptions
            // by the framework itself, not via this evaluator.
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod skills_tests;

#[cfg(test)]
#[path = "skills_activation_tests.rs"]
mod skills_activation_tests;

#[cfg(test)]
#[path = "skills_budget_tests.rs"]
mod skills_budget_tests;
