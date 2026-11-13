//! Rutas seguras para las páginas de un paquete DevDocs.
//!
//! Las claves de `db.json` son entrada no confiable (`properties/--*`,
//! `at-rules/@media`, incluso `../x`). Cada clave se convierte en una ruta
//! de fichero que:
//! - no contiene `.`/`..`/segmentos vacíos ni caracteres inválidos en
//!   Windows (`<>:"|?*\` y controles), ni `%`/`#` (ambiguos en URLs: el
//!   resolvedor decodifica `%XX` en bucle);
//! - no usa nombres reservados de Windows (`CON`, `NUL`, `COM1`...);
//! - es única sin distinguir mayúsculas (macOS/Windows).

use std::collections::{HashMap, HashSet};

/// Longitud máxima (en caracteres) de un segmento de ruta.
const MAX_SEGMENT_CHARS: usize = 100;

const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Resultado de asignar rutas a las claves.
#[derive(Debug, Default)]
pub(super) struct PathMap {
    /// Clave DevDocs → ruta relativa a `Documents/` (con `.html`).
    pub files: HashMap<String, String>,
    /// Claves cuyo nombre hubo que modificar.
    pub sanitized: usize,
    /// Rutas que chocaban sin distinguir mayúsculas.
    pub collisions: usize,
}

/// Segmento seguro a partir de un segmento de clave.
pub(super) fn safe_segment(seg: &str) -> String {
    let mut out: String = seg
        .chars()
        .take(MAX_SEGMENT_CHARS)
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/' | '%' | '#' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.trim().is_empty() || out == "." || out == ".." {
        return String::from("_");
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    out
}

/// Asigna una ruta única a cada clave. `keys` debe venir ordenada para que
/// el resultado sea determinista.
pub(super) fn build(keys: &[&str]) -> PathMap {
    let mut map = PathMap::default();
    let mut used: HashSet<String> = HashSet::with_capacity(keys.len());
    for key in keys {
        let segs: Vec<String> = key.split('/').map(safe_segment).collect();
        let base = format!("{}.html", segs.join("/"));
        if base != format!("{key}.html") {
            map.sanitized += 1;
        }
        let mut candidate = base.clone();
        let mut n = 2;
        while !used.insert(candidate.to_lowercase()) {
            map.collisions += 1;
            let stem = base.strip_suffix(".html").unwrap_or(&base);
            candidate = format!("{stem}-{n}.html");
            n += 1;
        }
        map.files.insert((*key).to_string(), candidate);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_segments_are_neutralized() {
        assert_eq!(safe_segment(".."), "_");
        assert_eq!(safe_segment("."), "_");
        assert_eq!(safe_segment(""), "_");
        assert_eq!(safe_segment("a:b*c?"), "a_b_c_");
        assert_eq!(safe_segment("100%25"), "100_25");
        assert_eq!(safe_segment("con"), "_con");
        assert_eq!(safe_segment("NUL.txt"), "_NUL.txt");
        assert_eq!(safe_segment("trail. "), "trail");
        assert_eq!(safe_segment("--*"), "--_");
        assert_eq!(safe_segment("@media"), "@media");
    }

    #[test]
    fn traversal_keys_stay_inside() {
        let map = build(&["../../etc/passwd", "/abs/path", "a//b"]);
        for path in map.files.values() {
            assert!(!path.starts_with('/'), "{path}");
            assert!(path
                .split('/')
                .all(|s| s != ".." && s != "." && !s.is_empty()));
        }
    }

    #[test]
    fn case_insensitive_collisions_get_suffix() {
        let map = build(&["Foo", "foo", "FOO"]);
        let mut files: Vec<&String> = map.files.values().collect();
        files.sort();
        let lower: HashSet<String> = files.iter().map(|f| f.to_lowercase()).collect();
        assert_eq!(lower.len(), 3);
        assert!(map.collisions >= 2);
    }

    #[test]
    fn long_segments_are_truncated() {
        let long = "x".repeat(500);
        assert!(safe_segment(&long).chars().count() <= MAX_SEGMENT_CHARS);
    }
}
