//! Escaneo de carpetas `.docset`, lectura de `Info.plist`, índices SQLite
//! e instalación/extracción tarix.
//!
//! Toda la lógica es independiente de Tauri y está cubierta por tests.
//! Los docsets son entrada no confiable: lo corrupto se registra en
//! `ScanReport::issues` sin abortar el escaneo.

pub mod icon;
pub mod index;
pub mod model;
pub mod plist;
pub mod scanner;
pub mod tarix;

pub use icon::{icon_data_url_for, MAX_ICON_BYTES, MAX_ICON_DIM};
pub use index::{read_index, IndexData, IndexError, IndexSchema};
pub use model::{Docset, Entry, IssueKind, PendingTarix, ScanError, ScanIssue, ScanReport};
pub use plist::{apply_to_docset, read_info_plist};
pub use scanner::{scan_dir, slugify, unique_slug};
pub use tarix::{
    cache_dir_for, cache_is_ready, cleanup_stale_cache, ensure_extracted, ExtractOutcome,
    ExtractStatus, TarixError, TarixLimits,
};

#[cfg(test)]
pub(crate) use crate::test_support::fixture_or_skip;
