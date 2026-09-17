//! `kglite okf …` — the offline front end for the vault format (VAULT.md).
//!
//! `check` is a converter's gate: it runs the real build, prints the §9 report
//! and sets the exit code. `build` is the same read kept — the vault as a
//! `.kgl`. Both live here rather than in `lib.rs` so the command table stays a
//! table; `lib.rs` carries only the variant and the dispatch arm.

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
        /// Which conventions to read the directory with.
        #[arg(long, value_enum, default_value_t = OkfDialect::Obsidian)]
        dialect: OkfDialect,
        /// Fail on warnings too — what a converter's own test suite wants.
        #[arg(long)]
        strict: bool,
        /// Print the report as a JSON object instead of text.
        #[arg(long)]
        json: bool,
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
    }
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
    fn build_requires_an_output_path() {
        assert!(Cli::try_parse_from(["kglite", "okf", "build", "vault"]).is_err());
        assert!(Cli::try_parse_from(["kglite", "okf", "build", "vault", "-o", "v.kgl"]).is_ok());
    }
}
