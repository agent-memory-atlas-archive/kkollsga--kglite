//! `kglite export` — the open-format exits: a lossless CSV tree with a
//! re-import blueprint, or RDF 1.2 (N-Quads / TriG).

use std::path::Path;

use anyhow::{Context, Result};
use clap::ValueEnum;
use kglite::api::io::{to_csv_dir, to_rdf, RdfExportOptions, RdfFormat};

use crate::load_graph;

/// Output format of `kglite export`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ExportFormat {
    /// Lossless CSV directory tree with `blueprint.json` and `manifest.json`.
    Csv,
    /// RDF 1.2 N-Quads.
    Nq,
    /// RDF 1.2 TriG.
    Trig,
}

impl ExportFormat {
    /// The format an output path names: `.nq` / `.trig`.
    fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "nq" => Some(Self::Nq),
            "trig" => Some(Self::Trig),
            _ => None,
        }
    }
}

pub(crate) fn run(
    graph_path: &Path,
    output: &Path,
    format: Option<ExportFormat>,
    base: Option<&str>,
    schema_org: bool,
) -> Result<()> {
    let format = format
        .or_else(|| ExportFormat::from_path(output))
        .with_context(|| {
            format!(
                "cannot infer the export format from {}; pass --format csv|nq|trig",
                output.display()
            )
        })?;
    if format == ExportFormat::Csv && (base.is_some() || schema_org) {
        anyhow::bail!("--base and --schema-org apply to the RDF formats only");
    }
    let graph = load_graph(graph_path)?;
    let output_str = output.to_string_lossy();
    match format {
        ExportFormat::Csv => {
            let summary = to_csv_dir(&graph, &output_str, None, &graph.parent_types)
                .map_err(|e| anyhow::anyhow!("CSV export failed: {e}"))?;
            eprintln!(
                "wrote {} files to {} ({} node types, {} relationship types)",
                summary.files_written,
                summary.output_dir,
                summary.nodes.len(),
                summary.connections.len()
            );
        }
        ExportFormat::Nq | ExportFormat::Trig => {
            let mut options = RdfExportOptions {
                format: if format == ExportFormat::Trig {
                    RdfFormat::TriG
                } else {
                    RdfFormat::NQuads
                },
                schema_org,
                ..RdfExportOptions::default()
            };
            if let Some(base) = base {
                options.base = base.to_string();
            }
            let summary = to_rdf(&graph, &output_str, None, &graph.parent_types, &options)
                .map_err(|e| anyhow::anyhow!("RDF export failed: {e}"))?;
            eprintln!(
                "wrote {} ({} statements)",
                summary.output_path, summary.statements
            );
        }
    }
    Ok(())
}
