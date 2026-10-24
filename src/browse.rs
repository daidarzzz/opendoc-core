//! Navegación por tipos (v0.2): tipos con contadores y entradas paginadas.
//!
//! Trabaja sobre `&SearchIndex`, sin lecturas nuevas de SQLite: ordenar y
//! contar se paga una sola vez al (re)construir el índice (`BrowseCache` se
//! invalida solo; `extend` lo reconstruye, p. ej. al instalar un tarix).
//! El orden de entradas es determinista: (nombre plegado, nombre, ruta).

use std::collections::HashMap;

use serde::Serialize;

use crate::search::{IndexedEntry, SearchIndex};

/// URL `opendoc://<id>/<ruta>` lista para el visor.
pub fn docset_url(id: &str, path: &str) -> String {
    format!("opendoc://{id}/{path}")
}

/// Tope de entradas por llamada (como `MAX_LIMIT` en búsqueda: no
/// serializar miles de entradas al frontend por accidente).
pub const MAX_BROWSE_LIMIT: usize = 500;
/// Página por defecto (la lista virtualizada pide de 100 en 100).
pub const DEFAULT_BROWSE_LIMIT: usize = 100;

/// Un tipo con su etiqueta visible y su número de entradas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KindInfo {
    /// Tipo normalizado (`Method`) o código original (`clm`).
    pub kind: String,
    /// Etiqueta visible en inglés (`Methods`).
    pub label: String,
    /// `true` si la etiqueta es inferida (no oficial en Dash): la UI
    /// muestra el código original como texto secundario.
    pub inferred: bool,
    /// Número de entradas de este tipo en el docset.
    pub count: usize,
}

/// Una entrada navegable, con su URL lista para el visor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NavEntry {
    /// Slug del docset.
    pub docset_id: String,
    /// Nombre mostrado.
    pub name: String,
    /// Tipo normalizado u original.
    pub kind: String,
    /// Ruta relativa a `Documents/`, con ancla si la hay.
    pub path: String,
    /// URL `opendoc://<id>/<ruta>` para abrir en el visor.
    pub url: String,
}

/// Tabla fija de (tipo, etiqueta): los comunes salen en este orden, luego
/// las inferidas (`INFERRED_LABELS`); lo no listado se muestra tal cual al
/// final, en orden alfabético. Inventario real (Paso 0): CSS trae
/// Property/Guide/Function/Class/Type/Element/Keyword; C++ Function/
/// Operator/Macro/Guide/File/Keyword/Enum/Tag/Attribute/Struct/Global/
/// Directive más códigos Apple (`cl`, `clm`, `clconst`, `tdef`, `instp`);
/// Python_3 Method/Function/Attribute/Section/Class/Macro/Option/Module/
/// Exception/Guide/Constant/Type/Variable/Statement/Struct/Enum.
const KIND_LABELS: &[(&str, &str)] = &[
    ("Class", "Classes"),
    ("Struct", "Structs"),
    ("Union", "Unions"),
    ("Enum", "Enums"),
    ("Interface", "Interfaces"),
    ("Protocol", "Protocols"),
    ("Trait", "Traits"),
    ("Category", "Categories"),
    ("Namespace", "Namespaces"),
    ("Function", "Functions"),
    ("Method", "Methods"),
    ("Constructor", "Constructors"),
    ("Destructor", "Destructors"),
    ("Operator", "Operators"),
    ("Macro", "Macros"),
    ("Property", "Properties"),
    ("Constant", "Constants"),
    ("Variable", "Variables"),
    ("Attribute", "Attributes"),
    ("Module", "Modules"),
    ("Guide", "Guides"),
    ("Section", "Sections"),
    ("Page", "Pages"),
    ("File", "Files"),
    ("Element", "Elements"),
    ("Type", "Types"),
    ("Keyword", "Keywords"),
    ("Exception", "Exceptions"),
    ("Option", "Options"),
    ("Statement", "Statements"),
    ("Typedef", "Typedefs"),
    ("Tag", "Tags"),
    ("Global", "Globals"),
    ("Directive", "Directives"),
];

/// Etiquetas inferidas (NO oficiales en Dash): códigos estilo Apple que
/// aparecen en docsets reales (C++ trae `cl`/`clm`/`clconst`/`tdef`/`instp`;
/// la guía oficial https://kapeli.com/docsets §13.5.1 no los lista).
/// Significado comprobado con 10 ejemplos por código en las fixtures:
/// `cl` son tipos (`weak_ptr`, `Namespace`, rangos…), `clm` mezcla
/// funciones miembro (`::swap`, `::find`) y tipos miembro (`::iterator`,
/// `::sentinel`), de ahí Members y no Methods; `clconst` son constantes
/// (`npos`, `*_v`, `seq`/`par`); `tdef` son alias (`int8_t`,
/// `conditional_t`).
/// `instp` (2 casos: `path::format`, `text_encoding::id`, enums miembro)
/// queda ambiguo y se muestra tal cual, como cualquier código no listado.
const INFERRED_LABELS: &[(&str, &str)] = &[
    ("cl", "Classes"),
    ("clm", "Members"),
    ("clconst", "Constants"),
    ("tdef", "Typedefs"),
];

/// Posición en el orden fijo: tabla oficial, luego inferidas en este
/// orden, luego lo desconocido (`usize::MAX`, alfabético entre sí).
fn kind_seq(kind: &str) -> usize {
    if let Some(pos) = KIND_LABELS.iter().position(|(k, _)| *k == kind) {
        return pos;
    }
    if let Some(pos) = INFERRED_LABELS.iter().position(|(k, _)| *k == kind) {
        return KIND_LABELS.len() + pos;
    }
    usize::MAX
}

/// Etiqueta visible: tabla oficial, inferida si aplica, código tal cual.
pub fn kind_label(kind: &str) -> String {
    KIND_LABELS
        .iter()
        .chain(INFERRED_LABELS.iter())
        .find(|(k, _)| *k == kind)
        .map_or_else(|| kind.to_string(), |(_, label)| label.to_string())
}

/// `true` si la etiqueta es inferida (código Apple, no oficial en Dash).
pub fn kind_inferred(kind: &str) -> bool {
    KIND_LABELS.iter().all(|(k, _)| *k != kind) && INFERRED_LABELS.iter().any(|(k, _)| *k == kind)
}

/// Caché de navegación: orden global + rangos y conteos por (docset, tipo).
/// Se construye con `rebuild` y se sustituye entera al recargar (nunca se
/// muta a medias).
#[derive(Debug, Default)]
pub struct BrowseCache {
    /// Índices a `SearchIndex::entries`, ordenados por (docset, kind,
    /// nombre plegado, nombre, ruta).
    order: Vec<usize>,
    /// Rango `[inicio, fin)` en `order` por (docset, kind).
    spans: HashMap<(String, String), (usize, usize)>,
    /// Tipos con conteo por docset, ya en orden de muestra.
    kinds: HashMap<String, Vec<KindInfo>>,
}

impl BrowseCache {
    /// Caché vacía (índice sin entradas).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Reconstruye la caché desde las entradas (coste `O(n log n)`, una vez
    /// por construcción/extensión del índice).
    pub fn rebuild(entries: &[IndexedEntry]) -> Self {
        // FASE 0: solo medida.
        let t_rebuild = std::time::Instant::now();
        let n = entries.len();
        let fold: Vec<String> = entries.iter().map(|e| e.name.to_lowercase()).collect();
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|&a, &b| {
            (
                &entries[a].docset_id,
                &entries[a].kind,
                &fold[a],
                &entries[a].name,
                &entries[a].path,
            )
                .cmp(&(
                    &entries[b].docset_id,
                    &entries[b].kind,
                    &fold[b],
                    &entries[b].name,
                    &entries[b].path,
                ))
        });
        let mut spans: HashMap<(String, String), (usize, usize)> = HashMap::new();
        let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
        let mut run_start = 0_usize;
        for (pos, &idx) in order.iter().enumerate() {
            let key = (entries[idx].docset_id.as_ref(), entries[idx].kind.as_ref());
            *counts.entry(key).or_default() += 1;
            let continues = pos + 1 < order.len() && {
                let next = order[pos + 1];
                entries[next].docset_id == entries[idx].docset_id
                    && entries[next].kind == entries[idx].kind
            };
            if !continues {
                spans.insert(
                    (
                        entries[idx].docset_id.to_string(),
                        entries[idx].kind.to_string(),
                    ),
                    (run_start, pos + 1),
                );
                run_start = pos + 1;
            }
        }
        // Agrupa conteos por docset y ordena tipos: tabla fija + resto A-Z.
        let mut per_docset: HashMap<&str, Vec<(&str, usize)>> = HashMap::new();
        for ((docset, kind), count) in counts {
            per_docset.entry(docset).or_default().push((kind, count));
        }
        let mut kinds: HashMap<String, Vec<KindInfo>> = HashMap::new();
        for (docset, mut list) in per_docset {
            list.sort_by(|a, b| kind_seq(a.0).cmp(&kind_seq(b.0)).then_with(|| a.0.cmp(b.0)));
            kinds.insert(
                docset.to_string(),
                list.into_iter()
                    .map(|(kind, count)| KindInfo {
                        kind: kind.to_string(),
                        label: kind_label(kind),
                        inferred: kind_inferred(kind),
                        count,
                    })
                    .collect(),
            );
        }
        let built = Self {
            order,
            spans,
            kinds,
        };
        // FASE 0: solo medida.
        crate::profile::mark(
            "index",
            format_args!(
                "browse_rebuild entries={n} ms={}",
                crate::profile::ms_since(t_rebuild)
            ),
        );
        built
    }
}

impl SearchIndex {
    /// Tipos con conteo de un docset, en orden de muestra (vacío si no hay).
    pub fn browse_kinds(&self, docset_id: &str) -> Vec<KindInfo> {
        self.browse
            .kinds
            .get(docset_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Página de entradas de un tipo (`offset`/`limit` ya saneados por el
    /// llamador). Tipo inexistente → vacío. Orden determinista global.
    pub fn browse_entries(
        &self,
        docset_id: &str,
        kind: &str,
        offset: usize,
        limit: usize,
    ) -> Vec<NavEntry> {
        let Some(&(start, end)) = self
            .browse
            .spans
            .get(&(docset_id.to_string(), kind.to_string()))
        else {
            return Vec::new();
        };
        self.browse.order[start..end]
            .iter()
            .skip(offset)
            .take(limit)
            .map(|&idx| {
                let e = &self.entries[idx];
                NavEntry {
                    docset_id: e.docset_id.to_string(),
                    name: e.name.clone(),
                    kind: e.kind.to_string(),
                    path: e.path.clone(),
                    url: docset_url(&e.docset_id, &e.path),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchIndex;

    /// Índice con varios docsets, tipos comunes y códigos raros.
    fn sample() -> SearchIndex {
        SearchIndex::build(vec![
            crate::docset::Entry {
                docset_id: "d".to_string(),
                name: "zebra".to_string(),
                kind: "Method".to_string(),
                path: "m/zebra.html".to_string(),
            },
            crate::docset::Entry {
                docset_id: "d".to_string(),
                name: "Alpha".to_string(),
                kind: "Method".to_string(),
                path: "m/alpha.html".to_string(),
            },
            crate::docset::Entry {
                docset_id: "d".to_string(),
                name: "beta".to_string(),
                kind: "Method".to_string(),
                path: "m/beta.html".to_string(),
            },
            crate::docset::Entry {
                docset_id: "d".to_string(),
                name: "Cosa".to_string(),
                kind: "clm".to_string(),
                path: "c/cosa.html".to_string(),
            },
            crate::docset::Entry {
                docset_id: "d".to_string(),
                name: "Guía".to_string(),
                kind: "Guide".to_string(),
                path: "g.html".to_string(),
            },
            crate::docset::Entry {
                docset_id: "otro".to_string(),
                name: "solo".to_string(),
                kind: "Function".to_string(),
                path: "s.html".to_string(),
            },
        ])
    }

    #[test]
    fn kinds_count_and_order() {
        let index = sample();
        let kinds = index.browse_kinds("d");
        // Orden fijo de comunes (Method < Guide) + raros al final.
        let names: Vec<&str> = kinds.iter().map(|k| k.kind.as_str()).collect();
        assert_eq!(names, vec!["Method", "Guide", "clm"]);
        assert_eq!(
            kinds[0],
            KindInfo {
                kind: "Method".to_string(),
                label: "Methods".to_string(),
                inferred: false,
                count: 3,
            }
        );
        assert_eq!(kinds[2].label, "Members"); // clm: inferida
        assert!(kinds[2].inferred);
        assert!(!kinds[0].inferred);
        assert!(index.browse_kinds("inexistente").is_empty());
        assert_eq!(index.browse_kinds("otro").len(), 1);
    }

    #[test]
    fn entries_order_and_pagination() {
        let index = sample();
        // Plegado: Alpha, beta, zebra (insensible a mayúsculas).
        let all = index.browse_entries("d", "Method", 0, 100);
        let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta", "zebra"]);
        assert_eq!(all[0].url, "opendoc://d/m/alpha.html");
        // Páginas contiguas cubren el total sin solapes.
        let p1 = index.browse_entries("d", "Method", 0, 2);
        let p2 = index.browse_entries("d", "Method", 2, 2);
        assert_eq!(p1.len(), 2);
        assert_eq!(p2.len(), 1);
        assert_eq!(p2[0].name, "zebra");
        // Bordes: más allá del final → vacío; tipo raro → vacío.
        assert!(index.browse_entries("d", "Method", 3, 10).is_empty());
        assert!(index.browse_entries("d", "NoExiste", 0, 10).is_empty());
        assert!(index.browse_entries("d", "Guide", 0, 0).is_empty());
    }

    #[test]
    fn entries_are_deterministic() {
        let (a, b) = (sample(), sample());
        assert_eq!(
            a.browse_entries("d", "Method", 0, 10),
            b.browse_entries("d", "Method", 0, 10)
        );
        assert_eq!(a.browse_kinds("d"), b.browse_kinds("d"));
    }

    #[test]
    fn extend_rebuilds_cache() {
        let mut index = sample();
        index.extend(vec![crate::docset::Entry {
            docset_id: "d".to_string(),
            name: "nueva".to_string(),
            kind: "Function".to_string(),
            path: "f/nueva.html".to_string(),
        }]);
        let kinds = index.browse_kinds("d");
        let names: Vec<&str> = kinds.iter().map(|k| k.kind.as_str()).collect();
        // Function (tabla) se inserta antes que Guide y clm.
        assert_eq!(names, vec!["Function", "Method", "Guide", "clm"]);
        assert_eq!(index.browse_entries("d", "Function", 0, 10).len(), 1);
    }

    #[test]
    fn inferred_before_unknown_alpha() {
        let index = SearchIndex::build(vec![
            crate::docset::Entry {
                docset_id: "d".to_string(),
                name: "x".to_string(),
                kind: "zzTop".to_string(),
                path: "x.html".to_string(),
            },
            crate::docset::Entry {
                docset_id: "d".to_string(),
                name: "y".to_string(),
                kind: "tdef".to_string(),
                path: "y.html".to_string(),
            },
            crate::docset::Entry {
                docset_id: "d".to_string(),
                name: "z".to_string(),
                kind: "instp".to_string(),
                path: "z.html".to_string(),
            },
        ]);
        let kinds = index.browse_kinds("d");
        let pairs: Vec<(&str, &str, bool)> = kinds
            .iter()
            .map(|k| (k.kind.as_str(), k.label.as_str(), k.inferred))
            .collect();
        // Inferida (tdef) antes que desconocidos (instp, zzTop: A-Z).
        assert_eq!(
            pairs,
            vec![
                ("tdef", "Typedefs", true),
                ("instp", "instp", false),
                ("zzTop", "zzTop", false),
            ]
        );
    }

    #[test]
    fn labels_cover_table_and_passthrough() {
        assert_eq!(kind_label("Class"), "Classes");
        assert_eq!(kind_label("Property"), "Properties");
        assert_eq!(kind_label("Category"), "Categories");
        assert_eq!(kind_label("cl"), "Classes");
        assert_eq!(kind_label("clm"), "Members");
        assert_eq!(kind_label("clconst"), "Constants");
        assert_eq!(kind_label("tdef"), "Typedefs");
        assert_eq!(kind_label("instp"), "instp");
        assert!(kind_inferred("clm"));
        assert!(!kind_inferred("Method"));
        assert!(!kind_inferred("instp"));
        assert!(kind_seq("Class") < kind_seq("Function"));
        assert!(kind_seq("Function") < kind_seq("Property"));
        assert!(kind_seq("Guide") < kind_seq("cl")); // tabla antes que inferidas
        assert!(kind_seq("tdef") < kind_seq("instp")); // inferida antes que resto
        assert_eq!(kind_seq("zzz-no-existe"), usize::MAX);
    }

    /// Fixture real CSS: los conteos suman el total y el orden es estable.
    #[test]
    fn real_css_kinds_sum_to_total() {
        let Some(path) =
            crate::docset::fixture_or_skip("CSS.docset/Contents/Resources/docSet.dsidx")
        else {
            return;
        };
        let data = crate::docset::read_index(&path, "css").expect("leer CSS");
        let index = SearchIndex::build(data.entries);
        let kinds = index.browse_kinds("css");
        let total: usize = kinds.iter().map(|k| k.count).sum();
        assert_eq!(total, 1249);
        // Inventario del Paso 0 (tipos normalizados por `read_index`).
        let get = |kind: &str| {
            kinds
                .iter()
                .find(|k| k.kind == kind)
                .map(|k| k.count)
                .unwrap_or(0)
        };
        assert_eq!(get("Property"), 638);
        assert_eq!(get("Guide"), 252);
        assert_eq!(get("Function"), 137);
        assert_eq!(get("Class"), 96);
        assert_eq!(get("Type"), 64);
        assert_eq!(get("Element"), 53);
        assert_eq!(get("Keyword"), 9);
        // Paginar un tipo completo por trozos lo cubre sin huecos y en
        // orden (plegado, nombre, ruta).
        let mut rows = Vec::new();
        for offset in (0..638).step_by(100) {
            rows.extend(
                index
                    .browse_entries("css", "Property", offset, 100)
                    .into_iter()
                    .map(|e| (e.name.to_lowercase(), e.name, e.path)),
            );
        }
        assert_eq!(rows.len(), 638);
        let mut sorted = rows.clone();
        sorted.sort();
        assert_eq!(rows, sorted);
    }
}
