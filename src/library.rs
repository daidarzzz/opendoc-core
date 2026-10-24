//! Carga compartida de una carpeta de docsets.
//!
//! La app y el CLI necesitan exactamente el mismo recorrido:
//!
//! 1. Escanear la carpeta (`docset::scan_dir`).
//! 2. Reutilizar las cachés tarix ya extraídas (por defecto **no** se
//!    extrae nada; el CLI nunca bloquea al usuario).
//! 3. Leer los índices de los docsets clásicos (aprovechando las entradas
//!    que el escáner ya abrió para resolver la página de inicio).
//! 4. Devolver docsets, pendientes, issues y todas las entradas.
//!
//! Nada de Tauri ni de red aquí: solo disco y SQLite.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::docset::tarix::{
    cache_dir_for, cache_is_ready, ensure_extracted, sweep_unreferenced_caches,
};
use crate::docset::{
    apply_to_docset, read_index, read_info_plist, scan_dir, Docset, Entry, IssueKind, PendingTarix,
    ScanError, ScanIssue, TarixError, TarixLimits,
};

/// Opciones de carga.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Base de `tarix-cache/<id>/` (datos de la app en producción).
    pub tarix_cache: PathBuf,
    /// Si es `false` (por defecto) solo se reutilizan cachés existentes;
    /// si es `true` se extrae lo que falte (carga explícita y lenta).
    pub extract_missing: bool,
}

impl LoadOptions {
    /// Opciones del CLI: reutilizar cachés, nunca extraer.
    pub fn new(tarix_cache: impl Into<PathBuf>) -> Self {
        Self {
            tarix_cache: tarix_cache.into(),
            extract_missing: false,
        }
    }
}

/// Todo lo cargado de una carpeta de docsets.
#[derive(Debug)]
pub struct Library {
    /// Carpeta escaneada.
    pub source_dir: PathBuf,
    /// Docsets abribles, ordenados por nombre.
    pub docsets: Vec<Docset>,
    /// Tarix detectados pero no extraídos.
    pub pending: Vec<PendingTarix>,
    /// Incidencias no fatales del escaneo/índices.
    pub issues: Vec<ScanIssue>,
    /// Entradas de todos los docsets abribles.
    pub entries: Vec<Entry>,
}

impl Library {
    /// Docset instalado por id.
    pub fn docset(&self, id: &str) -> Option<&Docset> {
        self.docsets.iter().find(|docset| docset.id == id)
    }
}

/// Error fatal de carga (solo la carpeta raíz).
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// La ruta no es una carpeta.
    #[error("not a directory: {}", path.display())]
    InvalidDir {
        /// Ruta que se intentó cargar.
        path: PathBuf,
    },
    /// El escaneo no pudo leer la raíz.
    #[error(transparent)]
    Scan(#[from] ScanError),
}

/// Un tarix ya disponible en la caché: `Docset` + entradas del índice.
#[derive(Debug)]
pub struct InstalledTarix {
    /// Docset con `contents_path` en la caché extraída.
    pub docset: Docset,
    /// Entradas leídas del `.dsidx` original.
    pub entries: Vec<Entry>,
}

/// Carga reutilizando solo cachés tarix existentes (nunca extrae).
pub fn load(source_dir: &Path, tarix_cache: &Path) -> Result<Library, LoadError> {
    load_with_options(source_dir, &LoadOptions::new(tarix_cache))
}

/// Carga con opciones explícitas (la app puede extraer lo que falte).
pub fn load_with_options(source_dir: &Path, opts: &LoadOptions) -> Result<Library, LoadError> {
    if !source_dir.is_dir() {
        return Err(LoadError::InvalidDir {
            path: source_dir.to_path_buf(),
        });
    }
    let report = scan_dir(source_dir)?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut issues = report.issues;
    let mut docsets = report.docsets;

    // Entradas que el escáner ya leyó al resolver la página de inicio.
    let mut scanned: HashMap<String, Vec<Entry>> = HashMap::new();
    for entry in report.index_entries {
        scanned
            .entry(entry.docset_id.clone())
            .or_default()
            .push(entry);
    }

    let mut pending = Vec::new();
    for tarix in &report.pending_tarix {
        let installed = match installed_from_cache(&opts.tarix_cache, tarix) {
            Ok(installed) => Some(installed),
            Err(_) if opts.extract_missing => install_pending(&opts.tarix_cache, tarix).ok(),
            Err(_) => None,
        };
        match installed {
            Some(installed) => {
                entries.extend(installed.entries);
                docsets.push(installed.docset);
            }
            None => {
                if opts.extract_missing
                    && !has_issue(&issues, &tarix.root_path, &IssueKind::TarixFailed)
                {
                    issues.push(ScanIssue {
                        path: tarix.root_path.clone(),
                        kind: IssueKind::TarixFailed,
                    });
                }
                pending.push(tarix.clone());
            }
        }
    }

    // Cachés que nadie referencia (docset borrado o convertido a clásico).
    {
        let mut referenced: HashSet<String> = pending.iter().map(|p| p.id.clone()).collect();
        for docset in &docsets {
            if let Ok(rel) = docset.contents_path.strip_prefix(&opts.tarix_cache) {
                if let Some(id) = rel.components().next() {
                    referenced.insert(id.as_os_str().to_string_lossy().into_owned());
                }
            }
        }
        sweep_unreferenced_caches(&opts.tarix_cache, &referenced);
    }

    docsets.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    for docset in &docsets {
        if docset.contents_path.starts_with(&opts.tarix_cache) {
            continue; // Tarix ya integrado arriba con sus entradas.
        }
        if let Some(known) = scanned.remove(&docset.id) {
            entries.extend(known);
            continue;
        }
        if has_issue(&issues, &docset.root_path, &IssueKind::InvalidIndex) {
            continue;
        }
        let dsidx = docset.contents_path.join("Resources/docSet.dsidx");
        match read_index(&dsidx, &docset.id) {
            Ok(data) => entries.extend(data.entries),
            Err(_) => {
                if !has_issue(&issues, &docset.root_path, &IssueKind::InvalidIndex) {
                    issues.push(ScanIssue {
                        path: docset.root_path.clone(),
                        kind: IssueKind::InvalidIndex,
                    });
                }
            }
        }
    }

    Ok(Library {
        source_dir: source_dir.to_path_buf(),
        docsets,
        pending,
        issues,
        entries,
    })
}

/// Construye el `InstalledTarix` desde una caché ya extraída y válida.
pub fn installed_from_cache(
    tarix_cache: &Path,
    pending: &PendingTarix,
) -> Result<InstalledTarix, TarixError> {
    let resources = pending.root_path.join("Contents/Resources");
    let tgz = resources.join("tarix.tgz");
    if !cache_is_ready(tarix_cache, &pending.id, &tgz) {
        return Err(TarixError::Corrupt(format!(
            "cache not ready for {}",
            pending.id
        )));
    }
    let extracted_contents = cache_dir_for(tarix_cache, &pending.id).join("Contents");
    let mut docset = Docset {
        id: pending.id.clone(),
        name: pending.name.clone(),
        platform: None,
        version: None,
        bundle_id: None,
        home_path: None,
        icon: pending.icon.clone(),
        root_path: pending.root_path.clone(),
        contents_path: extracted_contents,
    };
    let orig_contents = pending.root_path.join("Contents");
    if let Ok(Some(info)) = read_info_plist(&orig_contents) {
        apply_to_docset(&mut docset, &info);
    }
    let orig_dsidx = resources.join("docSet.dsidx");
    let data = read_index(&orig_dsidx, &pending.id)
        .map_err(|e| TarixError::Corrupt(format!("index: {e}")))?;
    if docset.home_path.is_none() {
        if let Some(first) = data.entries.first() {
            docset.home_path = Some(first.path.clone());
        }
    }
    Ok(InstalledTarix {
        docset,
        entries: data.entries,
    })
}

/// Extrae un tarix pendiente y lo devuelve listo (operación lenta).
pub fn install_pending(
    tarix_cache: &Path,
    pending: &PendingTarix,
) -> Result<InstalledTarix, TarixError> {
    let tgz = pending.root_path.join("Contents/Resources/tarix.tgz");
    ensure_extracted(tarix_cache, &pending.id, &tgz, TarixLimits::default())?;
    installed_from_cache(tarix_cache, pending)
}

/// `true` si ya hay un issue igual.
fn has_issue(issues: &[ScanIssue], path: &Path, kind: &IssueKind) -> bool {
    issues.iter().any(|i| i.path == path && &i.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_directory() {
        let cache = tempfile::tempdir().expect("tempdir");
        let err =
            load(Path::new("definitivamente/no/existe"), cache.path()).expect_err("debe fallar");
        assert!(matches!(err, LoadError::InvalidDir { .. }));
    }

    #[test]
    fn loads_fixtures_and_reuses_scanned_entries() {
        if crate::test_support::fixture_or_skip("CSS.docset").is_none() {
            return;
        }
        let cache = tempfile::tempdir().expect("tempdir");
        let lib = load(&crate::test_support::fixtures_dir(), cache.path()).expect("cargar");
        let mut ids: Vec<&str> = lib.docsets.iter().map(|d| d.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["css", "swift"]);
        let mut pending: Vec<&str> = lib.pending.iter().map(|p| p.id.as_str()).collect();
        pending.sort_unstable();
        assert_eq!(pending, vec!["c", "python-3"]);
        assert!(lib.issues.is_empty());
        assert_eq!(lib.entries.len(), 1249 + 9467);
        assert!(lib.docset("css").is_some());
        assert!(lib.docset("swift").is_some());
    }
}
