//! Búsqueda sobre el índice en memoria (T5).
//!
//! - Función pura y síncrona: no toca Tauri ni bloquea su hilo (T6 la
//!   llamará tras `Mutex`/`spawn_blocking`).
//! - Query vacía → vacío (la paleta mostrará recientes/favoritos en v0.2).
//! - Clave de orden: `(tier, kind_rank, score↓, name, docset_id)`.
//!   Tiers: 0 exacta, 1 prefijo, 2 difusa (insensibles a mayúsculas,
//!   sobre el nombre completo o su último segmento). El tipo ordena
//!   antes que el score, como en Zeal/Dash.
//! - Multi-palabra con AND: todas las palabras deben coincidir; el tier
//!   es el peor de ellas y el score la suma.
//! - `Pattern::new` (no `parse`) + partición manual: caracteres de API
//!   (`C++`, `!important`) nunca se leen como sintaxis de patrón.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use serde::{Deserialize, Serialize};

use super::index::{IndexedEntry, SearchIndex};

type RankedHit<'a> = (u8, u8, Reverse<u32>, &'a str, &'a str, usize);

/// Un resultado listo para la UI (serializable para T6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// Slug del docset.
    pub docset_id: String,
    /// Nombre de la entrada.
    pub name: String,
    /// Tipo normalizado.
    pub kind: String,
    /// Ruta relativa a `Documents/`.
    pub path: String,
    /// Score difuso de nucleo (mayor = mejor).
    pub score: u32,
}

/// Busca en el índice. `docset_ids` filtra por docset; `limit` trunca.
/// Nunca falla: sin coincidencias devuelve vacío.
pub fn search(
    index: &mut SearchIndex,
    query: &str,
    docset_ids: Option<&[String]>,
    limit: usize,
) -> Vec<SearchResult> {
    let query = query.trim();
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let patterns: Vec<Pattern> = query
        .split_whitespace()
        .map(|w| {
            Pattern::new(
                w,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
            )
        })
        .collect();
    let allowed: Option<HashSet<&str>> =
        docset_ids.map(|ids| ids.iter().map(String::as_str).collect());

    let SearchIndex {
        entries,
        matcher,
        utf32_buf,
        ..
    } = &mut *index;
    // La paleta solo enseña 20 filas: conserva el mejor top-K durante el
    // recorrido y evita ordenar/guardar todos los hits de docsets grandes.
    let mut hits: BinaryHeap<RankedHit<'_>> = BinaryHeap::with_capacity(limit);
    for (idx, entry) in entries.iter().enumerate() {
        if let Some(allowed) = &allowed {
            if !allowed.contains(entry.docset_id.as_ref()) {
                continue;
            }
        }
        if words.iter().any(|word| !could_match_literals(entry, word)) {
            continue;
        }
        if let Some((tier, score)) = match_entry(matcher, utf32_buf, entry, &words, &patterns) {
            let candidate = (
                tier,
                kind_rank(&entry.kind),
                Reverse(score),
                entry.name.as_str(),
                entry.docset_id.as_ref(),
                idx,
            );
            if hits.len() < limit {
                hits.push(candidate);
            } else if hits.peek().is_some_and(|worst| candidate < *worst) {
                hits.pop();
                hits.push(candidate);
            }
        }
    }
    let mut hits = hits.into_vec();
    hits.sort_unstable();
    hits.into_iter()
        .map(|(_, _, Reverse(score), _, _, idx)| {
            let entry = &entries[idx];
            SearchResult {
                docset_id: entry.docset_id.to_string(),
                name: entry.name.clone(),
                kind: entry.kind.to_string(),
                path: entry.path.clone(),
                score,
            }
        })
        .collect()
}

/// Descarta candidatos que carecen de dígitos/signos ASCII obligatorios.
/// Las letras se dejan al matcher: su normalización permite, por ejemplo,
/// que `i` coincida con `ï`, así que filtrarlas literalmente perdería hits.
fn could_match_literals(entry: &IndexedEntry, word: &str) -> bool {
    let literals = word
        .chars()
        .filter(|c| c.is_ascii_digit() || (c.is_ascii_punctuation() && !c.is_ascii_whitespace()));
    let mut full_ok = true;
    let mut segment_ok = true;
    for literal in literals {
        full_ok &= entry.lower_name.contains(literal);
        segment_ok &= entry
            .lower_segment
            .as_deref()
            .is_some_and(|segment| segment.contains(literal));
        if !full_ok && !segment_ok {
            return false;
        }
    }
    true
}

/// Tier y score de una entrada. `None` si alguna palabra no coincide.
fn match_entry(
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
    entry: &IndexedEntry,
    words: &[String],
    patterns: &[Pattern],
) -> Option<(u8, u32)> {
    let full = entry.name.as_str();
    let segment = last_segment(full);
    let mut worst_tier = 0_u8;
    let mut total = 0_u32;
    for (word, pattern) in words.iter().zip(patterns.iter()) {
        let (tier, score) = match_word(
            matcher,
            buf,
            word,
            pattern,
            (full, &entry.lower_name),
            segment.zip(entry.lower_segment.as_deref()),
        )?;
        worst_tier = worst_tier.max(tier);
        total = total.saturating_add(score);
    }
    Some((worst_tier, total))
}

/// Mejor (tier, score) entre nombre completo y último segmento.
fn match_word(
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
    word: &str,
    pattern: &Pattern,
    full: (&str, &str),
    segment: Option<(&str, &str)>,
) -> Option<(u8, u32)> {
    let mut best = tiered(matcher, buf, word, pattern, full.0, full.1, true);
    if let Some((seg, lower_seg)) = segment {
        // El tier por segmento exige query de 2+ caracteres para no
        // inundar con queries de una letra.
        let candidate = tiered(
            matcher,
            buf,
            word,
            pattern,
            seg,
            lower_seg,
            word.chars().count() >= 2,
        );
        best = match (best, candidate) {
            (Some(a), Some(b)) => Some(if (a.0, u32::MAX - a.1) <= (b.0, u32::MAX - b.1) {
                a
            } else {
                b
            }),
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
    }
    best
}

/// Tier de un candidato + score difuso (para sub-ordenar dentro del tier).
/// Sin `allow_tier01` solo opta a tier difuso.
fn tiered(
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
    word_lower: &str,
    pattern: &Pattern,
    candidate: &str,
    lower: &str,
    allow_tier01: bool,
) -> Option<(u8, u32)> {
    let score = pattern.score(Utf32Str::new(candidate, buf), matcher)?;
    if !allow_tier01 {
        return Some((2, score));
    }
    if lower == word_lower {
        Some((0, score))
    } else if lower.starts_with(word_lower) {
        Some((1, score))
    } else {
        Some((2, score))
    }
}

/// Último segmento tras separadores de API (`. # : /` y espacios).
/// `None` si no hay segmentación útil.
pub(crate) fn last_segment(name: &str) -> Option<&str> {
    let seg = name
        .rsplit(['.', '#', ':', '/', ' '])
        .next()
        .unwrap_or(name);
    if seg.is_empty() || seg == name {
        None
    } else {
        Some(seg)
    }
}

/// Prioridad por tipo: API (0-2) por encima de guías y resto (3).
fn kind_rank(kind: &str) -> u8 {
    match kind.to_lowercase().as_str() {
        "class" | "struct" | "union" | "enum" | "protocol" | "interface" | "trait" | "category" => {
            0
        }
        "function" | "method" | "constructor" | "destructor" | "operator" | "macro" => 1,
        "property" | "constant" | "variable" => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::super::index::entry;
    use super::super::index::SearchIndex;
    use super::*;

    fn index_of(entries: Vec<crate::docset::Entry>) -> SearchIndex {
        SearchIndex::build(entries)
    }

    #[test]
    fn exact_beats_prefix_beats_fuzzy() {
        let mut index = index_of(vec![
            entry("d", "xgrido", "Guide"),        // difusa
            entry("d", "grid-template", "Guide"), // prefijo
            entry("d", "grid", "Property"),       // exacta
        ]);
        let names: Vec<String> = search(&mut index, "grid", None, 10)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["grid", "grid-template", "xgrido"]);
    }

    #[test]
    fn kind_boost_within_tier() {
        let mut index = index_of(vec![
            entry("d", "thing", "Guide"),
            entry("d", "thing", "Class"),
            entry("d", "thing", "Function"),
        ]);
        let kinds: Vec<String> = search(&mut index, "thing", None, 10)
            .into_iter()
            .map(|r| r.kind)
            .collect();
        assert_eq!(kinds, vec!["Class", "Function", "Guide"]);
    }

    #[test]
    fn filters_and_limit() {
        let mut index = index_of(vec![
            entry("a", "alpha", "Guide"),
            entry("b", "alpha", "Guide"),
            entry("a", "alphabet", "Guide"),
        ]);
        let all = search(&mut index, "alpha", None, 10);
        assert_eq!(all.len(), 3);
        let filtered = search(&mut index, "alpha", Some(&["b".to_string()]), 10);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].docset_id, "b");
        let limited = search(&mut index, "alpha", None, 2);
        assert_eq!(limited.len(), 2);
        assert!(search(&mut index, "alpha", None, 0).is_empty());
    }

    #[test]
    fn ties_break_by_name_then_docset() {
        let mut index = index_of(vec![
            entry("b", "same", "Guide"),
            entry("a", "same", "Guide"),
        ]);
        let res = search(&mut index, "same", None, 10);
        assert_eq!(res[0].docset_id, "a");
        assert_eq!(res[1].docset_id, "b");
        // Misma query dos veces → mismo orden.
        let again = search(&mut index, "same", None, 10);
        assert_eq!(res, again);
    }

    #[test]
    fn last_segment_matches() {
        let mut index = index_of(vec![
            entry("cpp", "std::vector", "Class"),
            entry("py", "str.format", "Method"),
            entry("css", "unrelated-thing", "Property"),
        ]);
        let v = search(&mut index, "vector", None, 10);
        assert_eq!(v[0].name, "std::vector");
        let f = search(&mut index, "format", None, 10);
        assert_eq!(f[0].name, "str.format");
    }

    #[test]
    fn empty_query_returns_empty() {
        let mut index = index_of(vec![entry("d", "grid", "Property")]);
        assert!(search(&mut index, "", None, 10).is_empty());
        assert!(search(&mut index, "   ", None, 10).is_empty());
    }

    #[test]
    fn multiword_requires_all_words() {
        let mut index = index_of(vec![
            entry("d", "border-radius", "Property"),
            entry("d", "border-color", "Property"),
        ]);
        let res: Vec<String> = search(&mut index, "border radius", None, 10)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(res, vec!["border-radius"]);
    }

    #[test]
    fn real_css_cases() {
        let Some(dsidx) =
            crate::docset::fixture_or_skip("CSS.docset/Contents/Resources/docSet.dsidx")
        else {
            return;
        };
        let data = crate::docset::read_index(&dsidx, "css").expect("leer índice CSS");
        let mut index = SearchIndex::build(data.entries);
        // Exacta primero, insensible a mayúsculas.
        let grid: Vec<String> = search(&mut index, "grid", None, 5)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(grid[0], "grid");
        let upper: Vec<String> = search(&mut index, "GRID", None, 5)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(grid, upper);
        // "radius" encuentra las propiedades *-radius.
        let radius: Vec<String> = search(&mut index, "radius", None, 5)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert!(radius.iter().all(|n| n.contains("radius")));
        assert!(radius.contains(&"border-radius".to_string()));
        // Boost por tipo: "color" exacto Function antes que Property.
        let color = search(&mut index, "color", None, 5);
        assert_eq!(color[0].name, "color");
        assert_eq!(color[0].kind, "Function");
    }

    #[test]
    fn real_search_is_fast() {
        let Some(dsidx) =
            crate::docset::fixture_or_skip("CSS.docset/Contents/Resources/docSet.dsidx")
        else {
            return;
        };
        let data = crate::docset::read_index(&dsidx, "css").expect("leer índice CSS");
        let start = std::time::Instant::now();
        let mut index = SearchIndex::build(data.entries);
        let build = start.elapsed();
        let mut worst = std::time::Duration::ZERO;
        for q in ["grid", "radius", "color", "border"] {
            let start = std::time::Instant::now();
            let res = search(&mut index, q, None, 20);
            let elapsed = start.elapsed();
            assert!(!res.is_empty(), "sin resultados para {q}");
            worst = worst.max(elapsed);
        }
        eprintln!("css: build={build:?} peor búsqueda={worst:?}");
        assert!(build.as_secs() < 5, "build lento: {build:?}");
        assert!(worst.as_secs() < 1, "búsqueda lenta: {worst:?}");
    }

    /// Escala: medido en release build=17.9ms búsqueda=37.4ms (< 50ms).
    /// En debug con [profile.dev.package."*"] opt-level=3: build~130ms,
    /// búsqueda~340ms a 300k (los asserts son cotas debug amplias).
    #[test]
    fn large_synthetic_index_scales() {
        let kinds = ["Class", "Function", "Method", "Property", "Guide"];
        let mut raw = Vec::with_capacity(300_000);
        for i in 0..300_000_u32 {
            raw.push(entry(
                &format!("ds{}", i % 7),
                &format!("mod{}.item_{i:06}", i % 500),
                kinds[(i % 5) as usize],
            ));
        }
        let start = std::time::Instant::now();
        let mut index = SearchIndex::build(raw);
        let build = start.elapsed();
        assert_eq!(index.len(), 300_000);
        let start = std::time::Instant::now();
        let res = search(&mut index, "item_123", None, 20);
        let elapsed = start.elapsed();
        eprintln!("300k: build={build:?} búsqueda={elapsed:?}");
        assert!(!res.is_empty());
        assert_eq!(res.len(), 20);
        assert!(build.as_secs() < 20, "build lento: {build:?}");
        assert!(elapsed.as_secs() < 2, "búsqueda lenta: {elapsed:?}");
    }

    #[test]
    fn case_and_unicode_tolerant() {
        let mut index = index_of(vec![
            entry("d", "Café", "Guide"),
            entry("d", "naïve-case", "Function"),
            entry("d", "other", "Guide"),
        ]);
        let names: Vec<String> = search(&mut index, "CAFÉ", None, 10)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names[0], "Café");
        let naive: Vec<String> = search(&mut index, "naive", None, 10)
            .into_iter()
            .map(|r| r.name)
            .collect();
        // Normalization::Smart pliega diacríticos (naive → naïve-case).
        assert_eq!(naive[0], "naïve-case");
    }
}
