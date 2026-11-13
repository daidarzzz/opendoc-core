//! Resolves document requests within a docset's `Documents/` directory.
//!
//! [`resolve_request`] removes query and fragment suffixes and decodes URL
//! escapes before validating the path. It checks that the target is a file and
//! that its canonical path stays within the canonical documents directory.

use std::path::{Path, PathBuf};

/// Fichero resuelto listo para servir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    /// Ruta canónica dentro de `Documents/`.
    pub path: PathBuf,
    /// MIME por extensión.
    pub mime: &'static str,
}

/// Por qué no se sirve una petición.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// La ruta pide salir de `Documents/` o es absoluta/externa.
    #[error("path outside Documents/: {0}")]
    OutsideDocuments(String),
    /// No existe o no es un fichero servible.
    #[error("not found: {0}")]
    NotFound(String),
    /// Es o pasa por un enlace simbólico.
    #[error("symlinks not allowed: {0}")]
    Symlink(String),
}

/// Decodifica `%XX` repetidamente hasta estabilizar (máx 5 rondas).
/// `+` NO se toca (en rutas no es espacio). Secuencias rotas se dejan.
fn percent_decode_repeated(raw: &str) -> String {
    let mut current = raw.to_string();
    for _ in 0..5 {
        let next = percent_decode_once(&current);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

/// Una ronda de decodificación `%XX` (hex insensible a mayúsculas).
fn percent_decode_once(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 3 <= bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = &s[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resuelve `raw_path` (parte tras `<id>/`, con posible `?`/`#`) dentro de
/// `documents`. Nunca devuelve rutas de fuera, aunque existan.
pub fn resolve_request(documents: &Path, raw_path: &str) -> Result<ResolvedFile, ResolveError> {
    // Corta query y fragmento.
    let no_query = raw_path.split(['?', '#']).next().unwrap_or(raw_path);
    let decoded = percent_decode_repeated(no_query);
    if decoded.contains('\0') {
        return Err(ResolveError::OutsideDocuments(raw_path.to_string()));
    }
    // Barras de Windows se tratan como separadores (luego se valida igual).
    let unified = decoded.replace('\\', "/");
    // Letra de unidad (`C:/`, `C:`) o ruta absoluta.
    if has_drive_letter(&unified) || unified.starts_with('/') {
        return Err(ResolveError::OutsideDocuments(raw_path.to_string()));
    }
    // Segmentos: `..` prohibido, `.` y vacíos se saltan.
    let mut rel = PathBuf::new();
    for seg in unified.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(ResolveError::OutsideDocuments(raw_path.to_string()));
        }
        rel.push(seg);
    }
    if rel.as_os_str().is_empty() {
        return Err(ResolveError::NotFound(raw_path.to_string()));
    }
    let joined = documents.join(&rel);
    // Debe existir y ser fichero (los directorios se prueban aparte).
    let meta = std::fs::symlink_metadata(&joined)
        .map_err(|_| ResolveError::NotFound(raw_path.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(ResolveError::Symlink(raw_path.to_string()));
    }
    if !meta.is_file() {
        // Directorio → prueba su index.html; si no, 404.
        if meta.is_dir() {
            let index = joined.join("index.html");
            if let Ok(resolved) = checked_canonical(documents, &index, raw_path) {
                return Ok(resolved);
            }
        }
        return Err(ResolveError::NotFound(raw_path.to_string()));
    }
    checked_canonical(documents, &joined, raw_path)
}

/// `C:` / `c:` al inicio (tras normalizar separadores).
fn has_drive_letter(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

thread_local! {
    /// Última raíz `Documents/` canonicalizada en este hilo. Una página
    /// pide decenas de recursos del mismo docset y en Windows
    /// `canonicalize` abre un handle cada vez. Fail-closed: si la carpeta
    /// cambia después (otro destino, symlink), los ficheros nuevos ya no
    /// empiezan por la raíz guardada y se rechazan.
    static DOCS_CANON: std::cell::RefCell<Option<(PathBuf, PathBuf)>> =
        const { std::cell::RefCell::new(None) };
}

/// `documents.canonicalize()` recordando la última raíz del hilo.
fn canonical_documents(documents: &Path) -> std::io::Result<PathBuf> {
    DOCS_CANON.with(|cell| {
        if let Some((raw, canon)) = cell.borrow().as_ref() {
            if raw == documents {
                return Ok(canon.clone());
            }
        }
        let canon = documents.canonicalize()?;
        *cell.borrow_mut() = Some((documents.to_path_buf(), canon.clone()));
        Ok(canon)
    })
}

/// Verifica canonicalización + prefijo (symlinks intermedios, `..`
/// residuales) y calcula el MIME.
fn checked_canonical(
    documents: &Path,
    candidate: &Path,
    raw_path: &str,
) -> Result<ResolvedFile, ResolveError> {
    let docs_canon =
        canonical_documents(documents).map_err(|_| ResolveError::NotFound(raw_path.to_string()))?;
    let file_canon = candidate
        .canonicalize()
        .map_err(|_| ResolveError::NotFound(raw_path.to_string()))?;
    if !file_canon.starts_with(&docs_canon) {
        return Err(ResolveError::Symlink(raw_path.to_string()));
    }
    if !file_canon.is_file() {
        return Err(ResolveError::NotFound(raw_path.to_string()));
    }
    Ok(ResolvedFile {
        mime: content_type_for(&file_canon),
        path: file_canon,
    })
}

/// MIME por extensión (minúsculas). Desconocidas → `octet-stream`.
/// Sin sufijo `charset`: los ficheros se autodeclaran (como Zeal).
pub fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js" | "mjs") => "text/javascript",
        Some("json" | "map") => "application/json",
        Some("xml") => "text/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("bmp") => "image/bmp",
        Some("ico" | "cur") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("eot") => "application/vnd.ms-fontobject",
        Some("txt" | "md" | "text") => "text/plain",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Árbol sintético: `Documents/{a/b.html, "a b.html", "café.html",
    /// sub/index.html}`.
    fn sample_docs(dir: &Path) -> PathBuf {
        let docs = dir.join("Documents");
        for f in ["a/b.html", "a b.html", "café.html", "sub/index.html"] {
            let p = docs.join(f);
            std::fs::create_dir_all(p.parent().expect("padre")).expect("mkdirs");
            std::fs::write(&p, b"<html></html>").expect("write");
        }
        docs
    }

    #[test]
    fn valid_nested_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        let r = resolve_request(&docs, "a/b.html").expect("resuelve");
        assert_eq!(r.mime, "text/html");
        assert!(r.path.starts_with(docs.canonicalize().expect("canon")));
    }

    #[test]
    fn traversal_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        for evil in [
            "../fuera.html",
            "a/../../fuera.html",
            "..",
            "a/b.html/../../..",
            "/absoluta.html",
            "///etc/passwd",
        ] {
            assert!(
                matches!(
                    resolve_request(&docs, evil),
                    Err(ResolveError::OutsideDocuments(_))
                ),
                "debería rechazar {evil}"
            );
        }
    }

    #[test]
    fn absolute_and_drive_paths_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        for evil in [
            "C:/Windows/x.html",
            "C:x.html",
            "c:/y.html",
            "D:relativa.html",
        ] {
            assert!(
                matches!(
                    resolve_request(&docs, evil),
                    Err(ResolveError::OutsideDocuments(_))
                ),
                "debería rechazar {evil}"
            );
        }
    }

    #[test]
    fn backslash_is_normalized_then_checked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        // `\` es separador: `nope\nada.html` → `nope/nada.html`, que no
        // existe → NotFound (no traversal válido).
        assert!(matches!(
            resolve_request(&docs, "nope\\nada.html"),
            Err(ResolveError::NotFound(_))
        ));
        // `..` con barras invertidas → OutsideDocuments.
        assert!(matches!(
            resolve_request(&docs, "..\\fuera.html"),
            Err(ResolveError::OutsideDocuments(_))
        ));
    }

    #[test]
    fn encoded_traversal_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        for evil in [
            "%2e%2e/fuera.html",
            "%2E%2E%2Ffuera.html",
            "%252e%252e/fuera.html", // doble codificado
            "..%2ffuera.html",
            "%c0%ae%c0%ae/x.html", // overlong utf-8: no decodifica a `.`
        ] {
            let res = resolve_request(&docs, evil);
            assert!(!res.is_ok(), "{evil} no debe resolverse: {res:?}");
        }
    }

    #[test]
    fn spaces_unicode_and_percent_forms_resolve() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        assert!(resolve_request(&docs, "a b.html").is_ok());
        assert!(resolve_request(&docs, "a%20b.html").is_ok());
        assert!(resolve_request(&docs, "café.html").is_ok());
        assert!(resolve_request(&docs, "caf%C3%A9.html").is_ok());
    }

    #[test]
    fn anchor_and_query_are_stripped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        assert!(resolve_request(&docs, "a/b.html#frag").is_ok());
        assert!(resolve_request(&docs, "a/b.html?x=1#frag").is_ok());
        assert!(resolve_request(&docs, "a/b.html?x=1").is_ok());
    }

    #[test]
    fn directory_serves_index_or_404() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        assert!(resolve_request(&docs, "sub").is_ok());
        assert!(resolve_request(&docs, "sub/").is_ok());
        assert!(matches!(
            resolve_request(&docs, "a"),
            Err(ResolveError::NotFound(_))
        ));
    }

    #[test]
    #[cfg(unix)]
    fn symlink_file_and_dir_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        std::os::unix::fs::symlink(docs.join("a/b.html"), docs.join("link.html"))
            .expect("symlink fichero");
        std::os::unix::fs::symlink("/etc", docs.join("etc")).expect("symlink dir");
        assert!(matches!(
            resolve_request(&docs, "link.html"),
            Err(ResolveError::Symlink(_))
        ));
        assert!(matches!(
            resolve_request(&docs, "etc/passwd"),
            Err(ResolveError::Symlink(_) | ResolveError::NotFound(_))
        ));
    }

    #[test]
    #[cfg(windows)]
    fn symlink_file_rejected_windows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = sample_docs(dir.path());
        std::os::windows::fs::symlink_file(docs.join("a/b.html"), docs.join("link.html"))
            .expect("symlink fichero");
        assert!(matches!(
            resolve_request(&docs, "link.html"),
            Err(ResolveError::Symlink(_))
        ));
    }

    #[test]
    fn content_type_table() {
        let cases = [
            ("a.html", "text/html"),
            ("A.HTM", "text/html"),
            ("s.css", "text/css"),
            ("s.js", "text/javascript"),
            ("s.mjs", "text/javascript"),
            ("d.json", "application/json"),
            ("s.map", "application/json"),
            ("d.xml", "text/xml"),
            ("i.svg", "image/svg+xml"),
            ("i.png", "image/png"),
            ("i.jpg", "image/jpeg"),
            ("i.jpeg", "image/jpeg"),
            ("i.gif", "image/gif"),
            ("i.webp", "image/webp"),
            ("i.ico", "image/x-icon"),
            ("f.woff2", "font/woff2"),
            ("f.ttf", "font/ttf"),
            ("n.txt", "text/plain"),
            ("x.bin", "application/octet-stream"),
            ("sinext", "application/octet-stream"),
        ];
        for (name, mime) in cases {
            assert_eq!(content_type_for(Path::new(name)), mime, "{name}");
        }
    }
}
