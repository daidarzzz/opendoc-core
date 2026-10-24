//! Escaneo de primer nivel de la carpeta de docsets.
//!
//! - Solo mira el primer nivel: los docsets van directamente en la raíz.
//! - No sigue enlaces simbólicos.
//! - Orden determinista: los docsets se devuelven ordenados por nombre y
//!   los slugs de colisión se asignan en orden de nombre de carpeta, así
//!   que el resultado no depende del orden del sistema de archivos.
//!
//! La raíz se recibe por parámetro: el escáner no asume ninguna carpeta
//! fija (en v0.3 el gestor de docsets lo reutilizará sobre la carpeta de
//! instalación).

use std::collections::HashSet;
use std::fs;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use super::index::read_index;
use super::model::{Docset, IssueKind, PendingTarix, ScanError, ScanIssue, ScanReport};
use super::plist::{apply_to_docset, read_info_plist};

/// Sufijo de carpeta que identifica un docset (insensible a mayúsculas).
const DOCSET_EXT: &str = ".docset";

/// Escanea `root` buscando carpetas `*.docset` en el primer nivel.
///
/// Devuelve error solo si la raíz no existe o no se puede leer; cualquier
/// problema dentro de la carpeta se registra en `ScanReport::issues`.
pub fn scan_dir(root: &Path) -> Result<ScanReport, ScanError> {
    let entries = fs::read_dir(root).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ScanError::RootNotFound {
                path: root.to_path_buf(),
            }
        } else {
            ScanError::RootUnreadable {
                path: root.to_path_buf(),
                source,
            }
        }
    })?;

    // Nombres de archivo ordenados para un resultado determinista.
    let mut names: Vec<String> = Vec::new();
    let mut entry_issues: Vec<ScanIssue> = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                // No seguir enlaces simbólicos.
                let is_symlink = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
                if !is_symlink {
                    names.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
            Err(_) => entry_issues.push(ScanIssue {
                path: root.to_path_buf(),
                kind: IssueKind::EntryUnreadable,
            }),
        }
    }
    names.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });

    let mut report = ScanReport {
        docsets: Vec::new(),
        pending_tarix: Vec::new(),
        issues: entry_issues,
        index_entries: Vec::new(),
    };
    let mut used_slugs: HashSet<String> = HashSet::new();

    for name in names {
        let Some(stem) = strip_docset_ext(&name) else {
            continue; // Ruido: no es un *.docset, se ignora en silencio.
        };
        let root_path = root.join(&name);
        if !root_path.is_dir() {
            report.issues.push(ScanIssue {
                path: root_path,
                kind: IssueKind::NotDirectory,
            });
            continue;
        }
        let contents_path = root_path.join("Contents");
        if !contents_path.is_dir() {
            report.issues.push(ScanIssue {
                path: root_path,
                kind: IssueKind::MissingContents,
            });
            continue;
        }
        if !contents_path.join("Resources/Documents").is_dir() {
            // Sin Documents/ pero con triada tarix → instalable (T10),
            // no un issue. Exige también el .dsidx (las entradas salen
            // de ahí); sin él es un índice ausente normal.
            let resources = contents_path.join("Resources");
            let is_tarix = resources.join("tarix.tgz").is_file()
                && resources.join("tarixIndex.db").is_file()
                && resources.join("docSet.dsidx").is_file();
            if is_tarix {
                let id = unique_slug(&slugify(stem), &mut used_slugs);
                report.pending_tarix.push(PendingTarix {
                    id,
                    name: stem.to_string(),
                    icon: super::icon::icon_data_url_for(&root_path),
                    root_path,
                });
                continue;
            }
            report.issues.push(ScanIssue {
                path: root_path,
                kind: IssueKind::MissingDocuments,
            });
            continue;
        }
        if !contents_path.join("Resources/docSet.dsidx").is_file() {
            report.issues.push(ScanIssue {
                path: root_path,
                kind: IssueKind::MissingIndex,
            });
            continue;
        }
        let id = unique_slug(&slugify(stem), &mut used_slugs);
        let mut docset = Docset {
            id,
            name: stem.to_string(),
            platform: None,
            version: None,
            bundle_id: None,
            home_path: None,
            icon: super::icon::icon_data_url_for(&root_path),
            root_path: root_path.clone(),
            contents_path: contents_path.clone(),
        };
        // Info.plist ausente → valores por defecto en silencio.
        // Ilegible → issue, conservando los valores por defecto.
        match read_info_plist(&contents_path) {
            Ok(Some(info)) => apply_to_docset(&mut docset, &info),
            Ok(None) => {}
            Err(_) => report.issues.push(ScanIssue {
                path: root_path.clone(),
                kind: IssueKind::InvalidInfoPlist,
            }),
        }
        // Sin página de inicio aún → primera entrada del índice (T4).
        // Índice ilegible → issue, el docset se conserva sin home.
        // Las entradas leídas viajan en el informe (F1) para que la
        // carga no reabra este índice; home, issues y orden intactos.
        if docset.home_path.is_none() {
            let dsidx = contents_path.join("Resources/docSet.dsidx");
            match read_index(&dsidx, &docset.id) {
                Ok(data) => {
                    if let Some(first) = data.entries.first() {
                        docset.home_path = Some(first.path.clone());
                    }
                    report.index_entries.extend(data.entries);
                }
                Err(_) => report.issues.push(ScanIssue {
                    path: root_path,
                    kind: IssueKind::InvalidIndex,
                }),
            }
        }
        report.docsets.push(docset);
    }

    // Orden determinista por nombre para tests y UI.
    report.docsets.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(report)
}

/// Quita la extensión `.docset` (insensible a mayúsculas).
/// Devuelve `None` si el nombre no termina en ella.
fn strip_docset_ext(name: &str) -> Option<&str> {
    if name.len() >= DOCSET_EXT.len()
        && name[name.len() - DOCSET_EXT.len()..].eq_ignore_ascii_case(DOCSET_EXT)
    {
        Some(&name[..name.len() - DOCSET_EXT.len()])
    } else {
        None
    }
}

/// Convierte un nombre en slug seguro para URL: minúsculas, solo
/// `[a-z0-9]`, resto a `-` (colapsados, sin bordes). Si no queda nada
/// (p. ej. nombres no ASCII), devuelve `"docset"`.
pub fn slugify(raw: &str) -> String {
    let mut slug = String::with_capacity(raw.len());
    let mut prev_dash = true; // Recorta guiones iniciales.
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        String::from("docset")
    } else {
        slug
    }
}

/// Asigna un slug único de forma determinista: el primero usa la base y
/// las colisiones reciben `-2`, `-3`... Se expone para que el instalador de
/// la app use el mismo algoritmo al colocar un docset recién descargado.
#[doc(hidden)]
pub fn unique_slug(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut n = 2_u32;
    loop {
        let candidate = format!("{base}-{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Une `parts` bajo `base` creando los directorios.
#[cfg(test)]
fn mkdirs(base: &Path, parts: &str) {
    fs::create_dir_all(base.join(parts)).expect("crear dirs de fixture");
}

/// Crea un esqueleto de docset. Con `complete = true` incluye
/// `Resources/Documents/` y un `docSet.dsidx` mínimo válido (esquema
/// estándar con una entrada, para que el fallback de home funcione).
#[cfg(test)]
fn make_docset(root: &Path, name: &str, complete: bool) -> PathBuf {
    let docset = root.join(name);
    mkdirs(&docset, "Contents/Resources");
    if complete {
        mkdirs(&docset, "Contents/Resources/Documents");
        let conn = rusqlite::Connection::open(docset.join("Contents/Resources/docSet.dsidx"))
            .expect("abrir dsidx");
        conn.execute_batch(
            "CREATE TABLE searchIndex(id INTEGER PRIMARY KEY, name TEXT, type TEXT, path TEXT);
             INSERT INTO searchIndex(name, type, path) VALUES ('home', 'Guide', 'home/page.html');",
        )
        .expect("poblar dsidx");
        drop(conn);
    }
    docset
}

#[cfg(test)]
mod tests {
    use super::super::model::ScanError;
    use super::*;

    #[test]
    fn valid_docset_is_scanned() {
        let dir = tempfile::tempdir().expect("tempdir");
        make_docset(dir.path(), "Python_3.docset", true);

        let report = scan_dir(dir.path()).expect("scan");
        assert!(report.issues.is_empty());
        assert_eq!(report.docsets.len(), 1);
        let docset = &report.docsets[0];
        assert_eq!(docset.name, "Python_3");
        assert_eq!(docset.id, "python-3");
        assert_eq!(docset.platform, None);
        assert_eq!(docset.version, None);
        assert_eq!(docset.bundle_id, None);
        // Sin plist ni index.html: el home sale de la primera entrada.
        assert_eq!(docset.home_path.as_deref(), Some("home/page.html"));
        assert_eq!(docset.root_path, dir.path().join("Python_3.docset"));
        assert_eq!(
            docset.contents_path,
            dir.path().join("Python_3.docset/Contents")
        );
    }

    #[test]
    fn missing_root_returns_error() {
        let missing = PathBuf::from("definitivamente/no/existe");
        let err = scan_dir(&missing).expect_err("debe fallar");
        assert!(matches!(err, ScanError::RootNotFound { .. }));
    }

    #[test]
    fn file_as_root_returns_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("un-fichero.txt");
        fs::write(&file, b"hola").expect("write");
        let err = scan_dir(&file).expect_err("debe fallar");
        assert!(matches!(err, ScanError::RootUnreadable { .. }));
    }

    #[test]
    fn corrupt_docsets_become_issues() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("SinContents.docset")).expect("mkdir");
        let no_docs = make_docset(dir.path(), "SinDocuments.docset", false);
        mkdirs(&no_docs, "Contents");
        let no_index = make_docset(dir.path(), "SinIndice.docset", false);
        mkdirs(&no_index, "Contents/Resources/Documents");
        fs::write(dir.path().join("Fichero.docset"), b"no soy un dir").expect("write");

        let report = scan_dir(dir.path()).expect("scan");
        assert!(report.docsets.is_empty());
        assert_eq!(report.issues.len(), 4);
        let kinds: Vec<IssueKind> = report.issues.iter().map(|i| i.kind.clone()).collect();
        assert!(kinds.contains(&IssueKind::MissingContents));
        assert!(kinds.contains(&IssueKind::MissingDocuments));
        assert!(kinds.contains(&IssueKind::MissingIndex));
        assert!(kinds.contains(&IssueKind::NotDirectory));
    }

    #[test]
    fn noise_is_ignored_silently() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("random.txt"), b"x").expect("write");
        fs::create_dir(dir.path().join("otra-carpeta")).expect("mkdir");

        let report = scan_dir(dir.path()).expect("scan");
        assert!(report.docsets.is_empty());
        assert!(report.issues.is_empty());
    }

    #[test]
    fn slug_collisions_get_deterministic_suffix() {
        let dir = tempfile::tempdir().expect("tempdir");
        make_docset(dir.path(), "Python 3.docset", true);
        make_docset(dir.path(), "Python_3.docset", true);
        make_docset(dir.path(), "PYTHON-3.docset", true);

        let report = scan_dir(dir.path()).expect("scan");
        let ids: Vec<&str> = report.docsets.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["python-3", "python-3-2", "python-3-3"]);
    }

    #[test]
    fn non_ascii_name_falls_back_to_docset_slug() {
        let dir = tempfile::tempdir().expect("tempdir");
        make_docset(dir.path(), "日本語.docset", true);

        let report = scan_dir(dir.path()).expect("scan");
        assert_eq!(report.docsets.len(), 1);
        assert_eq!(report.docsets[0].id, "docset");
    }

    #[test]
    fn docsets_are_sorted_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Creados en orden inverso a propósito.
        for name in ["Zebra.docset", "mango.docset", "CSS.docset"] {
            make_docset(dir.path(), name, true);
        }

        let report = scan_dir(dir.path()).expect("scan");
        let names: Vec<&str> = report.docsets.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["CSS", "mango", "Zebra"]);
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("Python_3"), "python-3");
        assert_eq!(slugify("  C++  "), "c");
        assert_eq!(slugify("--Hola--Mundo--"), "hola-mundo");
        assert_eq!(slugify("日本語"), "docset");
        assert_eq!(slugify(""), "docset");
    }

    /// Escribe un `Contents/Info.plist` sintético en un docset de fixture.
    fn write_test_plist(docset: &Path, body: &str) {
        fs::write(docset.join("Contents/Info.plist"), body).expect("escribir plist");
    }

    const PLIST_FULL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Mi Python</string>
<key>CFBundleIdentifier</key><string>python3</string>
<key>DocSetPlatformFamily</key><string>python</string>
<key>CFBundleShortVersionString</key><string>3.12</string>
<key>dashIndexFilePath</key><string>library/index.html</string>
</dict></plist>"#;

    #[test]
    fn plist_enriches_docset_but_keeps_folder_slug() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docset = make_docset(dir.path(), "Python_3.docset", true);
        write_test_plist(&docset, PLIST_FULL);

        let report = scan_dir(dir.path()).expect("scan");
        assert!(report.issues.is_empty());
        assert_eq!(report.docsets.len(), 1);
        let docset = &report.docsets[0];
        // El nombre mostrado viene del plist...
        assert_eq!(docset.name, "Mi Python");
        // ...pero el id sigue derivando de la carpeta (estable para URLs).
        assert_eq!(docset.id, "python-3");
        assert_eq!(docset.platform.as_deref(), Some("python"));
        assert_eq!(docset.version.as_deref(), Some("3.12"));
        assert_eq!(docset.bundle_id.as_deref(), Some("python3"));
        assert_eq!(docset.home_path.as_deref(), Some("library/index.html"));
    }

    #[test]
    fn corrupt_plist_becomes_issue_and_keeps_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docset = make_docset(dir.path(), "Roto.docset", true);
        write_test_plist(&docset, "ni siquiera xml");

        let report = scan_dir(dir.path()).expect("scan");
        assert_eq!(report.docsets.len(), 1);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].kind, IssueKind::InvalidInfoPlist);
        // Se conservan los valores por defecto de T2 (+ home del índice).
        assert_eq!(report.docsets[0].name, "Roto");
        assert_eq!(report.docsets[0].id, "roto");
        assert_eq!(report.docsets[0].platform, None);
        assert_eq!(
            report.docsets[0].home_path.as_deref(),
            Some("home/page.html")
        );
    }

    #[test]
    fn home_falls_back_to_index_html() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docset = make_docset(dir.path(), "SinHome.docset", true);
        write_test_plist(
            &docset,
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleName</key><string>SinHome</string>
</dict></plist>"#,
        );
        fs::write(
            docset.join("Contents/Resources/Documents/index.html"),
            b"<html></html>",
        )
        .expect("index.html");

        let report = scan_dir(dir.path()).expect("scan");
        assert!(report.issues.is_empty());
        assert_eq!(report.docsets[0].home_path.as_deref(), Some("index.html"));
    }

    #[test]
    fn real_css_fixture_home_comes_from_plist() {
        if super::super::fixture_or_skip("CSS.docset").is_none() {
            return;
        }
        let report = scan_dir(&crate::test_support::fixtures_dir()).expect("scan fixtures");
        let css = report
            .docsets
            .iter()
            .find(|d| d.id == "css")
            .expect("CSS.docset en fixtures");
        assert_eq!(css.name, "CSS");
        assert_eq!(css.platform.as_deref(), Some("css"));
        assert_eq!(
            css.home_path.as_deref(),
            Some("developer.mozilla.org/en-US/docs/Web/CSS/Reference.html")
        );
    }

    #[test]
    fn home_falls_back_to_first_index_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Sin plist y sin index.html: el home sale del índice.
        let docset = dir.path().join("Idx.docset");
        mkdirs(&docset, "Contents/Resources/Documents");
        let conn = rusqlite::Connection::open(docset.join("Contents/Resources/docSet.dsidx"))
            .expect("abrir dsidx");
        conn.execute_batch(
            "CREATE TABLE searchIndex(id INTEGER PRIMARY KEY, name TEXT, type TEXT, path TEXT);
             INSERT INTO searchIndex(name, type, path) VALUES
               ('printf', 'Function', 'man/printf.html'),
               ('malloc', 'Function', 'man/malloc.html');",
        )
        .expect("poblar dsidx");
        drop(conn);

        let report = scan_dir(dir.path()).expect("scan");
        assert!(report.issues.is_empty());
        assert_eq!(
            report.docsets[0].home_path.as_deref(),
            Some("man/printf.html")
        );
    }

    #[test]
    fn scanner_read_entries_travel_in_report() {
        // F1: las entradas leídas para el home viajan en el informe para
        // que la carga no reabra el índice (mismo id, nombre y ruta).
        let dir = tempfile::tempdir().expect("tempdir");
        make_docset(dir.path(), "Idx.docset", true);

        let report = scan_dir(dir.path()).expect("scan");
        assert!(report.issues.is_empty());
        assert_eq!(report.index_entries.len(), 1);
        let entry = &report.index_entries[0];
        assert_eq!(entry.docset_id, "idx");
        assert_eq!(entry.name, "home");
        assert_eq!(entry.path, "home/page.html");
        // Con home conocido (plist) no se lee el índice: nada viaja.
        let dir = tempfile::tempdir().expect("tempdir");
        let docset = make_docset(dir.path(), "Idx.docset", true);
        write_test_plist(
            &docset,
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleName</key><string>Idx</string>
<key>dashIndexFilePath</key><string>home/page.html</string>
</dict></plist>"#,
        );
        let report = scan_dir(dir.path()).expect("scan");
        assert!(report.issues.is_empty());
        assert!(report.index_entries.is_empty());
        assert_eq!(
            report.docsets[0].home_path.as_deref(),
            Some("home/page.html")
        );
    }

    #[test]
    fn tarix_style_docset_is_pending_not_issue() {
        // C++.docset es formato tarix (sin Documents/ pero con triada):
        // instalable (T10), no un issue.
        if super::super::fixture_or_skip("C++.docset").is_none() {
            return;
        }
        let report = scan_dir(&crate::test_support::fixtures_dir()).expect("scan fixtures");
        let pending = report
            .pending_tarix
            .iter()
            .find(|p| p.root_path.ends_with("C++.docset"))
            .expect("C++ pendiente");
        assert_eq!(pending.name, "C++");
        assert!(
            report
                .issues
                .iter()
                .all(|i| !i.path.ends_with("C++.docset")),
            "tarix no genera issues"
        );
    }

    #[test]
    fn real_fixture_icons_come_from_docset_root() {
        // C++ y Python_3 traen icon.png en la raíz (también como pendientes
        // tarix); CSS y Swift no traen y quedan en None (genérico en la UI).
        if super::super::fixture_or_skip("C++.docset").is_none() {
            return;
        }
        let report = scan_dir(&crate::test_support::fixtures_dir()).expect("scan fixtures");
        let icon_of = |suffix: &str| {
            report
                .docsets
                .iter()
                .find(|d| d.root_path.ends_with(suffix))
                .and_then(|d| d.icon.clone())
                .or_else(|| {
                    report
                        .pending_tarix
                        .iter()
                        .find(|p| p.root_path.ends_with(suffix))
                        .and_then(|p| p.icon.clone())
                })
        };
        for with_icon in ["C++.docset", "Python_3.docset"] {
            let url = icon_of(with_icon).expect("con icono");
            assert!(
                url.starts_with("data:image/png;base64,"),
                "{with_icon}: data-URL PNG"
            );
        }
        for without_icon in ["CSS.docset", "Swift.docset"] {
            assert!(
                icon_of(without_icon).is_none(),
                "{without_icon}: sin icono → genérico"
            );
        }
    }

    #[test]
    fn issue_kind_messages_are_clear() {
        assert_eq!(IssueKind::MissingContents.to_string(), "missing Contents/");
        assert_eq!(
            IssueKind::MissingDocuments.to_string(),
            "missing Contents/Resources/Documents/"
        );
        assert_eq!(
            IssueKind::MissingIndex.to_string(),
            "missing Contents/Resources/docSet.dsidx"
        );
    }
}
