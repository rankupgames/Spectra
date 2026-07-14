//! Spectra's topology extraction, selection, and context-ledger engine.

use std::path::Path;

mod adapters;
mod error;
pub mod graph;
mod index;
pub mod ledger;
mod select;

pub use adapters::{SupportedLanguage, is_supported_path, supported_languages};
pub use error::{Error, Result};
pub use index::{CodeIndex, INDEX_VERSION, IndexReport, SourceSpan};
pub use ledger::{
    LedgerAnchor, LedgerEvent, LedgerEventKind, LedgerFactsProjection, LedgerProjection,
    LedgerSource, LedgerState, LedgerStore,
};
pub use select::{Selection, SelectionOptions, select_subgraph};

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
