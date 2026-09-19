//! `kglite okf …` — the offline front end for the vault format (VAULT.md).
//!
//! `check` is a converter's gate: it runs the real build, prints the §9 report
//! and sets the exit code. `build` is the same read kept — the vault as a
//! `.kgl`. `export` runs the other way, writing a `.kgl` back out as a vault
//! (§10). `status` answers the cheap lifecycle question — has this vault moved
//! since the `.kgl` was built (§12) — without reading a single note. All four
//! live here rather than in `lib.rs` so the command table stays a table;
//! `lib.rs` carries only the variant and the dispatch arm.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use kglite::okf::{BuildOptions, BuildReport, Dialect};

use crate::exec;
use crate::ReportedAgentFailure;

#[derive(Subcommand, Debug)]
pub(crate) enum OkfCommand {
    /// Check a vault against `VAULT.md` and report what a build would find.
    ///
    /// Runs the same read `build` runs and throws the graph away, so the
    /// report is what a build does rather than a second opinion about it.
    /// Exits non-zero when the report carries an error.
    Check {
        /// Path to the vault directory.
        directory: PathBuf,
        /// Which conventions to read the directory with; `okf.validate`
        /// defaults to the same one, while `okf.build` defaults to `okf`.
        #[arg(long, value_enum, default_value_t = OkfDialect::Obsidian)]
        dialect: OkfDialect,
        /// Fail on warnings too — what a converter's own test suite wants.
        #[arg(long)]
        strict: bool,
        /// Print the report as a JSON object instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Write a `.kgl` graph out as a vault directory.
    ///
    /// Only files a previous export wrote are replaced or removed; anything
    /// else in the directory is refused and named, which exits non-zero.
    Export {
        /// Path to the `.kgl` graph to write out.
        graph: PathBuf,
        /// Directory to write the vault into; created if it does not exist.
        directory: PathBuf,
        /// Replace files this export does not own, and ones edited since.
        #[arg(long)]
        force: bool,
        /// The directory the graph's attachments were read from, so their
        /// bytes are copied into the vault.
        #[arg(long, value_name = "DIR")]
        source_root: Option<PathBuf>,
        /// Write this edge type's edges as a table under this heading, keeping
        /// their properties (`VAULT.md` §10.6). Repeatable; merged over the
        /// source vault's own `export.edge_tables:`.
        #[arg(long = "edge-table", value_name = "TYPE=HEADING")]
        edge_tables: Vec<String>,
    },
    /// Print a vault's fingerprint, and whether a `.kgl` is still current.
    ///
    /// Reads no note: the fingerprint is a `stat` pass over the files a build
    /// would read (`VAULT.md` §12). With `--graph`, exits non-zero when that
    /// graph was built from an older state of the directory.
    Status {
        /// Path to the vault directory.
        directory: PathBuf,
        /// A `.kgl` built from this vault, to compare against it.
        #[arg(long, value_name = "FILE")]
        graph: Option<PathBuf>,
        /// Which conventions to read the directory with.
        #[arg(long, value_enum, default_value_t = OkfDialect::Obsidian)]
        dialect: OkfDialect,
    },
    /// Build a vault into a `.kgl` graph file.
    Build {
        /// Path to the vault directory.
        directory: PathBuf,
        /// Path to write the `.kgl` graph to.
        #[arg(short, long)]
        output: PathBuf,
        /// Which conventions to read the directory with.
        #[arg(long, value_enum, default_value_t = OkfDialect::Obsidian)]
        dialect: OkfDialect,
    },
}

/// The dialect names, as an enum rather than a string.
///
/// `Dialect::parse` falls back to `okf` for anything it does not recognise,
/// which is right for a library keyword and wrong at a terminal: `--dialect
/// obsidan` would silently read the vault as an OKF bundle and report a
/// different graph. clap refuses it here instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum OkfDialect {
    Obsidian,
    Okf,
    Loose,
}

impl From<OkfDialect> for Dialect {
    fn from(value: OkfDialect) -> Self {
        match value {
            OkfDialect::Obsidian => Dialect::Obsidian,
            OkfDialect::Okf => Dialect::Okf,
            OkfDialect::Loose => Dialect::Loose,
        }
    }
}

pub(crate) fn run(command: &OkfCommand) -> Result<()> {
    match command {
        OkfCommand::Check {
            directory,
            dialect,
            strict,
            json,
        } => check(directory, *dialect, *strict, *json),
        OkfCommand::Build {
            directory,
            output,
            dialect,
        } => build(directory, output, *dialect),
        OkfCommand::Export {
            graph,
            directory,
            force,
            source_root,
            edge_tables,
        } => export(graph, directory, *force, source_root.clone(), edge_tables),
        OkfCommand::Status {
            directory,
            graph,
            dialect,
        } => status(directory, graph.as_deref(), *dialect),
    }
}

/// `kglite okf export` — the graph as a vault, report on stderr.
///
/// Exits non-zero when the export refused a file, for the same reason `check`
/// exits non-zero on an error: a converter's CI wants to hear that the vault
/// on disk is not the one the graph describes.
fn export(
    graph_path: &Path,
    directory: &Path,
    force: bool,
    source_root: Option<PathBuf>,
    edge_tables: &[String],
) -> Result<()> {
    let graph = crate::load_graph(graph_path)?;
    let opts = kglite::okf::ExportOptions {
        force,
        source_root,
        edge_tables: parse_edge_tables(edge_tables)?,
        ..kglite::okf::ExportOptions::default()
    };
    let report = kglite::okf::export(&graph, directory, &opts)
        .map_err(|reason| anyhow::anyhow!("{reason}"))
        .with_context(|| format!("failed to export to {}", directory.display()))?;
    eprint!("{}", report.render());
    exec::write_stdout(&format!("wrote {}", directory.display()))?;
    if report.refusals.is_empty() {
        Ok(())
    } else {
        // The refusals are the diagnostic and stderr already carries them.
        Err(ReportedAgentFailure.into())
    }
}

/// `--edge-table TYPE=Heading`, as the map [`kglite::okf::ExportOptions`] takes.
///
/// The heading may hold `=` (`Worked on by = who?`), so the split is on the
/// *first* one; the type may not, so nothing is lost by it.
fn parse_edge_tables(args: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut out = std::collections::BTreeMap::new();
    for arg in args {
        let (conn_type, heading) = arg.split_once('=').with_context(|| {
            format!("--edge-table {arg}: expected TYPE=HEADING, as `WORKED_ON_BY=Worked on by`")
        })?;
        if conn_type.is_empty() || heading.trim().is_empty() {
            anyhow::bail!("--edge-table {arg}: both the edge type and the heading are required");
        }
        out.insert(conn_type.to_string(), heading.to_string());
    }
    Ok(out)
}

/// `kglite okf status` — the fingerprint, and the verdict when a graph is named.
///
/// Three outcomes, two exit codes, because the CLI has two
/// (`ReportedAgentFailure` is the whole of its non-zero vocabulary):
/// **current** exits 0, **stale** exits 1 with the verdict already printed,
/// and a graph that carries no provenance at all — one not built by
/// `okf build`, or written before the stamp existed — is a question this
/// command cannot answer, so it is an ordinary error (`Error: …` on stderr,
/// exit 1) rather than a third verdict.
fn status(directory: &Path, graph_path: Option<&Path>, dialect: OkfDialect) -> Result<()> {
    let current = kglite::okf::fingerprint(directory, &options(dialect))
        .map_err(|reason| anyhow::anyhow!("{reason}"))?;
    let Some(graph_path) = graph_path else {
        exec::write_stdout(&format!("{current:016x}  {}", directory.display()))?;
        return Ok(());
    };
    let graph = crate::load_graph(graph_path)?;
    let Some(stamped) = graph.source_fingerprint else {
        anyhow::bail!(
            "{} carries no vault provenance — it was not built from a directory by \
             `kglite okf build`, so there is nothing to compare {} against",
            graph_path.display(),
            directory.display()
        );
    };
    let root = graph.source_root.as_deref().unwrap_or("(unknown)");
    if stamped == current {
        exec::write_stdout(&format!(
            "current  {current:016x}  {} built from {root}",
            graph_path.display()
        ))?;
        return Ok(());
    }
    // The verdict is the diagnostic and stdout carries it; `ReportedAgentFailure`
    // turns this into a bare non-zero exit rather than a second explanation.
    exec::write_stdout(&format!(
        "stale    {stamped:016x} built, {current:016x} now  {} vs {}",
        graph_path.display(),
        directory.display()
    ))?;
    Err(ReportedAgentFailure.into())
}

fn options(dialect: OkfDialect) -> BuildOptions {
    BuildOptions::for_dialect(dialect.into())
}

/// `kglite okf check` — print the report, exit non-zero on an error.
fn check(directory: &Path, dialect: OkfDialect, strict: bool, json: bool) -> Result<()> {
    let report = kglite::okf::validate(directory, &options(dialect))
        .map_err(|reason| anyhow::anyhow!("{reason}"))?;
    if json {
        exec::write_stdout(&serde_json::to_string(&as_json(&report, strict))?)?;
    } else {
        exec::write_stdout_raw(&report.render())?;
    }
    if report.is_ok(strict) {
        Ok(())
    } else {
        // The report *is* the diagnostic, and it has already been printed;
        // `exit_code` turns this into a bare non-zero exit rather than
        // appending a second, vaguer explanation of the same failure.
        Err(ReportedAgentFailure.into())
    }
}

/// `kglite okf build` — the vault as a `.kgl`, with the same report printed.
///
/// The report goes to **stderr**: a build's contract is the file it wrote, and
/// a caller redirecting stdout is capturing a graph, not a summary.
fn build(directory: &Path, output: &Path, dialect: OkfDialect) -> Result<()> {
    let mut built = kglite::okf::build(directory, &options(dialect))
        .map_err(|reason| anyhow::anyhow!("{reason}"))
        .with_context(|| format!("failed to build {}", directory.display()))?;
    kglite::api::io::save_graph(&mut built.graph, &output.to_string_lossy())
        .with_context(|| format!("failed to save {}", output.display()))?;
    eprint!("{}", built.report.render());
    exec::write_stdout(&format!("wrote {}", output.display()))?;
    Ok(())
}

/// The report as JSON, for a harness that wants the lists rather than the text.
/// The keys are the Python `VaultReport`'s, so one vault reads alike from a
/// terminal, a `.py` test and a shell pipeline.
fn as_json(report: &BuildReport, strict: bool) -> serde_json::Value {
    serde_json::json!({
        "ok": report.is_ok(strict),
        "strict": strict,
        "counts": {
            "files_scanned": report.files_scanned,
            "concepts": report.concepts,
            "nodes_by_label": report.nodes_by_label,
            "edges_by_type": report.edges_by_type,
            "dangling": report.dangling,
            "folder_notes": report.folder_notes,
            "missing_attachments": report.missing_attachments,
            "ambiguous_attachments": report.ambiguous_attachments,
            "indexes_declared": report.indexes_declared,
            "text_indexes_built": report.text_indexes_built,
            "skills_imported": report.skills_imported,
            "recipes_imported": report.recipes_imported,
            "forced_splits": report.forced_splits,
            "embed_targets": report.embed_targets,
        },
        "errors": report.errors,
        "warnings": report.warnings,
    })
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn a_mistyped_sub_verb_is_an_unknown_subcommand() {
        // The root command has a fall-through `[GRAPH]` positional, so a typo
        // there opens an empty shell instead of failing. Inside the group
        // there is no such positional, and this asserts it stays that way.
        let error = Cli::try_parse_from(["kglite", "okf", "chekc", "vault"])
            .expect_err("a mistyped sub-verb must not fall through to anything");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        assert!(error.to_string().contains("chekc"), "{error}");
    }

    #[test]
    fn an_unknown_dialect_is_refused_rather_than_defaulted() {
        let error = Cli::try_parse_from(["kglite", "okf", "check", "v", "--dialect", "obsidan"])
            .expect_err("`Dialect::parse` would have silently read it as `okf`");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        assert!(
            error.to_string().contains("obsidian"),
            "the error names the spellings that work: {error}"
        );
    }

    #[test]
    fn export_takes_a_graph_and_a_directory() {
        assert!(Cli::try_parse_from(["kglite", "okf", "export", "v.kgl"]).is_err());
        assert!(Cli::try_parse_from(["kglite", "okf", "export", "v.kgl", "out"]).is_ok());
        assert!(Cli::try_parse_from([
            "kglite",
            "okf",
            "export",
            "v.kgl",
            "out",
            "--force",
            "--source-root",
            "vault"
        ])
        .is_ok());
        // `--source-root` takes a value; a bare flag is a parse error rather
        // than a silent "copy from nowhere".
        assert!(
            Cli::try_parse_from(["kglite", "okf", "export", "v.kgl", "out", "--source-root"])
                .is_err()
        );
    }

    #[test]
    fn build_requires_an_output_path() {
        assert!(Cli::try_parse_from(["kglite", "okf", "build", "vault"]).is_err());
        assert!(Cli::try_parse_from(["kglite", "okf", "build", "vault", "-o", "v.kgl"]).is_ok());
    }
}
