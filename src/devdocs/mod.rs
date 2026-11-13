//! Importador de paquetes DevDocs → docset de OpenDoc.
//!
//! DevDocs publica por documentación un `index.json` (entradas) y un
//! `db.json` (mapa `ruta → fragmento HTML`). [`convert`] los transforma una
//! sola vez, al instalar, en un `<Nombre>.docset` estándar
//! (`Info.plist` + `docSet.dsidx` + `Documents/*.html`), de modo que la
//! búsqueda, el navegador por tipos, el servidor de ficheros y la CLI lo
//! tratan igual que cualquier docset de Dash/Zeal.
//!
//! Garantías:
//! - Entrada no confiable: las claves se sanean (ver [`paths`]) y nada se
//!   escribe fuera del directorio temporal de la conversión.
//! - Atómico: se construye en un directorio temporal hermano y solo se
//!   renombra al destino si todo salió bien; ante cualquier error el
//!   destino previo queda intacto.
//! - Tolerante con los enlaces: los enlaces internos se reescriben a los
//!   ficheros reales, los muertos se desactivan (`#`) y las anclas que
//!   DevDocs pierde en la cabecera de la página se recrean.
//! - Estricto con el paquete: sin páginas, sin entradas válidas o con
//!   demasiadas entradas sin página, la conversión se cancela.
//!
//! La descarga HTTP no vive aquí (el core no tiene cliente HTTP): la
//! aplicación obtiene los bytes ([`index_url`], [`db_url`]) y llama a
//! [`convert`].

mod links;
mod paths;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use links::{
    collect_ids, encode_path, escape, h1_text, is_external, percent_decode, relative_file,
    resolve_key, rewrite_hrefs, slug_of, split_href, strip_h1_backticks, AnchorIndex, Rewrite,
};

/// Hoja de estilos propia que se copia a cada docset convertido.
const CSS: &str = include_str!("devdocs.css");
/// Nombre del CSS dentro de `Documents/`.
const CSS_FILE: &str = "_opendoc-devdocs.css";

/// Tope de páginas por paquete (el mayor de DevDocs ronda las 8.000).
pub const MAX_PAGES: usize = 100_000;
/// Tope de HTML total por paquete, en bytes.
pub const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// Fracción máxima tolerada de entradas sin página (1/N).
const MAX_MISSING_ENTRIES_RATIO: usize = 20;

/// Metadatos de una documentación (elemento de `docs.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevDocsMeta {
    /// Nombre mostrado (`Python`).
    pub name: String,
    /// Slug del paquete (`python~3.12`).
    pub slug: String,
    /// Versión publicada (`3.12.9`), si la hay.
    #[serde(default)]
    pub release: Option<String>,
    /// Versión corta (`3.12`), a veces vacía.
    #[serde(default)]
    pub version: Option<String>,
    /// Marca de tiempo de la última actualización (sirve para detectar
    /// actualizaciones).
    #[serde(default)]
    pub mtime: Option<u64>,
    /// Atribución/licencia en HTML.
    #[serde(default)]
    pub attribution: Option<String>,
    /// Tamaño de `db.json` en bytes.
    #[serde(default)]
    pub db_size: Option<u64>,
}

/// Lee el catálogo `docs.json` de DevDocs.
pub fn parse_catalog(docs_json: &[u8]) -> Result<Vec<DevDocsMeta>, DevDocsError> {
    serde_json::from_slice(docs_json).map_err(DevDocsError::InvalidCatalog)
}

/// URL de `index.json` de un paquete.
pub fn index_url(slug: &str) -> String {
    format!("https://documents.devdocs.io/{slug}/index.json")
}

/// URL de `db.json` de un paquete.
pub fn db_url(slug: &str) -> String {
    format!("https://documents.devdocs.io/{slug}/db.json")
}

/// `OpenDocDevDocsMtime` guardado en el `Info.plist` de un docset
/// convertido (`None` si no es de DevDocs o el plist no se puede leer).
/// Sirve para saber si hay una versión más reciente en el catálogo.
pub fn installed_mtime(docset_root: &Path) -> Option<u64> {
    let value = plist::Value::from_file(docset_root.join("Contents/Info.plist")).ok()?;
    let dict = value.into_dictionary()?;
    if dict.get("OpenDocSource")?.as_string()? != "devdocs" {
        return None;
    }
    dict.get("OpenDocDevDocsMtime")?.as_unsigned_integer()
}

/// Resumen de una conversión.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConvertReport {
    /// Páginas escritas.
    pub pages: usize,
    /// Entradas en `docSet.dsidx`.
    pub entries: usize,
    /// Entradas descartadas por apuntar a una página inexistente.
    pub entries_dropped: usize,
    /// Entradas cuya ancla no existe en el HTML (abren la página arriba).
    pub entry_anchors_missing: usize,
    /// Claves cuyo nombre de fichero hubo que sanear.
    pub sanitized_names: usize,
    /// Enlaces internos reescritos a su fichero.
    pub links_internal: usize,
    /// Enlaces internos sin página destino (desactivados).
    pub links_dead: usize,
    /// Enlaces a un ancla inexistente que no se pudo recrear.
    pub links_anchor_missing: usize,
    /// Anclas de cabecera recreadas (`#module-os`...).
    pub anchors_synthesized: usize,
}

/// Errores de la conversión. Ninguno deja un docset a medias.
#[derive(Debug, thiserror::Error)]
pub enum DevDocsError {
    /// Slug vacío o con caracteres no permitidos.
    #[error("invalid DevDocs slug: {0:?}")]
    InvalidSlug(String),
    /// El destino no termina en `.docset` o no tiene nombre.
    #[error("destination must be a *.docset folder: {}", .0.display())]
    InvalidDest(PathBuf),
    /// `docs.json` ilegible.
    #[error("invalid DevDocs catalog: {0}")]
    InvalidCatalog(#[source] serde_json::Error),
    /// `index.json` ilegible.
    #[error("invalid DevDocs index.json: {0}")]
    InvalidIndex(#[source] serde_json::Error),
    /// `db.json` ilegible.
    #[error("invalid DevDocs db.json: {0}")]
    InvalidDb(#[source] serde_json::Error),
    /// El paquete no tiene páginas o entradas utilizables.
    #[error("package has no usable {0}")]
    Empty(&'static str),
    /// Supera los topes de seguridad.
    #[error("package too large (pages={pages}, bytes={bytes})")]
    TooLarge {
        /// Número de páginas.
        pages: usize,
        /// Bytes de HTML.
        bytes: u64,
    },
    /// Demasiadas entradas del índice sin página: paquete incompleto.
    #[error("incomplete package: {dropped} of {total} index entries have no page")]
    TooManyMissing {
        /// Entradas sin página.
        dropped: usize,
        /// Entradas totales.
        total: usize,
    },
    /// Error de disco.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// Error al construir el índice SQLite.
    #[error("index error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Error al escribir `Info.plist`.
    #[error("Info.plist error: {0}")]
    Plist(#[from] plist::Error),
}

#[derive(Deserialize)]
struct IndexFile {
    entries: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    name: String,
    path: String,
    #[serde(rename = "type", default)]
    kind: String,
}

/// Convierte un paquete DevDocs en `dest` (`.../<Nombre>.docset`).
///
/// Si `dest` existe se sustituye de forma atómica. Es bloqueante y pesada
/// en disco: la aplicación debe llamarla desde un hilo bloqueante.
pub fn convert(
    index_json: &[u8],
    db_json: &[u8],
    meta: &DevDocsMeta,
    dest: &Path,
) -> Result<ConvertReport, DevDocsError> {
    validate_slug(&meta.slug)?;
    let dest_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| n.to_ascii_lowercase().ends_with(".docset") && n.len() > ".docset".len())
        .ok_or_else(|| DevDocsError::InvalidDest(dest.to_path_buf()))?
        .to_string();

    let index: IndexFile =
        serde_json::from_slice(index_json).map_err(DevDocsError::InvalidIndex)?;
    let db: HashMap<String, String> =
        serde_json::from_slice(db_json).map_err(DevDocsError::InvalidDb)?;
    let total_bytes: u64 = db.values().map(|h| h.len() as u64).sum();
    if db.len() > MAX_PAGES || total_bytes > MAX_TOTAL_BYTES {
        return Err(DevDocsError::TooLarge {
            pages: db.len(),
            bytes: total_bytes,
        });
    }
    if db.is_empty() {
        return Err(DevDocsError::Empty("pages"));
    }
    let known = |path: &str| db.contains_key(path.split('#').next().unwrap_or(path));
    let valid = index.entries.iter().filter(|e| known(&e.path)).count();
    if valid == 0 {
        return Err(DevDocsError::Empty("index entries"));
    }
    let dropped = index.entries.len() - valid;
    if dropped * MAX_MISSING_ENTRIES_RATIO > index.entries.len() {
        return Err(DevDocsError::TooManyMissing {
            dropped,
            total: index.entries.len(),
        });
    }

    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{dest_name}.tmp-{}", std::process::id()));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    let result = build(&tmp, &index, &db, meta, dropped);
    match result {
        Ok(report) => {
            if let Err(e) = replace_final(dest, &tmp) {
                let _ = fs::remove_dir_all(&tmp);
                return Err(e.into());
            }
            Ok(report)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

/// `true` si el slug es seguro para URLs, ids y nombres de carpeta.
pub fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 80
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-'))
        && !slug.starts_with('.')
}

fn validate_slug(slug: &str) -> Result<(), DevDocsError> {
    if is_valid_slug(slug) {
        Ok(())
    } else {
        Err(DevDocsError::InvalidSlug(slug.to_string()))
    }
}

/// Construye el docset completo dentro de `root`.
fn build(
    root: &Path,
    index: &IndexFile,
    db: &HashMap<String, String>,
    meta: &DevDocsMeta,
    dropped: usize,
) -> Result<ConvertReport, DevDocsError> {
    let resources = root.join("Contents/Resources");
    let documents = resources.join("Documents");
    fs::create_dir_all(&documents)?;
    fs::write(documents.join(CSS_FILE), CSS)?;

    let mut keys: Vec<&str> = db.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let path_map = paths::build(&keys);
    let mut report = ConvertReport {
        pages: keys.len(),
        entries_dropped: dropped,
        sanitized_names: path_map.sanitized,
        ..ConvertReport::default()
    };

    // Pasada 1: anclas existentes y títulos.
    let ids: HashMap<&str, AnchorIndex> = keys
        .iter()
        .map(|k| (*k, AnchorIndex::new(collect_ids(&db[*k]))))
        .collect();
    let h1_slugs: HashMap<&str, String> = keys
        .iter()
        .map(|k| {
            (
                *k,
                h1_text(&db[*k]).map(|t| slug_of(&t)).unwrap_or_default(),
            )
        })
        .collect();

    // Pasada 2: anclas de cabecera que DevDocs pierde (`#module-os`,
    // `#argparse-tutorial`): se recrean al principio de la página destino.
    let mut synthetic: HashMap<String, BTreeSet<String>> = HashMap::new();
    for key in &keys {
        rewrite_hrefs(&db[*key], |href| {
            if let Some((target, Some(frag))) = internal_target(key, href, db) {
                let t = target.as_str();
                if ids[t].resolve(frag).is_none() && is_header_anchor(frag, &h1_slugs[t]) {
                    synthetic
                        .entry(target.clone())
                        .or_default()
                        .insert(frag.to_string());
                }
            }
            Rewrite::Keep
        });
    }
    report.anchors_synthesized = synthetic.values().map(BTreeSet::len).sum();

    // Pasada 3: reescritura y escritura de páginas.
    let mut made_dirs: HashSet<PathBuf> = HashSet::new();
    for key in &keys {
        let file = &path_map.files[*key];
        let html = strip_h1_backticks(&db[*key]);
        let from_file = file.as_str();
        let body = rewrite_hrefs(&html, |href| {
            if is_external(href) {
                return Rewrite::Keep;
            }
            let (path, frag) = split_href(href);
            if path.is_empty() {
                // Ancla de la misma página (o `#`).
                let Some(frag) = frag.filter(|f| !f.is_empty()) else {
                    return Rewrite::Keep;
                };
                return match fix_fragment(&ids[*key], synthetic.get(*key), frag) {
                    Fragment::Fixed(real) => Rewrite::Replace(format!("#{real}")),
                    Fragment::Missing => {
                        report.links_anchor_missing += 1;
                        Rewrite::Keep
                    }
                    Fragment::Ok => Rewrite::Keep,
                };
            }
            let target = resolve_key(key, path);
            let Some(target_file) = path_map.files.get(&target) else {
                report.links_dead += 1;
                return Rewrite::Replace(String::from("#"));
            };
            report.links_internal += 1;
            let mut new = encode_path(&relative_file(from_file, target_file));
            if let Some(frag) = frag.filter(|f| !f.is_empty()) {
                let anchors = &ids[target.as_str()];
                let frag = match fix_fragment(anchors, synthetic.get(target.as_str()), frag) {
                    Fragment::Fixed(real) => real,
                    Fragment::Missing => {
                        report.links_anchor_missing += 1;
                        frag.to_string()
                    }
                    Fragment::Ok => frag.to_string(),
                };
                new.push('#');
                new.push_str(&frag);
            }
            Rewrite::Replace(new)
        });
        let title = h1_text(&html).unwrap_or_else(|| (*key).to_string());
        let depth = file.matches('/').count();
        let anchors: String = synthetic
            .get(*key)
            .map(|set| {
                set.iter()
                    .map(|a| format!("<span id=\"{}\"></span>", escape(a)))
                    .collect()
            })
            .unwrap_or_default();
        let page = format!(
            "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{}</title>\
             <link rel=\"stylesheet\" href=\"{}{CSS_FILE}\"></head>\
             <body><div class=\"_page\">{anchors}{body}</div></body></html>",
            escape(&title),
            "../".repeat(depth),
        );
        let target = documents.join(file);
        if let Some(dir) = target.parent() {
            if made_dirs.insert(dir.to_path_buf()) {
                fs::create_dir_all(dir)?;
            }
        }
        fs::write(&target, page)?;
    }

    write_index(
        &resources.join("docSet.dsidx"),
        index,
        &path_map.files,
        &ids,
        &mut report,
    )?;
    let home = path_map
        .files
        .get("index")
        .or_else(|| keys.first().and_then(|k| path_map.files.get(*k)))
        .cloned()
        .unwrap_or_else(|| String::from("index.html"));
    write_info_plist(&root.join("Contents/Info.plist"), meta, &encode_path(&home))?;
    Ok(report)
}

/// Destino interno de un `href` (clave de página existente + ancla).
fn internal_target<'a>(
    key: &str,
    href: &'a str,
    db: &HashMap<String, String>,
) -> Option<(String, Option<&'a str>)> {
    if is_external(href) {
        return None;
    }
    let (path, frag) = split_href(href);
    let target = if path.is_empty() {
        key.to_string()
    } else {
        resolve_key(key, path)
    };
    db.contains_key(&target)
        .then_some((target, frag.filter(|f| !f.is_empty())))
}

/// Qué hacer con el fragmento de un enlace.
enum Fragment {
    /// Existe (o se recreará como ancla de cabecera).
    Ok,
    /// Existe otra ancla equivalente: se usa esa.
    Fixed(String),
    /// No hay forma de localizarla: la página abrirá arriba.
    Missing,
}

fn fix_fragment(
    anchors: &AnchorIndex,
    synthetic: Option<&BTreeSet<String>>,
    frag: &str,
) -> Fragment {
    if anchors.contains(frag) || synthetic.is_some_and(|s| s.contains(frag)) {
        return Fragment::Ok;
    }
    match anchors.resolve(frag) {
        Some(real) if real == frag => Fragment::Ok,
        Some(real) => Fragment::Fixed(real),
        None => Fragment::Missing,
    }
}

/// `true` si el ancla ausente es, con alta probabilidad, la cabecera de la
/// página (módulos de Sphinx o el slug del propio `<h1>`).
fn is_header_anchor(frag: &str, h1_slug: &str) -> bool {
    if frag.starts_with("module-") {
        return true;
    }
    let decoded = percent_decode(frag);
    !h1_slug.is_empty() && decoded == *h1_slug
}

/// Crea `docSet.dsidx` (esquema estándar de Dash).
fn write_index(
    path: &Path,
    index: &IndexFile,
    files: &HashMap<String, String>,
    ids: &HashMap<&str, AnchorIndex>,
    report: &mut ConvertReport,
) -> Result<(), DevDocsError> {
    let mut rows: BTreeMap<(String, String, String), ()> = BTreeMap::new();
    for e in &index.entries {
        let (page, frag) = match e.path.split_once('#') {
            Some((p, f)) => (p, Some(f)),
            None => (e.path.as_str(), None),
        };
        let Some(file) = files.get(page) else {
            continue;
        };
        let mut target = encode_path(file);
        if let Some(frag) = frag.filter(|f| !f.is_empty()) {
            let frag = match ids[page].resolve(frag) {
                Some(real) => real,
                None => {
                    report.entry_anchors_missing += 1;
                    frag.to_string()
                }
            };
            target.push('#');
            target.push_str(&frag);
        }
        let kind = match e.kind.trim() {
            "" => "Guide",
            k => k,
        };
        rows.insert((e.name.clone(), kind.to_string(), target), ());
    }
    report.entries = rows.len();

    let mut conn = rusqlite::Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE searchIndex(id INTEGER PRIMARY KEY, name TEXT, type TEXT, path TEXT);
         CREATE UNIQUE INDEX anchor ON searchIndex (name, type, path);",
    )?;
    let tx = conn.transaction()?;
    {
        let mut stmt =
            tx.prepare("INSERT INTO searchIndex(name, type, path) VALUES (?1, ?2, ?3)")?;
        for (name, kind, target) in rows.keys() {
            stmt.execute(rusqlite::params![name, kind, target])?;
        }
    }
    tx.commit()?;
    conn.close().map_err(|(_, e)| e)?;
    Ok(())
}

/// Escribe `Info.plist` con los metadatos del docset y su procedencia.
fn write_info_plist(path: &Path, meta: &DevDocsMeta, home: &str) -> Result<(), DevDocsError> {
    let mut dict = plist::Dictionary::new();
    let name = if meta.name.trim().is_empty() {
        meta.slug.as_str()
    } else {
        meta.name.trim()
    };
    dict.insert(
        "CFBundleIdentifier".into(),
        format!("devdocs-{}", meta.slug).into(),
    );
    dict.insert("CFBundleName".into(), name.into());
    let family = meta.slug.split('~').next().unwrap_or(&meta.slug);
    dict.insert("DocSetPlatformFamily".into(), family.into());
    dict.insert("dashIndexFilePath".into(), home.into());
    dict.insert("isDashDocset".into(), true.into());
    let version = [&meta.release, &meta.version]
        .into_iter()
        .flatten()
        .map(|v| v.trim())
        .find(|v| !v.is_empty());
    if let Some(v) = version {
        dict.insert("CFBundleVersion".into(), v.into());
    }
    dict.insert("OpenDocSource".into(), "devdocs".into());
    dict.insert("OpenDocDevDocsSlug".into(), meta.slug.as_str().into());
    if let Some(mtime) = meta.mtime {
        dict.insert(
            "OpenDocDevDocsMtime".into(),
            plist::Value::Integer(mtime.into()),
        );
    }
    if let Some(attr) = meta.attribution.as_deref() {
        let text = strip_tags(attr);
        if !text.is_empty() {
            dict.insert("OpenDocAttribution".into(), text.into());
        }
    }
    plist::Value::Dictionary(dict).to_file_xml(path)?;
    Ok(())
}

/// Texto plano de un fragmento HTML corto (la atribución).
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let text = links::unescape(&out)
        .replace("&copy;", "©")
        .replace("&ndash;", "–")
        .replace("&mdash;", "—");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sustituye `dest` por `tmp` conservando el anterior si algo falla.
fn replace_final(dest: &Path, tmp: &Path) -> io::Result<()> {
    let old = dest.with_extension("docset.old");
    if old.exists() {
        fs::remove_dir_all(&old)?;
    }
    if dest.exists() {
        fs::rename(dest, &old)?;
    }
    if let Err(e) = fs::rename(tmp, dest) {
        if old.exists() && !dest.exists() {
            let _ = fs::rename(&old, dest);
        }
        return Err(e);
    }
    if old.exists() {
        let _ = fs::remove_dir_all(&old);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
