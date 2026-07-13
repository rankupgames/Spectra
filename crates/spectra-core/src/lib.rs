//! Spectra's topology extraction, selection, and rendering engine.

mod adapters;
mod error;
pub mod graph;
mod index;
pub mod ledger;
mod render;
mod select;

pub use adapters::{SupportedLanguage, is_supported_path, supported_languages};
pub use error::{Error, Result};
pub use index::{CodeIndex, INDEX_VERSION, IndexReport, SourceSpan};
pub use ledger::{
    LedgerAnchor, LedgerEvent, LedgerEventKind, LedgerProjection, LedgerState, LedgerStore,
};
pub use render::{MapArtifact, MapRelation, RenderOptions, SourceAnchor, render_map};
pub use select::{Selection, SelectionOptions, select_subgraph};

use std::path::Path;

/// Refresh an index and render a query-focused topology in one operation.
pub fn map_project(
    project: &Path,
    query: &str,
    max_nodes: usize,
    output_dir: &Path,
) -> Result<MapArtifact> {
    let (index, _) = sync_project(project)?;
    let selection = select_subgraph(
        &index,
        query,
        SelectionOptions {
            max_nodes: max_nodes.clamp(1, 96),
        },
    );
    let artifact = render_map(
        &index,
        &selection,
        query,
        output_dir,
        RenderOptions::default(),
    )?;
    let map_id = artifact
        .png_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("topology")
        .to_owned();
    LedgerStore::transaction(project, |ledger| {
        ledger.append(LedgerEventKind::MapRendered {
            map_id,
            query: ledger::redact_text(query),
            anchors: artifact
                .anchors
                .iter()
                .map(|(visual_id, anchor)| ledger::LedgerAnchor {
                    visual_id: visual_id.clone(),
                    path: anchor.path.clone(),
                    start_line: anchor.start_line,
                    end_line: anchor.end_line,
                })
                .collect(),
            nodes: artifact.node_count,
            truncated: artifact.truncated,
        })?;
        Ok(())
    })?;
    Ok(artifact)
}

/// Reconcile the project index and record material repository changes.
pub fn sync_project(project: &Path) -> Result<(CodeIndex, IndexReport)> {
    let (index, report, _lock) = CodeIndex::refresh_holding_lock(project)?;
    LedgerStore::transaction(project, |ledger| {
        if report.changed > 0 || report.removed > 0 || ledger.is_empty() {
            ledger.append(LedgerEventKind::RepositorySynced {
                files: report.files,
                changed: report.changed,
                removed: report.removed,
                nodes: report.nodes,
                edges: report.edges,
            })?;
        }
        Ok(())
    })?;
    Ok((index, report))
}
