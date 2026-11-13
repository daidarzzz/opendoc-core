//! Manejo de enlaces y anclas del HTML de DevDocs (sin dependencias de
//! HTML/regex: el formato es estable y generado por DevDocs).
//!
//! Los fragmentos de DevDocs enlazan con rutas relativas *sin extensión*
//! (`../values/box-edge#visual-box`) resueltas contra la URL de la página.
//! Aquí se resuelven contra la clave de la página y se reescriben hacia el
//! fichero `.html` real.

use std::collections::{BTreeSet, HashMap, HashSet};

/// Decisión del visitante de enlaces.
pub(super) enum Rewrite {
    /// Dejar el `href` como está.
    Keep,
    /// Sustituir el valor (ya sin escapar; se escapa al escribir).
    Replace(String),
}

/// Revierte las entidades básicas que aparecen en atributos.
pub(super) fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

/// Escapa un valor para ponerlo entre comillas dobles o como texto.
pub(super) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// `true` si el `href` lleva esquema (`https:`, `mailto:`, `data:`...) o es
/// protocolo-relativo (`//host/..`).
pub(super) fn is_external(href: &str) -> bool {
    if href.starts_with("//") {
        return true;
    }
    let mut chars = href.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
            return false;
        }
    }
    false
}

/// Separa `ruta`, descartando la query, y `#fragmento`.
pub(super) fn split_href(href: &str) -> (&str, Option<&str>) {
    let (before, frag) = match href.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (href, None),
    };
    let path = before.split('?').next().unwrap_or(before);
    (path, frag)
}

/// Decodifica `%XX` una vez (UTF-8 con pérdida).
pub(super) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hi = char::from(bytes[i + 1]).to_digit(16);
            let lo = char::from(bytes[i + 2]).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Codifica una ruta relativa dejando legibles los caracteres habituales.
pub(super) fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'/'
                    | b'@'
                    | b'+'
                    | b','
                    | b'='
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b';'
            );
        if keep {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Resuelve `rel` (sin query/fragmento) contra la página `base_key` y
/// devuelve la clave destino. Sube como mucho hasta la raíz.
pub(super) fn resolve_key(base_key: &str, rel: &str) -> String {
    let decoded = percent_decode(rel);
    let mut stack: Vec<&str> = if decoded.is_empty() {
        return base_key.to_string();
    } else if decoded.starts_with('/') {
        Vec::new()
    } else {
        let mut v: Vec<&str> = base_key.split('/').collect();
        v.pop(); // quita el nombre de la página: queda el "directorio".
        v
    };
    for seg in decoded.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    stack.join("/")
}

/// Ruta relativa entre dos ficheros de `Documents/`.
pub(super) fn relative_file(from_file: &str, to_file: &str) -> String {
    let from_dirs: Vec<&str> = {
        let mut v: Vec<&str> = from_file.split('/').collect();
        v.pop();
        v
    };
    let to: Vec<&str> = to_file.split('/').collect();
    let mut common = 0;
    while common < from_dirs.len() && common + 1 < to.len() && from_dirs[common] == to[common] {
        common += 1;
    }
    let mut out = "../".repeat(from_dirs.len() - common);
    out.push_str(&to[common..].join("/"));
    out
}

/// Recorre las etiquetas `<a ... href="...">` del HTML y reescribe su
/// `href` según el visitante. Cualquier otra parte se copia intacta.
pub(super) fn rewrite_hrefs<F>(html: &str, mut visit: F) -> String
where
    F: FnMut(&str) -> Rewrite,
{
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len() + html.len() / 16);
    let mut copied = 0;
    let mut i = 0;
    while let Some(off) = html[i..].find("<a") {
        let start = i + off;
        let after = start + 2;
        i = after;
        if !bytes.get(after).is_some_and(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Some(tag_end) = tag_end(bytes, after) else {
            break;
        };
        if let Some((vs, ve)) = find_href(html, after, tag_end) {
            if let Rewrite::Replace(new) = visit(&unescape(&html[vs..ve])) {
                out.push_str(&html[copied..vs]);
                out.push_str(&escape(&new));
                copied = ve;
            }
        }
        i = tag_end;
    }
    out.push_str(&html[copied..]);
    out
}

/// Índice del `>` que cierra la etiqueta, respetando comillas.
fn tag_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (k, &b) in bytes.iter().enumerate().skip(from) {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(k),
                _ => {}
            },
        }
    }
    None
}

/// Rango del valor de `href="..."` dentro de `html[from..to]`.
fn find_href(html: &str, from: usize, to: usize) -> Option<(usize, usize)> {
    let bytes = html.as_bytes();
    let mut k = from;
    while k < to {
        // Salta espacios y lee el nombre del atributo.
        while k < to && bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        let name_start = k;
        while k < to && !bytes[k].is_ascii_whitespace() && bytes[k] != b'=' && bytes[k] != b'/' {
            k += 1;
        }
        let name = &html[name_start..k];
        while k < to && bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        if k < to && bytes[k] == b'=' {
            k += 1;
            while k < to && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            if k >= to {
                return None;
            }
            let (vs, ve) = if bytes[k] == b'"' || bytes[k] == b'\'' {
                let q = bytes[k];
                let vs = k + 1;
                let mut e = vs;
                while e < to && bytes[e] != q {
                    e += 1;
                }
                k = e + 1;
                (vs, e)
            } else {
                let vs = k;
                while k < to && !bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                (vs, k)
            };
            if name.eq_ignore_ascii_case("href") {
                return Some((vs, ve.min(to)));
            }
        } else if k == name_start {
            k += 1; // evita bucles con basura (`/`, `=` sueltos).
        }
    }
    None
}

/// `id="..."` y `<a name="...">` presentes en el HTML (aproximado a
/// propósito: un falso positivo solo evita una ancla sintética).
pub(super) fn collect_ids(html: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    for needle in [" id=\"", " name=\"", " id='", " name='"] {
        let quote = needle.as_bytes()[needle.len() - 1] as char;
        let mut pos = 0;
        while let Some(off) = html[pos..].find(needle) {
            let s = pos + off + needle.len();
            match html[s..].find(quote) {
                Some(e) => {
                    ids.insert(unescape(&html[s..s + e]));
                    pos = s + e + 1;
                }
                None => break,
            }
        }
    }
    ids
}

/// Normaliza un ancla para compararla con tolerancia: decodifica `%XX`,
/// pasa a minúsculas y reduce todo lo no alfanumérico a un único `-`
/// (`polars.DataFrame.cast` y `polars-dataframe-cast` coinciden).
pub(super) fn norm_anchor(s: &str) -> String {
    slug_of(&percent_decode(s))
}

/// Anclas de una página con búsqueda tolerante.
#[derive(Debug, Default)]
pub(super) struct AnchorIndex {
    exact: HashSet<String>,
    /// Normalizado → ancla real (solo si es única).
    norm: HashMap<String, String>,
    /// Normalizado sin sufijo `-N` → ancla real (solo si es única).
    loose: HashMap<String, String>,
    /// `(normalizado, ancla real)` ordenado, para coincidencias por prefijo.
    sorted: Vec<(String, String)>,
}

impl AnchorIndex {
    pub(super) fn new(ids: HashSet<String>) -> Self {
        let sorted: BTreeSet<&String> = ids.iter().collect();
        let mut norm: HashMap<String, Option<String>> = HashMap::new();
        let mut loose: HashMap<String, Option<String>> = HashMap::new();
        for id in sorted {
            let n = norm_anchor(id);
            if n.is_empty() {
                continue;
            }
            let l = strip_numeric_suffix(&n).to_string();
            for (map, key) in [(&mut norm, n), (&mut loose, l)] {
                map.entry(key)
                    .and_modify(|v| {
                        if v.as_deref() != Some(id.as_str()) {
                            *v = None; // ambigua: no se usa.
                        }
                    })
                    .or_insert_with(|| Some(id.clone()));
            }
        }
        let mut sorted: Vec<(String, String)> = ids
            .iter()
            .map(|id| (norm_anchor(id), id.clone()))
            .filter(|(n, _)| !n.is_empty())
            .collect();
        sorted.sort();
        let keep = |m: HashMap<String, Option<String>>| -> HashMap<String, String> {
            m.into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v)))
                .collect()
        };
        Self {
            exact: ids,
            norm: keep(norm),
            loose: keep(loose),
            sorted,
        }
    }

    /// Ancla real que corresponde a `frag`: exacta, decodificada o, si no
    /// hay otra opción, la única equivalente normalizada.
    pub(super) fn resolve(&self, frag: &str) -> Option<String> {
        if self.exact.contains(frag) {
            return Some(frag.to_string());
        }
        let decoded = percent_decode(frag);
        if self.exact.contains(&decoded) {
            return Some(decoded);
        }
        let n = norm_anchor(frag);
        if n.is_empty() {
            return None;
        }
        self.norm
            .get(&n)
            .or_else(|| self.loose.get(strip_numeric_suffix(&n)))
            .cloned()
            .or_else(|| self.unique_prefix(&n))
    }

    /// Única ancla cuyo texto normalizado empieza por `n-` (títulos que
    /// DevDocs alarga: `ANY` → `ANY---Any-value-in-MASK`). Con dos o más
    /// candidatas no se adivina.
    fn unique_prefix(&self, n: &str) -> Option<String> {
        if n.len() < 3 {
            return None;
        }
        let prefix = format!("{n}-");
        let start = self
            .sorted
            .partition_point(|(k, _)| k.as_str() < prefix.as_str());
        let mut hits = self.sorted[start..]
            .iter()
            .take_while(|(k, _)| k.starts_with(&prefix));
        let first = hits.next()?;
        hits.next().is_none().then(|| first.1.clone())
    }

    pub(super) fn contains(&self, frag: &str) -> bool {
        self.exact.contains(frag)
    }
}

/// `introduction-2` → `introduction` (solo sufijos numéricos de duplicados).
fn strip_numeric_suffix(n: &str) -> &str {
    match n.rsplit_once('-') {
        Some((head, tail))
            if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => n,
    }
}

/// Contenido textual del primer `<h1>` (sin etiquetas ni backticks).
pub(super) fn h1_text(html: &str) -> Option<String> {
    let start = html.find("<h1")?;
    let open_end = start + html[start..].find('>')? + 1;
    let close = open_end + html[open_end..].find("</h1>")?;
    let mut text = String::new();
    let mut in_tag = false;
    for c in html[open_end..close].chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = unescape(&text).replace('`', "");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// Quita los backticks literales dentro del primer `<h1>` (DevDocs los
/// deja en los títulos de CSS/JS).
pub(super) fn strip_h1_backticks(html: &str) -> String {
    let Some(start) = html.find("<h1") else {
        return html.to_string();
    };
    let Some(open) = html[start..].find('>') else {
        return html.to_string();
    };
    let open_end = start + open + 1;
    let Some(close) = html[open_end..].find("</h1>") else {
        return html.to_string();
    };
    let close = open_end + close;
    let mut out = String::with_capacity(html.len());
    out.push_str(&html[..open_end]);
    out.push_str(&html[open_end..close].replace('`', ""));
    out.push_str(&html[close..]);
    out
}

/// Slug comparable de un título (`Argparse Tutorial` → `argparse-tutorial`).
pub(super) fn slug_of(text: &str) -> String {
    let mut slug = String::new();
    let mut dash = true;
    for c in text.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_detection() {
        assert!(is_external("https://x.dev/a"));
        assert!(is_external("mailto:a@b"));
        assert!(is_external("//cdn.x/y"));
        assert!(is_external("data:text/plain,hi"));
        assert!(!is_external("properties/grid"));
        assert!(!is_external("../a:b"));
        assert!(!is_external("#frag"));
        assert!(!is_external(""));
    }

    #[test]
    fn keys_resolve_like_urls_without_extension() {
        assert_eq!(
            resolve_key("properties/grid", "grid-area"),
            "properties/grid-area"
        );
        assert_eq!(
            resolve_key("properties/grid", "../values/box"),
            "values/box"
        );
        assert_eq!(resolve_key("index", "properties"), "properties");
        assert_eq!(resolve_key("a/b", "./c/"), "a/c");
        assert_eq!(resolve_key("a/b", "/x/y"), "x/y");
        assert_eq!(resolve_key("a/b", "../../../../z"), "z");
        assert_eq!(resolve_key("a/b", ""), "a/b");
        assert_eq!(resolve_key("a/b", "%40media"), "a/@media");
    }

    #[test]
    fn relative_paths_between_files() {
        assert_eq!(relative_file("a/b.html", "a/c.html"), "c.html");
        assert_eq!(relative_file("a/b.html", "c/d.html"), "../c/d.html");
        assert_eq!(relative_file("index.html", "a/b.html"), "a/b.html");
        assert_eq!(relative_file("a/b/c.html", "a/d.html"), "../d.html");
    }

    #[test]
    fn href_rewrite_only_touches_anchor_hrefs() {
        let html = r##"<p><a class="x" href="grid?q=1#top">g</a> <area href="grid"> <a href='b'>b</a> <a name="n">n</a><abbr title="t">x</abbr> <a href="a&amp;b">c</a></p>"##;
        let mut seen = Vec::new();
        let out = rewrite_hrefs(html, |h| {
            seen.push(h.to_string());
            Rewrite::Replace(format!("N:{h}"))
        });
        assert_eq!(seen, ["grid?q=1#top", "b", "a&b"]);
        assert!(out.contains(r#"href="N:grid?q=1#top""#));
        assert!(out.contains("href='N:b'") || out.contains(r#"href='N:b'"#));
        assert!(out.contains(r#"<area href="grid">"#));
        assert!(out.contains(r#"href="N:a&amp;b""#));
        assert!(out.contains(r#"<abbr title="t">x</abbr>"#));
    }

    #[test]
    fn keep_returns_identical_html() {
        let html = r#"<a href="x">y</a> trailing <a href="unterminated"#;
        assert_eq!(rewrite_hrefs(html, |_| Rewrite::Keep), html);
    }

    #[test]
    fn ids_and_titles() {
        let ids = collect_ids(r#"<h2 id="a">x</h2><a name="b"></a><span id='c'></span>"#);
        assert!(ids.contains("a") && ids.contains("b") && ids.contains("c"));
        assert_eq!(
            h1_text("<h1>`flex-wrap` CSS <em>property</em></h1>").as_deref(),
            Some("flex-wrap CSS property")
        );
        assert_eq!(
            strip_h1_backticks("<h1>`a`</h1><p>`b`</p>"),
            "<h1>a</h1><p>`b`</p>"
        );
        assert_eq!(slug_of("Argparse Tutorial!"), "argparse-tutorial");
    }

    #[test]
    fn anchors_resolve_tolerantly() {
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        let a = AnchorIndex::new(set(&[
            "polars-dataframe-cast",
            "Introduction-2",
            "parse-java.lang.String-",
            "dup-1",
            "dup-2",
        ]));
        assert_eq!(
            a.resolve("polars-dataframe-cast").as_deref(),
            Some("polars-dataframe-cast")
        );
        assert_eq!(
            a.resolve("polars.DataFrame.cast").as_deref(),
            Some("polars-dataframe-cast")
        );
        assert_eq!(a.resolve("Introduction").as_deref(), Some("Introduction-2"));
        assert_eq!(
            a.resolve("parse(java.lang.String)").as_deref(),
            Some("parse-java.lang.String-")
        );
        assert_eq!(a.resolve("dup"), None, "ambiguous must not guess");
        assert_eq!(a.resolve("nothing"), None);
        let p = AnchorIndex::new(set(&[
            "ANY-_002d-Any-value",
            "index-array",
            "COUNT-x",
            "COUNT-y",
        ]));
        assert_eq!(p.resolve("ANY").as_deref(), Some("ANY-_002d-Any-value"));
        assert_eq!(p.resolve("COUNT"), None, "two candidates: no guessing");
        assert_eq!(p.resolve("AN"), None);
        assert_eq!(a.resolve("---"), None);
    }

    #[test]
    fn percent_helpers_roundtrip() {
        assert_eq!(percent_decode("%40media%2Fx"), "@media/x");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(encode_path("a b/@c#d"), "a%20b/@c%23d");
    }
}
