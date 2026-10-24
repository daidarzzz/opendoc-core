//! Soporte de docsets tarix (T10, adelantado de v0.2).
//!
//! Un tarix es un `.tgz` con el docset clásico dentro + `tarixIndex.db`
//! (catálogo para extracción parcial; aquí extracción completa en caché,
//! on-demand por rangos queda como optimización).
//!
//! - La caché vive en datos de la app (`tarix-cache/<id>/`), nunca en la
//!   carpeta del usuario. Se indexa por id y manifest (`tgz_len/mtime`):
//!   si coincide y existen `Documents/` + `docSet.dsidx` se reutiliza.
//! - Se quita el primer componente (`Python.docset/`); raíz única o error.
//! - Seguridad fail-closed: `..`, absolutas, unidad (`C:`), NUL → abortan
//!   TODA la extracción. Symlinks/hardlinks/especiales y nombres inválidos
//!   en Windows (`:`, reservados...) se saltan y cuentan (0 en los docsets
//!   reales; abortar 600 MB por un recurso roto es desproporcionado y los
//!   enlaces jamás se siguen).
//! - Topes: 4 GB y 200.000 entradas, contando BYTES REALES escritos (no
//!   los declarados) con tope durante la copia.
//! - Anti-caché-rota: se extrae a `<id>.tmp/` y se renombra al terminar;
//!   reemplazar usa `<id>.old` intermedio; `cleanup_stale` barre restos.
//! - Rutas largas: medido máx 139 (C++) + base ~60 < 260 → sin `\\?\`.
//!   Si un futuro docset excede, falla como `TarixFailed` visible.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Tope de bytes reales extraídos (4 GB).
pub const MAX_EXTRACT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Tope de entradas procesadas (200.000).
pub const MAX_EXTRACT_FILES: u64 = 200_000;

/// Topes configurables (los tests inyectan pequeños).
#[derive(Debug, Clone, Copy)]
pub struct TarixLimits {
    /// Tope de bytes reales.
    pub max_bytes: u64,
    /// Tope de entradas.
    pub max_files: u64,
}

impl Default for TarixLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_EXTRACT_BYTES,
            max_files: MAX_EXTRACT_FILES,
        }
    }
}

/// Resultado de una extracción.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractOutcome {
    /// Ficheros y directorios creados.
    pub files: u64,
    /// Bytes reales escritos en disco.
    pub bytes: u64,
    /// Entradas saltadas (enlaces, nombres inválidos en Windows).
    pub skipped: u64,
}

/// Estado tras `ensure_extracted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractStatus {
    /// Caché válida reutilizada (manifest + `Documents/` + `.dsidx`).
    Reused,
    /// Extraído ahora.
    Extracted(ExtractOutcome),
}

/// Manifest de caché para saber cuándo reextraer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CacheManifest {
    tgz_len: u64,
    tgz_mtime: u64,
    files: u64,
    bytes: u64,
    skipped: u64,
}

/// Error de extracción (por docset, como issue `TarixFailed`).
#[derive(Debug, thiserror::Error)]
pub enum TarixError {
    /// No se puede abrir el `.tgz`.
    #[error("cannot read {path}: {source}")]
    Unreadable {
        /// Ruta del `.tgz`.
        path: PathBuf,
        /// Error subyacente.
        #[source]
        source: std::io::Error,
    },
    /// Flujo gzip/tar roto o truncado.
    #[error("corrupt or truncated tgz: {0}")]
    Corrupt(String),
    /// Entrada maliciosa (`..`, absoluta, unidad, NUL, raíz múltiple).
    #[error("unsafe entry in tgz: {0}")]
    UnsafeEntry(String),
    /// Límite de tamaño superado (bytes reales).
    #[error("size limit exceeded ({max} bytes)")]
    TooBig {
        /// Tope aplicado.
        max: u64,
    },
    /// Límite de entradas superado.
    #[error("file limit exceeded ({max})")]
    TooManyFiles {
        /// Tope aplicado.
        max: u64,
    },
    /// Fallo de disco al escribir (p. ej. disco lleno).
    #[error("disk failure: {0}")]
    Io(#[from] std::io::Error),
}

/// Directorio de caché para un id.
pub fn cache_dir_for(cache_base: &Path, id: &str) -> PathBuf {
    cache_base.join(id)
}

/// `true` si la caché de `id` ya está extraída y coincide con el `.tgz`
/// (manifest + `Documents/` + `.dsidx`). No extrae nada: permite reutilizar
/// cachés creadas por la app sin bloquear.
pub fn cache_is_ready(cache_base: &Path, id: &str, tgz_path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(tgz_path) else {
        return false;
    };
    let stamp = TarixStamp {
        len: meta.len(),
        mtime: mtime_secs(&meta),
    };
    cache_is_valid(&cache_dir_for(cache_base, id), &stamp)
}

/// `true` si la caché de `id` tiene `Contents/Resources/Documents/`
/// extraída. La extracción real solo renombra a la carpeta final cuando ha
/// terminado, así que la presencia de `Documents/` basta para reutilizarla
/// (las entradas se leen siempre del `.dsidx` original).
pub fn cache_has_documents(cache_base: &Path, id: &str) -> bool {
    cache_dir_for(cache_base, id)
        .join("Contents/Resources/Documents")
        .is_dir()
}

/// Asegura la caché: la reutiliza si es válida o extrae de cero.
pub fn ensure_extracted(
    cache_base: &Path,
    id: &str,
    tgz_path: &Path,
    limits: TarixLimits,
) -> Result<ExtractStatus, TarixError> {
    let meta = std::fs::metadata(tgz_path).map_err(|source| TarixError::Unreadable {
        path: tgz_path.to_path_buf(),
        source,
    })?;
    let stamp = TarixStamp {
        len: meta.len(),
        mtime: mtime_secs(&meta),
    };
    let final_dir = cache_dir_for(cache_base, id);
    if cache_is_valid(&final_dir, &stamp) {
        return Ok(ExtractStatus::Reused);
    }
    let outcome = extract_tgz(tgz_path, &final_dir, limits)?;
    let manifest = CacheManifest {
        tgz_len: stamp.len,
        tgz_mtime: stamp.mtime,
        files: outcome.files,
        bytes: outcome.bytes,
        skipped: outcome.skipped,
    };
    let json =
        serde_json::to_string_pretty(&manifest).map_err(|e| TarixError::Corrupt(e.to_string()))?;
    std::fs::write(final_dir.join("manifest.json"), json)?;
    Ok(ExtractStatus::Extracted(outcome))
}

/// Huella del `.tgz` para el manifest.
struct TarixStamp {
    len: u64,
    mtime: u64,
}

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Caché válida: manifest coincidente + `Documents/` + `.dsidx`.
fn cache_is_valid(final_dir: &Path, stamp: &TarixStamp) -> bool {
    let manifest: CacheManifest = match std::fs::read_to_string(final_dir.join("manifest.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(manifest) => manifest,
        None => return false,
    };
    manifest.tgz_len == stamp.len
        && manifest.tgz_mtime == stamp.mtime
        && final_dir.join("Contents/Resources/Documents").is_dir()
        && final_dir.join("Contents/Resources/docSet.dsidx").is_file()
}

/// Extrae con reemplazo seguro (final→old, tmp→final, borrar old).
fn extract_tgz(
    tgz_path: &Path,
    final_dir: &Path,
    limits: TarixLimits,
) -> Result<ExtractOutcome, TarixError> {
    let tmp_dir = final_dir.with_extension("tmp");
    if tmp_dir.exists() {
        std::fs::remove_dir_all(&tmp_dir)?;
    }
    let outcome = unpack_validated(tgz_path, &tmp_dir, limits).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    })?;
    replace_final(final_dir, &tmp_dir)?;
    Ok(outcome)
}

/// Sustituye la caché: final→old, tmp→final, borrar old.
fn replace_final(final_dir: &Path, tmp_dir: &Path) -> Result<(), TarixError> {
    let old_dir = final_dir.with_extension("old");
    if old_dir.exists() {
        std::fs::remove_dir_all(&old_dir)?;
    }
    if final_dir.exists() {
        std::fs::rename(final_dir, &old_dir)?;
    }
    let renamed = std::fs::rename(tmp_dir, final_dir);
    if renamed.is_err() {
        // Intenta dejar lo anterior como estaba.
        if old_dir.exists() && !final_dir.exists() {
            let _ = std::fs::rename(&old_dir, final_dir);
        }
        renamed?;
    }
    if old_dir.exists() {
        std::fs::remove_dir_all(&old_dir)?;
    }
    Ok(())
}

/// Borra cachés huérfanas: subdirectorios de `cache_base` que ningún
/// pendiente ni instalado referencia (docset borrado o convertido a
/// clásico). No toca `.tmp`/`.old` (los gestiona `cleanup_stale_cache`)
/// ni ficheros sueltos. Best-effort: los errores se ignoran.
pub fn sweep_unreferenced_caches(
    cache_base: &Path,
    referenced: &std::collections::HashSet<String>,
) {
    let entries = match std::fs::read_dir(cache_base) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".tmp") || name.ends_with(".old") {
            continue;
        }
        if !referenced.contains(&name) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}
/// Limpieza al arrancar: borra `<id>.tmp*` y resuelve `<id>.old`
/// (si falta el final, restaura el old; si no, lo borra).
pub fn cleanup_stale_cache(cache_base: &Path) {
    let entries = match std::fs::read_dir(cache_base) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if name.ends_with(".tmp") {
            let _ = std::fs::remove_dir_all(&path);
        } else if name.ends_with(".old") {
            let stem = name.trim_end_matches(".old");
            let final_dir = cache_base.join(stem);
            if !final_dir.exists() {
                let _ = std::fs::rename(&path, &final_dir);
            } else {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

/// Desembala validando cada entrada. Solo sale con caché completa o error
/// (y tmp borrado por el llamador). Se expone para reutilizar la
/// validación en `install` (F2) sin duplicarla ni debilitarla: mismas
/// reglas (raíz única, sin escapes, sin enlaces, topes por bytes reales).
#[doc(hidden)]
pub fn unpack_validated(
    tgz_path: &Path,
    tmp_dir: &Path,
    limits: TarixLimits,
) -> Result<ExtractOutcome, TarixError> {
    // FASE 0: solo medida (descompresión+validación+escritura van en el
    // mismo bucle; no se separan sin cambiar el algoritmo).
    let t_unpack = std::time::Instant::now();
    let file = std::fs::File::open(tgz_path).map_err(|source| TarixError::Unreadable {
        path: tgz_path.to_path_buf(),
        source,
    })?;
    // Búfer de lectura: el iterador del tar pide cabeceras (512 B) y
    // trozos pequeños; sin esto cada petición baja a `read(2)`.
    let buffered = std::io::BufReader::with_capacity(1024 * 1024, file);
    let gz = flate2::read::GzDecoder::new(buffered);
    let mut archive = tar::Archive::new(gz);
    let entries = archive
        .entries()
        .map_err(|e| TarixError::Corrupt(e.to_string()))?;

    let mut outcome = ExtractOutcome {
        files: 0,
        bytes: 0,
        skipped: 0,
    };
    let mut root: Option<String> = None;
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    for entry in entries {
        let mut entry = entry.map_err(|e| TarixError::Corrupt(e.to_string()))?;
        outcome.files += 1;
        if outcome.files > limits.max_files {
            return Err(TarixError::TooManyFiles {
                max: limits.max_files,
            });
        }
        let raw = entry.path_bytes().into_owned();
        if raw.contains(&0) {
            return Err(TarixError::UnsafeEntry("<NUL en nombre>".to_string()));
        }
        let raw_str = String::from_utf8_lossy(&raw).into_owned();
        let kind = entry.header().entry_type();
        // Enlaces y especiales: saltar y contar (jamás seguir).
        if kind.is_symlink() || kind.is_hard_link() {
            outcome.skipped += 1;
            outcome.files -= 1;
            continue;
        }
        if !kind.is_file() && !kind.is_dir() {
            outcome.skipped += 1;
            outcome.files -= 1;
            continue;
        }
        let rel = match strip_single_root(&raw_str, &mut root)? {
            Some(rel) => rel,
            None => {
                outcome.files -= 1;
                continue; // La propia raíz contenedora.
            }
        };
        // Nombre inválido en Windows (p. ej. `:` en espejos archivados):
        // saltar y contar. Sanear rompería la resolución del índice.
        if has_invalid_windows_component(&rel) {
            outcome.skipped += 1;
            outcome.files -= 1;
            continue;
        }
        let dest = tmp_dir.join(&rel);
        // Directorios ya creados (memoizados): evita repetir
        // `create_dir_all` —que re-recorre componentes— por cada fichero
        // del mismo padre. Solo contiene rutas ya validadas y creadas.
        if kind.is_dir() {
            if created_dirs.insert(dest.clone()) {
                std::fs::create_dir_all(&dest)?;
            }
            continue;
        }
        // Fichero: padres + copia con tope de bytes reales.
        if let Some(parent) = dest.parent() {
            if created_dirs.insert(parent.to_path_buf()) {
                std::fs::create_dir_all(parent)?;
            }
        }
        let remaining = limits.max_bytes.saturating_sub(outcome.bytes);
        let file_out = std::fs::File::create(&dest)?;
        // Búfer de escritura: coalesca los `write(2)` de 8 KiB de la copia.
        // `flush` explícito (el `drop` lo silenciaría ante disco lleno).
        let mut out = std::io::BufWriter::with_capacity(128 * 1024, file_out);
        let mut take = (&mut entry).take(remaining);
        let n = std::io::copy(&mut take, &mut out)?;
        out.flush()?;
        outcome.bytes += n;
        if n == remaining {
            let mut probe = [0u8; 1];
            if entry.read(&mut probe)? != 0 {
                return Err(TarixError::TooBig {
                    max: limits.max_bytes,
                });
            }
        }
    }
    // FASE 0: solo medida.
    let ms = crate::profile::ms_since(t_unpack).max(1);
    crate::profile::mark(
        "install",
        format_args!(
            "extraccion tgz={} files={} bytes={} skipped={} ms={ms} mbs={:.1}",
            tgz_path.display(),
            outcome.files,
            outcome.bytes,
            outcome.skipped,
            outcome.bytes as f64 / 1_048_576.0 / (ms as f64 / 1000.0)
        ),
    );
    Ok(outcome)
}

/// Quita el primer componente (raíz única del tgz). `None` = la propia
/// raíz (se salta en silencio). Raíz múltiple o ruta maliciosa → error.
/// Se expone para `install` (misma política, distinto destino).
#[doc(hidden)]
pub fn strip_single_root(
    raw: &str,
    root: &mut Option<String>,
) -> Result<Option<PathBuf>, TarixError> {
    if raw.starts_with('/') || has_drive_letter(raw) {
        return Err(TarixError::UnsafeEntry(raw.to_string()));
    }
    let mut parts: Vec<&str> = raw
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if parts.is_empty() {
        return Ok(None);
    }
    let first = parts.remove(0);
    if first == ".." {
        return Err(TarixError::UnsafeEntry(raw.to_string()));
    }
    match root {
        Some(expected) if expected != first => {
            return Err(TarixError::UnsafeEntry(format!("multiple roots: {first}")));
        }
        Some(_) => {}
        None => *root = Some(first.to_string()),
    }
    if parts.is_empty() {
        return Ok(None);
    }
    if parts.contains(&"..") {
        return Err(TarixError::UnsafeEntry(raw.to_string()));
    }
    Ok(Some(parts.iter().collect()))
}

/// `C:` / `c:` al inicio.
fn has_drive_letter(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Componente inválido en Windows: prohibidos, controles, reservados o
/// terminados en espacio/punto. Se saltan y cuentan (sanear rompería la
/// resolución del índice). Se expone para `install`.
#[doc(hidden)]
pub fn has_invalid_windows_component(rel: &Path) -> bool {
    rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.chars()
            .any(|ch| matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') || ch.is_control())
            || s.ends_with(' ')
            || s.ends_with('.')
            || is_reserved_name(&s)
    })
}

/// CON, PRN, AUX, NUL, COM1-9, LPT1-9 (sin extensión, insensible a caso).
fn is_reserved_name(s: &str) -> bool {
    matches!(
        s.split('.')
            .next()
            .unwrap_or(s)
            .to_ascii_uppercase()
            .as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construye un `.tgz` sintético en `dir` con las entradas dadas:
    /// `(ruta, contenido | None=directorio)` más symlinks aparte.
    fn make_tgz(dir: &Path, files: &[(&str, Option<&str>)], symlinks: &[(&str, &str)]) -> PathBuf {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let path = dir.join("test.tgz");
        let file = std::fs::File::create(&path).expect("crear tgz");
        let enc = GzEncoder::new(file, Compression::fast());
        let mut archive = tar::Builder::new(enc);
        for (name, content) in files {
            if let Some(content) = content {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, name, content.as_bytes())
                    .expect("añadir fichero");
            } else {
                archive.append_dir(name, ".").expect("añadir dir");
            }
        }
        for (link, target) in symlinks {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name(target).expect("link name");
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            archive
                .append_data(&mut header, link, std::io::empty())
                .expect("symlink");
        }
        archive
            .into_inner()
            .expect("cerrar gz")
            .finish()
            .expect("finish");
        path
    }

    #[test]
    fn extracts_stripping_first_component() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tgz = make_tgz(
            dir.path(),
            &[
                ("Root/", None),
                ("Root/Contents/Resources/Documents/a.txt", Some("hola")),
                ("Root/Contents/Resources/docSet.dsidx", Some("")),
            ],
            &[],
        );
        let status =
            ensure_extracted(dir.path(), "demo", &tgz, TarixLimits::default()).expect("extraer");
        let outcome = match status {
            ExtractStatus::Extracted(o) => o,
            ExtractStatus::Reused => panic!("debería extraer la primera vez"),
        };
        assert_eq!(outcome.skipped, 0);
        let base = dir.path().join("demo");
        assert_eq!(
            std::fs::read_to_string(base.join("Contents/Resources/Documents/a.txt")).expect("leer"),
            "hola"
        );
        assert!(base.join("manifest.json").is_file());
        // Segunda vez: reutiliza sin tocar nada.
        assert_eq!(
            ensure_extracted(dir.path(), "demo", &tgz, TarixLimits::default()).expect("reusar"),
            ExtractStatus::Reused
        );
    }

    #[test]
    fn malicious_paths_abort_at_validation() {
        // El builder de tar ya rechaza `..` al crear; la validación
        // propia se prueba directo sobre la función.
        let mut root: Option<String> = None;
        assert!(strip_single_root("../evil.txt", &mut root).is_err());
        assert!(strip_single_root("/abs.txt", &mut root).is_err());
        assert!(strip_single_root("C:/win.txt", &mut root).is_err());
        assert!(strip_single_root("Root/../../evil.txt", &mut root).is_err());
        assert!(strip_single_root("Root", &mut root).unwrap().is_none());
        // Raíz múltiple.
        assert!(strip_single_root("Other/x.txt", &mut root).is_err());
        let _ = root;
    }

    #[test]
    fn malicious_archive_aborts_without_valid_cache() {
        // Raíz múltiple sí se puede construir con el builder.
        let dir = tempfile::tempdir().expect("tempdir");
        let tgz = make_tgz(
            dir.path(),
            &[("A/x.txt", Some("a")), ("B/y.txt", Some("b"))],
            &[],
        );
        let err = ensure_extracted(dir.path(), "demo", &tgz, TarixLimits::default())
            .expect_err("debe abortar");
        assert!(matches!(err, TarixError::UnsafeEntry(_)));
        assert!(
            !dir.path().join("demo").exists(),
            "sin caché válida tras abortar"
        );
    }

    #[test]
    fn multiple_roots_abort() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tgz = make_tgz(
            dir.path(),
            &[("A/x.txt", Some("a")), ("B/y.txt", Some("b"))],
            &[],
        );
        let err = ensure_extracted(dir.path(), "demo", &tgz, TarixLimits::default())
            .expect_err("debe abortar");
        assert!(matches!(err, TarixError::UnsafeEntry(_)));
    }

    #[test]
    fn symlinks_and_bad_names_are_skipped_and_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tgz = make_tgz(
            dir.path(),
            &[
                ("Root/ok.txt", Some("ok")),
                ("Root/Malo:dos.txt", Some("x")),
                ("Root/AUX.txt", Some("x")),
                ("Root/trailingdot.", Some("x")),
            ],
            &[("Root/link", "ok.txt")],
        );
        let status =
            ensure_extracted(dir.path(), "demo", &tgz, TarixLimits::default()).expect("extraer");
        match status {
            ExtractStatus::Extracted(o) => assert_eq!(o.skipped, 4, "{o:?}"),
            ExtractStatus::Reused => panic!("debería extraer"),
        }
        let base = dir.path().join("demo");
        assert!(base.join("ok.txt").is_file());
        assert!(!base.join("link").exists(), "symlink jamás creado");
    }

    #[test]
    fn limits_abort() {
        let tiny = TarixLimits {
            max_bytes: 10,
            max_files: 1000,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let tgz = make_tgz(
            dir.path(),
            &[("Root/grande.txt", Some("123456789012345"))],
            &[],
        );
        let err = ensure_extracted(dir.path(), "demo", &tgz, tiny).expect_err("tope bytes");
        assert!(matches!(err, TarixError::TooBig { .. }));
        assert!(!dir.path().join("demo").exists());

        let tiny_files = TarixLimits {
            max_bytes: u64::MAX,
            max_files: 2,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let tgz = make_tgz(
            dir.path(),
            &[
                ("Root/", None),
                ("Root/a.txt", Some("a")),
                ("Root/b.txt", Some("b")),
                ("Root/c.txt", Some("c")),
            ],
            &[],
        );
        let err = ensure_extracted(dir.path(), "demo", &tgz, tiny_files).expect_err("tope files");
        assert!(matches!(err, TarixError::TooManyFiles { .. }));
    }

    #[test]
    fn real_python_extracts_with_zero_skips() {
        let Some(tgz) =
            super::super::fixture_or_skip("Python_3.docset/Contents/Resources/tarix.tgz")
        else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        match ensure_extracted(dir.path(), "python", &tgz, TarixLimits::default()).expect("extraer")
        {
            ExtractStatus::Extracted(o) => {
                eprintln!(
                    "python: files={} bytes={} skipped={}",
                    o.files, o.bytes, o.skipped
                );
                assert!(o.files > 1000);
                assert_eq!(o.skipped, 0);
            }
            ExtractStatus::Reused => panic!("caché fresca: debería extraer"),
        }
        assert!(dir
            .path()
            .join("python/Contents/Resources/Documents/doc/index.html")
            .is_file());
        assert!(dir
            .path()
            .join("python/Contents/Resources/docSet.dsidx")
            .is_file());
    }

    #[test]
    fn real_cpp_extracts_skipping_colon_names() {
        let Some(tgz) = super::super::fixture_or_skip("C++.docset/Contents/Resources/tarix.tgz")
        else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        match ensure_extracted(dir.path(), "cpp", &tgz, TarixLimits::default()).expect("extraer") {
            ExtractStatus::Extracted(o) => {
                eprintln!(
                    "c++: files={} bytes={} skipped={}",
                    o.files, o.bytes, o.skipped
                );
                assert!(o.files > 20000);
                assert!(o.skipped > 100, "los `:` de C++ deben saltarse");
            }
            ExtractStatus::Reused => panic!("caché fresca: debería extraer"),
        }
        assert!(dir.path().join("cpp/Contents/Resources/Documents").is_dir());
    }

    #[test]
    fn stale_tmp_and_old_are_cleaned() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("a.tmp")).expect("tmp");
        std::fs::create_dir_all(dir.path().join("b.old")).expect("old");
        std::fs::write(dir.path().join("b.old/x.txt"), "viejo").expect("write");
        // Con final ausente, el old se restaura.
        cleanup_stale_cache(dir.path());
        assert!(!dir.path().join("a.tmp").exists());
        assert!(!dir.path().join("b.old").exists());
        assert!(dir.path().join("b/x.txt").is_file());
        // Con final presente, el old se borra.
        std::fs::create_dir_all(dir.path().join("c")).expect("final");
        std::fs::create_dir_all(dir.path().join("c.old")).expect("old");
        cleanup_stale_cache(dir.path());
        assert!(dir.path().join("c").is_dir());
        assert!(!dir.path().join("c.old").exists());
    }

    #[test]
    fn sweep_drops_only_unreferenced_caches() {
        use std::collections::HashSet;
        let dir = tempfile::tempdir().expect("tempdir");
        // Referenciada (pendiente o instalada): se conserva.
        std::fs::create_dir_all(dir.path().join("demo/Contents")).expect("demo");
        // Huérfanas: manifest solo, contenido completo sin referencia.
        std::fs::create_dir_all(dir.path().join("viejo")).expect("viejo");
        std::fs::write(dir.path().join("viejo/manifest.json"), "{}").expect("manifest");
        std::fs::create_dir_all(dir.path().join("otro/Contents")).expect("otro");
        // `.tmp`/`.old` no son de este barrido.
        std::fs::create_dir_all(dir.path().join("x.tmp")).expect("tmp");
        let referenced: HashSet<String> = ["demo".to_string()].into_iter().collect();
        sweep_unreferenced_caches(dir.path(), &referenced);
        assert!(dir.path().join("demo").is_dir());
        assert!(!dir.path().join("viejo").exists());
        assert!(!dir.path().join("otro").exists());
        assert!(dir.path().join("x.tmp").is_dir());
    }

    #[test]
    fn large_file_roundtrips_exactly() {
        // 5 MB de un tirón: ejercita búfers sin cambiar el resultado.
        let dir = tempfile::tempdir().expect("tempdir");
        let big = "x".repeat(5 * 1024 * 1024);
        let tgz = make_tgz(dir.path(), &[("Root/grande.bin", Some(big.as_str()))], &[]);
        let status =
            ensure_extracted(dir.path(), "grande", &tgz, TarixLimits::default()).expect("extraer");
        match status {
            ExtractStatus::Extracted(o) => {
                assert_eq!(o.bytes, 5 * 1024 * 1024);
                assert_eq!(o.skipped, 0);
            }
            ExtractStatus::Reused => panic!("debería extraer"),
        }
        let out = std::fs::read(dir.path().join("grande/grande.bin")).expect("leer");
        assert_eq!(out.len(), big.len());
        assert!(out.iter().all(|b| *b == b'x'));
    }
}
