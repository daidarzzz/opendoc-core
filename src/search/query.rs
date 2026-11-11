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
//! - Incremental: si la consulta amplía la anterior con el mismo filtro,
//!   solo se revisan las entradas que ya coincidieron (más letras o más
//!   palabras nunca añaden coincidencias). Con muchos candidatos se
//!   reparte entre hilos; la clave de orden incluye el índice, así que el
//!   resultado es idéntico al recorrido completo en serie (ver tests
//!   `*_match_full_scan`).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use serde::{Deserialize, Serialize};

use super::index::{new_scratch, IndexedEntry, LastQuery, SearchIndex};

/// Clave de orden de un hit: (tier, tipo, score↓, nombre, docset, índice).
/// El índice la hace total: el resultado no depende de cuántos hilos
/// busquen ni en qué orden terminen.
type RankedHit<'a> = (u8, u8, Reverse<u32>, &'a str, &'a str, usize);

/// A partir de cuántos candidatos se reparte la búsqueda entre hilos
/// (por debajo, crear hilos cuesta más de lo que ahorra).
const PARALLEL_MIN: usize = 40_000;
/// Tope de hilos de búsqueda.
const MAX_THREADS: usize = 8;

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

/// Qué entradas hay que mirar.
#[derive(Clone, Copy)]
enum Candidates<'a> {
    /// Todas, en `[inicio, fin)` (con el filtro de docsets aplicado).
    Range(usize, usize),
    /// Solo estas (las que coincidieron con la consulta anterior).
    List(&'a [u32]),
}

impl Candidates<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Range(start, end) => end - start,
            Self::List(list) => list.len(),
        }
    }

    /// Parte en `n` trozos contiguos (el orden se conserva).
    fn split(self, n: usize) -> Vec<Self> {
        let len = self.len();
        let chunk = len.div_ceil(n.max(1)).max(1);
        (0..len)
            .step_by(chunk)
            .map(|from| {
                let to = (from + chunk).min(len);
                match self {
                    Self::Range(start, _) => Self::Range(start + from, start + to),
                    Self::List(list) => Self::List(&list[from..to]),
                }
            })
            .collect()
    }
}

/// Consulta ya preparada (compartida entre hilos).
struct Prepared<'q> {
    words: &'q [String],
    patterns: &'q [Pattern],
    allowed: Option<&'q HashSet<&'q str>>,
    limit: usize,
}

/// Resultado de recorrer un trozo: mejores `limit` + todos los que
/// coincidieron (para la siguiente tecla).
struct Scan<'a> {
    top: BinaryHeap<RankedHit<'a>>,
    hits: Vec<u32>,
}

/// Busca en el índice. `docset_ids` filtra por docset; `limit` trunca.
/// Nunca falla: sin coincidencias devuelve vacío.
///
/// Rápida al escribir: si la consulta amplía la anterior (`bo` → `bor`,
/// `str` → `str format`) con el mismo filtro, solo se miran las entradas
/// que ya coincidieron (una coincidencia difusa con más letras implica la
/// de menos). Con muchos candidatos, el recorrido se reparte entre hilos.
/// El resultado es idéntico al de recorrer todo el índice.
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
    let prepared = Prepared {
        words: &words,
        patterns: &patterns,
        allowed: allowed.as_ref(),
        limit,
    };

    let SearchIndex {
        entries,
        matchers,
        last,
        ..
    } = &mut *index;
    // La consulta anterior sirve si esta la amplía con el mismo filtro.
    let previous = last.take().filter(|prev| {
        query.starts_with(prev.query.as_str()) && prev.docset_ids.as_deref() == docset_ids
    });
    let candidates = match &previous {
        Some(prev) => Candidates::List(&prev.hits),
        None => Candidates::Range(0, entries.len()),
    };

    let threads = if candidates.len() >= PARALLEL_MIN {
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .clamp(1, MAX_THREADS)
    } else {
        1
    };
    if matchers.len() < threads {
        matchers.resize_with(threads, new_scratch);
    }

    let entries: &[IndexedEntry] = entries;
    let scans: Vec<Scan<'_>> = if threads == 1 {
        let (matcher, buf) = &mut matchers[0];
        vec![scan(entries, candidates, &prepared, matcher, buf)]
    } else {
        let chunks = candidates.split(threads);
        let prepared = &prepared;
        std::thread::scope(|s| {
            let handles: Vec<_> = chunks
                .into_iter()
                .zip(matchers.iter_mut())
                .map(|(chunk, (matcher, buf))| {
                    s.spawn(move || scan(entries, chunk, prepared, matcher, buf))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| match h.join() {
                    Ok(scan) => scan,
                    // Un pánico en un hilo se propaga igual que en serie.
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect()
        })
    };

    // Une los trozos: los hits quedan en orden de índice (trozos contiguos
    // y en orden) y el top se recalcula sobre la unión de los tops.
    let mut hits = Vec::with_capacity(scans.iter().map(|s| s.hits.len()).sum());
    let mut top: Vec<RankedHit<'_>> = Vec::with_capacity(limit * scans.len());
    for scan in scans {
        hits.extend(scan.hits);
        top.extend(scan.top.into_vec());
    }
    top.sort_unstable();
    top.truncate(limit);
    let results = top
        .into_iter()
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
        .collect();
    *last = Some(LastQuery {
        query: query.to_string(),
        docset_ids: docset_ids.map(<[String]>::to_vec),
        hits,
    });
    results
}

/// Recorre un trozo de candidatos con su propio scratch de nucleo.
fn scan<'a>(
    entries: &'a [IndexedEntry],
    candidates: Candidates<'_>,
    q: &Prepared<'_>,
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
) -> Scan<'a> {
    // La paleta solo enseña 20 filas: conserva el mejor top-K durante el
    // recorrido y evita ordenar/guardar todos los hits de docsets grandes.
    let mut top: BinaryHeap<RankedHit<'a>> = BinaryHeap::with_capacity(q.limit + 1);
    let mut hits = Vec::new();
    let mut visit = |idx: usize, entry: &'a IndexedEntry| {
        if q.words
            .iter()
            .any(|word| !could_match_literals(entry, word))
        {
            return;
        }
        let Some((tier, score)) = match_entry(matcher, buf, entry, q.words, q.patterns) else {
            return;
        };
        // Los índices caben en u32 (un índice con más de 4.000 millones de
        // entradas no cabría en memoria de todas formas).
        hits.push(idx as u32);
        let candidate = (
            tier,
            entry.kind_rank,
            Reverse(score),
            entry.name.as_str(),
            entry.docset_id.as_ref(),
            idx,
        );
        if top.len() < q.limit {
            top.push(candidate);
        } else if top.peek().is_some_and(|worst| candidate < *worst) {
            top.pop();
            top.push(candidate);
        }
    };
    match candidates {
        Candidates::Range(start, end) => {
            for (idx, entry) in entries.iter().enumerate().take(end).skip(start) {
                if let Some(allowed) = q.allowed {
                    if !allowed.contains(entry.docset_id.as_ref()) {
                        continue;
                    }
                }
                visit(idx, entry);
            }
        }
        // Ya filtrados por docset en la consulta anterior (mismo filtro).
        Candidates::List(list) => {
            for &idx in list {
                visit(idx as usize, &entries[idx as usize]);
            }
        }
    }
    Scan { top, hits }
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
/// Sin asignar memoria (se calcula para cada entrada al indexar): la
/// comparación ASCII sin mayúsculas equivale aquí a `to_lowercase()`,
/// porque ninguna de estas palabras tiene letras cuyo plegado Unicode
/// salga de un carácter no ASCII.
pub(crate) fn kind_rank(kind: &str) -> u8 {
    const RANKS: &[(&str, u8)] = &[
        ("class", 0),
        ("struct", 0),
        ("union", 0),
        ("enum", 0),
        ("protocol", 0),
        ("interface", 0),
        ("trait", 0),
        ("category", 0),
        ("function", 1),
        ("method", 1),
        ("constructor", 1),
        ("destructor", 1),
        ("operator", 1),
        ("macro", 1),
        ("property", 2),
        ("constant", 2),
        ("variable", 2),
    ];
    RANKS
        .iter()
        .find(|(word, _)| kind.eq_ignore_ascii_case(word))
        .map_or(3, |&(_, rank)| rank)
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

    /// Escala: 300k entradas sintéticas. Los asserts son cotas amplias
    /// (también valen en debug); las cifras reales en release están en el
    /// README (sección Rendimiento) y salen del benchmark externo.
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

    // ── Equivalencia con el algoritmo anterior ──────────────────────────
    //
    // La búsqueda incremental y en paralelo debe devolver EXACTAMENTE lo
    // mismo (orden y scores) que recorrer todo el índice en serie, como
    // hacía la versión 0.1.1. `reference` es ese algoritmo, sin caché ni
    // hilos ni tipo precalculado.

    fn reference(
        index: &SearchIndex,
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
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let mut buf = Vec::new();
        let mut hits = Vec::new();
        for (idx, entry) in index.entries.iter().enumerate() {
            if let Some(ids) = docset_ids {
                if !ids.iter().any(|id| id.as_str() == entry.docset_id.as_ref()) {
                    continue;
                }
            }
            if words.iter().any(|w| !could_match_literals(entry, w)) {
                continue;
            }
            if let Some((tier, score)) =
                match_entry(&mut matcher, &mut buf, entry, &words, &patterns)
            {
                hits.push((
                    tier,
                    kind_rank(&entry.kind),
                    Reverse(score),
                    entry.name.clone(),
                    entry.docset_id.to_string(),
                    idx,
                ));
            }
        }
        hits.sort();
        hits.truncate(limit);
        hits.into_iter()
            .map(|(_, _, Reverse(score), _, _, idx)| {
                let e = &index.entries[idx];
                SearchResult {
                    docset_id: e.docset_id.to_string(),
                    name: e.name.clone(),
                    kind: e.kind.to_string(),
                    path: e.path.clone(),
                    score,
                }
            })
            .collect()
    }

    /// Escribe `text` letra a letra (y luego lo borra) comparando cada paso.
    fn type_and_compare(
        index: &mut SearchIndex,
        text: &str,
        filter: Option<&[String]>,
        limit: usize,
    ) {
        let steps: Vec<&str> = text
            .char_indices()
            .map(|(i, c)| &text[..i + c.len_utf8()])
            .collect();
        for q in steps.iter().chain(steps.iter().rev()) {
            let got = search(index, q, filter, limit);
            let want = reference(index, q, filter, limit);
            assert_eq!(got, want, "consulta {q:?} filtro {filter:?}");
        }
    }

    /// Índice sintético grande y variado: separadores de API, mayúsculas,
    /// dígitos, signos y Unicode; por encima de `PARALLEL_MIN` para que la
    /// búsqueda vaya en paralelo.
    fn varied_index(n: usize) -> SearchIndex {
        let stems = [
            "border-radius",
            "std::vector",
            "str.format",
            "Array#map",
            "grid",
            "naïve",
            "Café",
            "operator<<",
            "!important",
            "C++",
            "push_back",
            "HashMap",
            "border",
            "vectorize",
            "format_args",
            "Grid-Template",
            "radius",
            "i18n",
            "utf8",
            "x264",
        ];
        let kinds = [
            "Class", "Function", "Method", "Property", "Guide", "clm", "Unknown",
        ];
        let mut raw = Vec::with_capacity(n);
        for i in 0..n {
            let stem = stems[i % stems.len()];
            let name = match i % 4 {
                0 => stem.to_string(),
                1 => format!("mod{}.{stem}", i % 97),
                2 => format!("{stem}_{i}"),
                _ => format!("ns{}::{stem}::item{}", i % 13, i % 31),
            };
            raw.push(entry(
                &format!("ds{}", i % 9),
                &name,
                kinds[i % kinds.len()],
            ));
        }
        SearchIndex::build(raw)
    }

    #[test]
    fn incremental_and_parallel_match_full_scan() {
        let mut index = varied_index(PARALLEL_MIN + 5_000);
        let filter = ["ds1".to_string(), "ds4".to_string()];
        for text in [
            "border radius",
            "std::vec",
            "str.format",
            "Array#map",
            "operator<<",
            "!imp",
            "C++",
            "naive",
            "CAFÉ",
            "x264",
            "item1 vec",
            "zzz",
            "b",
            "i18n",
        ] {
            type_and_compare(&mut index, text, None, 20);
            type_and_compare(&mut index, text, Some(&filter), 20);
        }
        // Cambiar el filtro o el límite a mitad no reutiliza mal la caché.
        let a = search(&mut index, "bor", None, 20);
        assert_eq!(a, reference(&index, "bor", None, 20));
        let b = search(&mut index, "bord", Some(&filter), 5);
        assert_eq!(b, reference(&index, "bord", Some(&filter), 5));
        let c = search(&mut index, "borde", None, 50);
        assert_eq!(c, reference(&index, "borde", None, 50));
    }

    #[test]
    fn kind_rank_matches_lowercase_version() {
        let old = |kind: &str| match kind.to_lowercase().as_str() {
            "class" | "struct" | "union" | "enum" | "protocol" | "interface" | "trait"
            | "category" => 0,
            "function" | "method" | "constructor" | "destructor" | "operator" | "macro" => 1,
            "property" | "constant" | "variable" => 2,
            _ => 3,
        };
        for kind in [
            "Class",
            "CLASS",
            "class",
            "Struct",
            "Function",
            "METHOD",
            "Macro",
            "Property",
            "Constant",
            "Variable",
            "Guide",
            "clm",
            "Unknown",
            "",
            "İnterface",
            "Clase",
            "Ünion",
        ] {
            assert_eq!(kind_rank(kind), old(kind), "{kind}");
        }
    }

    #[test]
    fn changing_entries_forgets_the_previous_query() {
        let mut index = varied_index(2_000);
        assert_eq!(
            search(&mut index, "gr", None, 20),
            reference(&index, "gr", None, 20)
        );
        // Llegan entradas nuevas que coinciden con "gri" pero no estaban
        // entre los candidatos de "gr" (no existían).
        index.extend(vec![entry("nuevo", "grilla", "Class")]);
        let hits = search(&mut index, "gri", None, 20);
        assert_eq!(hits, reference(&index, "gri", None, 20));
        assert!(hits.iter().any(|h| h.name == "grilla"));
        // Y al quitar/sustituir docsets también se olvida.
        assert!(!search(&mut index, "gril", None, 20).is_empty());
        index.remove_docset("nuevo");
        assert!(search(&mut index, "grill", None, 20).is_empty());
        index.replace_docset(&[], vec![entry("otro", "grillo", "Guide")]);
        assert_eq!(search(&mut index, "grill", None, 20)[0].name, "grillo");
    }

    #[test]
    fn real_indexes_match_full_scan() {
        let mut all = Vec::new();
        for (path, id) in [
            ("CSS.docset/Contents/Resources/docSet.dsidx", "css"),
            ("C++.docset/Contents/Resources/docSet.dsidx", "cpp"),
            ("Python_3.docset/Contents/Resources/docSet.dsidx", "py"),
            ("Swift.docset/Contents/Resources/docSet.dsidx", "swift"),
        ] {
            if let Some(dsidx) = crate::docset::fixture_or_skip(path) {
                all.extend(
                    crate::docset::read_index(&dsidx, id)
                        .expect("leer índice")
                        .entries,
                );
            }
        }
        if all.is_empty() {
            return;
        }
        // Repetido hasta superar el umbral de paralelo, con ids distintos.
        let mut big = Vec::new();
        let mut copy = 0;
        while big.len() < PARALLEL_MIN * 2 {
            for e in &all {
                let mut e = e.clone();
                e.docset_id = format!("{}{copy}", e.docset_id);
                big.push(e);
            }
            copy += 1;
        }
        let mut index = SearchIndex::build(big);
        let filter = ["css0".to_string(), "cpp1".to_string()];
        for text in [
            "grid",
            "border-radius",
            "vector",
            "push back",
            "str.format",
            "std::map",
            "color",
        ] {
            type_and_compare(&mut index, text, None, 20);
            type_and_compare(&mut index, text, Some(&filter), 20);
        }
    }
}
