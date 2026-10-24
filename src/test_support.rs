//! Utilidades compartidas por los tests (fixtures reales opcionales).
//!
//! Los docsets de prueba no viven en este repositorio (son pesados). Los
//! tests se saltan con un aviso si no están: no fallan en un clon limpio.
//!
//! Orden de búsqueda:
//! 1. `OPENDOC_FIXTURES` (ruta explícita).
//! 2. `<crate>/tests/fixtures` (si existe).
//! 3. `../opendoc/src-tauri/tests/fixtures` (checkout hermano de la app).

use std::path::{Path, PathBuf};

/// Carpeta de fixtures resuelta, exista o no.
pub fn fixtures_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OPENDOC_FIXTURES") {
        return PathBuf::from(dir);
    }
    let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        return PathBuf::from("tests/fixtures");
    };
    let root = Path::new(&manifest);
    let local = root.join("tests/fixtures");
    if local.is_dir() {
        return local;
    }
    let sibling = root.join("../opendoc/src-tauri/tests/fixtures");
    if sibling.is_dir() {
        return sibling;
    }
    local
}

/// Ruta a una fixture real o `None` si no está descargada (con aviso).
pub fn fixture_or_skip(relative: &str) -> Option<PathBuf> {
    let path = fixtures_dir().join(relative);
    if path.exists() {
        Some(path)
    } else {
        eprintln!(
            "SKIP: falta la fixture {relative} (define OPENDOC_FIXTURES o consulta \
             src-tauri/tests/fixtures/README.md)"
        );
        None
    }
}
