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
/// *not* rebuilt: its routes are fixed tool names settled before the
/// allowlist, and the catalogue is documented immutable after boot.
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
        assert!(body.contains("domain skill already selected the"));
        assert!(body.contains("call `list_recipe_queries()` once"));
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
    /// The generic `cypher_query` skill ships to every deployment — shipping,
    /// legal, maritime — with no `applies_when` gate, because Cypher applies
    /// to any graph. Code-graph *methodology* does not: it opened with the
    /// four-step code-graph workflow and "Never `grep` for a definition",
    /// ~1.5k tokens of instruction about a codebase, delivered verbatim to a
    /// graph of vessels. That content already lives in `code_graph_analysis`,
    /// which gates on `graph_has_node_type: [Function, Class]` and reaches
    /// the readers it is for.
    #[test]
    fn the_generic_cypher_skill_carries_no_code_graph_preamble() {
        let body = include_str!("../skills/cypher_query.md");
        for marker in [
            "Never `grep`",
            "Code-graph workflow",
            "read_code_source(qualified_name=…)",
        ] {
            assert!(
                !body.contains(marker),
                "cypher_query.md still carries code-graph methodology: {marker:?}"
            );
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

#[cfg(test)]
#[path = "skills_activation_tests.rs"]
mod skills_activation_tests;

#[cfg(test)]
#[path = "skills_budget_tests.rs"]
mod skills_budget_tests;
