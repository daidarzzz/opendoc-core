//! Índice en memoria para la búsqueda interactiva (T5).
//!
//! - `build` consume las `Entry` de los docsets abribles y las deja en un
//!   `Vec` contiguo. El llamador decide qué entra: los docsets tarix con
//!   `MissingDocuments` (p. ej. el C++ de Zeal) no se indexan hasta v1.0.
//! - `docset_id` y `kind` se repiten miles de veces: se internan en
//!   `Arc<str>` vía pool para no duplicar `String` por entrada.
//! - `Matcher` de nucleo (~135 KB de scratch) y el buffer UTF-32 se
//!   reutilizan entre búsquedas; `search` (en `query.rs`) toma
//!   `&mut SearchIndex` por eso, pero es determinista y no toca Tauri
//!   (T6 lo llamará tras `Mutex`/`spawn_blocking`).

use std::collections::HashMap;
use std::sync::Arc;

use nucleo_matcher::{Config, Matcher};

use crate::browse::BrowseCache;
use crate::docset::Entry;

/// Una entrada lista para buscar.
#[derive(Debug, Clone)]
pub struct IndexedEntry {
    /// Nombre de la API (`str.format`, `std::vector`...).
    pub name: String,
    /// Forma plegada precalculada para filtros y ranking en cada tecla.
    pub lower_name: String,
    /// Forma plegada del último segmento de API, si existe.
    pub lower_segment: Option<String>,
    /// Ruta relativa a `Documents/`, con ancla si la hay.
    pub path: String,
    /// Tipo normalizado. Internado: compartido entre entradas.
    pub kind: Arc<str>,
    /// Slug del docset. Internado: compartido entre entradas.
    pub docset_id: Arc<str>,
}

/// Todas las entradas cargadas + scratch reutilizable de nucleo.
pub struct SearchIndex {
    pub(crate) entries: Vec<IndexedEntry>,
    pub(crate) matcher: Matcher,
    pub(crate) utf32_buf: Vec<char>,
    pool: HashMap<String, Arc<str>>,
    /// Navegación por tipos: orden + conteos, reconstruida en cada
    /// `build`/`extend` (ver `browse`).
    pub(crate) browse: BrowseCache,
}

impl SearchIndex {
    /// Índice vacío con matcher y buffers listos.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            matcher: Matcher::new(Config::DEFAULT),
            utf32_buf: Vec::with_capacity(256),
            pool: HashMap::new(),
            browse: BrowseCache::empty(),
        }
    }

    /// Construye el índice desde las entradas de los docsets abribles.
    pub fn build(entries: Vec<Entry>) -> Self {
        // FASE 0: solo medida.
        let t_build = std::time::Instant::now();
        let n = entries.len();
        let mut index = Self::new();
        index.extend(entries);
        index.entries.shrink_to_fit();
        crate::profile::mark(
            "index",
            format_args!(
                "build total_entries={n} ms={}",
                crate::profile::ms_since(t_build)
            ),
        );
        index
    }

    /// Construye el índice solo para buscar: no calcula la navegación por
    /// tipos (`BrowseCache`). El CLI no la usa y ahorra el `O(n log n)`.
    pub fn build_for_search(entries: Vec<Entry>) -> Self {
        let mut index = Self::new();
        index.add_entries(entries);
        index.entries.shrink_to_fit();
        index
    }

    /// Añade entradas al `Vec` interno (sin tocar la navegación).
    fn add_entries(&mut self, entries: Vec<Entry>) {
        let Self {
            entries: mine,
            pool,
            ..
        } = self;
        mine.extend(entries.into_iter().map(|e| IndexedEntry {
            lower_name: e.name.to_lowercase(),
            lower_segment: crate::search::query::last_segment(&e.name).map(str::to_lowercase),
            name: e.name,
            path: e.path,
            kind: intern(pool, &e.kind),
            docset_id: intern(pool, &e.docset_id),
        }));
    }

    /// Añade entradas compartiendo el internado (p. ej. tarix instalados
    /// después de la carga inicial) y reconstruye la navegación.
    pub fn extend(&mut self, entries: Vec<Entry>) {
        // FASE 0: solo medida (incluye el rebuild de navegación).
        let t_ext = std::time::Instant::now();
        let added = entries.len();
        self.add_entries(entries);
        self.browse = BrowseCache::rebuild(&self.entries);
        // FASE 0: solo medida.
        crate::profile::mark(
            "index",
            format_args!(
                "extend added={added} total={} ms={}",
                self.entries.len(),
                crate::profile::ms_since(t_ext)
            ),
        );
    }

    /// Quita las entradas de un docset y reconstruye la navegación
    /// (al sustituirlo por una versión recién instalada).
    pub fn remove_docset(&mut self, docset_id: &str) {
        // FASE 0: solo medida.
        let t_rm = std::time::Instant::now();
        self.entries.retain(|e| e.docset_id.as_ref() != docset_id);
        self.browse = BrowseCache::rebuild(&self.entries);
        crate::profile::mark(
            "index",
            format_args!(
                "remove docset={docset_id} remaining={} ms={}",
                self.entries.len(),
                crate::profile::ms_since(t_rm)
            ),
        );
    }

    /// Sustituye las entradas de varios docsets por unas nuevas con UNA
    /// sola reconstrucción (F1): mismo estado final que `remove_docset`
    /// más `extend` (retain conserva el orden y extend anexa), sin el
    /// rebuild intermedio que nadie observa.
    pub fn replace_docset(&mut self, old_ids: &[String], entries: Vec<Entry>) {
        // FASE 0: solo medida.
        let t_replace = std::time::Instant::now();
        let added = entries.len();
        self.entries
            .retain(|e| !old_ids.iter().any(|id| e.docset_id.as_ref() == id));
        self.add_entries(entries);
        self.browse = BrowseCache::rebuild(&self.entries);
        crate::profile::mark(
            "index",
            format_args!(
                "replace dropped={} added={added} total={} ms={}",
                old_ids.len(),
                self.entries.len(),
                crate::profile::ms_since(t_replace)
            ),
        );
    }

    /// Número de entradas indexadas.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` si no hay entradas.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Devuelve el `Arc<str>` canónico para `s`, creando una sola asignación
/// por valor distinto.
fn intern(pool: &mut HashMap<String, Arc<str>>, s: &str) -> Arc<str> {
    if let Some(shared) = pool.get(s) {
        return shared.clone();
    }
    let shared: Arc<str> = Arc::from(s);
    pool.insert(s.to_string(), shared.clone());
    shared
}

/// Entrada de prueba (`cfg(test)` para compartir entre tests del módulo).
#[cfg(test)]
pub(crate) fn entry(id: &str, name: &str, kind: &str) -> Entry {
    Entry {
        docset_id: id.to_string(),
        name: name.to_string(),
        kind: kind.to_string(),
        path: format!("{name}.html"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_strings_are_interned() {
        let index = SearchIndex::build(vec![
            entry("css", "grid", "Property"),
            entry("css", "color", "Property"),
            entry("cpp", "vector", "Class"),
        ]);
        assert_eq!(index.len(), 3);
        assert!(Arc::ptr_eq(&index.entries[0].kind, &index.entries[1].kind));
        assert!(Arc::ptr_eq(
            &index.entries[0].docset_id,
            &index.entries[1].docset_id
        ));
        assert!(!Arc::ptr_eq(
            &index.entries[0].docset_id,
            &index.entries[2].docset_id
        ));
    }

    #[test]
    fn build_for_search_matches_build_but_skips_browse() {
        let entries = vec![
            entry("css", "grid", "Property"),
            entry("css", "color", "Property"),
            entry("cpp", "vector", "Class"),
        ];
        let full = SearchIndex::build(entries.clone());
        let mut light = SearchIndex::build_for_search(entries);
        assert_eq!(light.len(), full.len());
        // La navegación no se construye (el CLI no la usa).
        assert!(light.browse_kinds("css").is_empty());
        // Los resultados de búsqueda son idénticos.
        for query in ["grid", "col", "vector"] {
            let mut full_copy = SearchIndex::build(vec![
                entry("css", "grid", "Property"),
                entry("css", "color", "Property"),
                entry("cpp", "vector", "Class"),
            ]);
            let expected = crate::search::search(&mut full_copy, query, None, 10);
            let got = crate::search::search(&mut light, query, None, 10);
            let names = |hits: Vec<crate::search::SearchResult>| {
                hits.into_iter().map(|r| r.name).collect::<Vec<_>>()
            };
            assert_eq!(names(got), names(expected), "{query}");
        }
    }

    #[test]
    fn remove_docset_drops_only_its_entries() {
        let mut index = SearchIndex::build(vec![
            entry("a", "uno", "Guide"),
            entry("b", "dos", "Guide"),
            entry("a", "tres", "Guide"),
        ]);
        index.remove_docset("a");
        assert_eq!(index.len(), 1);
        assert_eq!(index.entries[0].name, "dos");
        // Navegación reconstruida: `a` ya no tiene tipos.
        assert!(index.browse_kinds("a").is_empty());
        assert_eq!(index.browse_kinds("b").len(), 1);
        // Id desconocido: no cambia nada.
        index.remove_docset("nadie");
        assert_eq!(index.len(), 1);
    }

    /// F1: `replace_docset` produce exactamente el mismo estado que
    /// `remove_docset` + `extend` (orden, kinds, conteos y búsqueda).
    type EntrySnap = Vec<(String, String, String, String)>;
    type KindsSnap = Vec<(String, usize)>;
    fn snapshot(index: &SearchIndex) -> (EntrySnap, KindsSnap) {
        let entries = index
            .entries
            .iter()
            .map(|e| {
                (
                    e.docset_id.to_string(),
                    e.name.clone(),
                    e.kind.to_string(),
                    e.path.clone(),
                )
            })
            .collect();
        let mut kinds: Vec<(String, usize)> = Vec::new();
        for id in ["a", "b", "c"] {
            for info in index.browse_kinds(id) {
                kinds.push((format!("{id}/{}", info.kind), info.count));
            }
        }
        (entries, kinds)
    }

    #[test]
    fn replace_docset_matches_remove_plus_extend() {
        let seed = vec![
            entry("a", "uno", "Guide"),
            entry("b", "dos", "Function"),
            entry("a", "tres", "Guide"),
            entry("c", "cuatro", "Class"),
        ];
        let mut old_way = SearchIndex::build(seed.clone());
        old_way.remove_docset("a");
        old_way.remove_docset("nadie");
        old_way.extend(vec![
            entry("a", "nueva", "Method"),
            entry("b", "otra", "Guide"),
        ]);

        let mut new_way = SearchIndex::build(seed);
        new_way.replace_docset(
            &["a".to_string(), "nadie".to_string()],
            vec![entry("a", "nueva", "Method"), entry("b", "otra", "Guide")],
        );

        assert_eq!(snapshot(&new_way), snapshot(&old_way));
        // La búsqueda ve lo mismo en ambos.
        for query in ["nueva", "dos", "uno"] {
            let mut old_search = old_way;
            let mut new_search = new_way;
            let old_hits = crate::search::search(&mut old_search, query, None, 10);
            let new_hits = crate::search::search(&mut new_search, query, None, 10);
            let names = |hits: Vec<crate::search::SearchResult>| {
                hits.into_iter().map(|r| r.name).collect::<Vec<_>>()
            };
            assert_eq!(names(new_hits), names(old_hits), "{query}");
            old_way = old_search;
            new_way = new_search;
        }
    }
}
