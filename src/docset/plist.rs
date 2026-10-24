//! Lectura tolerante de `Contents/Info.plist`.
//!
//! Todos los campos son opcionales y los tipos inesperados se ignoran
//! (p. ej. un entero donde se espera string da `None`, no un error).
//! `DashDocSetFallbackURL` se ignora a propósito: los docsets "online
//! redirect" están fuera del MVP (SPEC §3.2).

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use super::model::Docset;

/// Metadatos leídos de `Info.plist`. Todo opcional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InfoPlist {
    /// `CFBundleName`: nombre mostrado (sobrescribe el de la carpeta).
    pub bundle_name: Option<String>,
    /// `CFBundleIdentifier`.
    pub bundle_id: Option<String>,
    /// `DocSetPlatformFamily`.
    pub platform_family: Option<String>,
    /// `dashIndexFilePath`: página de inicio del docset.
    pub dash_index_path: Option<String>,
    /// `isDashDocset`.
    pub is_dash_docset: Option<bool>,
    /// `DashDocSetFamily` (tipo de TOC, no plataforma: no se usa como tal).
    pub dash_docset_family: Option<String>,
    /// `CFBundleShortVersionString` o, en su defecto, `CFBundleVersion`.
    pub version: Option<String>,
}

/// Error al leer un `Info.plist` presente en disco.
/// (Si el fichero no existe, `read_info_plist` devuelve `Ok(None)`.)
#[derive(Debug, thiserror::Error)]
pub enum PlistError {
    /// El fichero existe pero no se puede leer.
    #[error("cannot read Info.plist: {}", path.display())]
    Unreadable {
        /// Ruta del `Info.plist`.
        path: PathBuf,
        /// Error de E/S subyacente.
        #[source]
        source: std::io::Error,
    },
    /// El fichero se leyó pero no es un plist válido.
    #[error("invalid Info.plist: {}", path.display())]
    Invalid {
        /// Ruta del `Info.plist`.
        path: PathBuf,
        /// Error de parseo subyacente.
        #[source]
        source: plist::Error,
    },
}

/// Lee `Contents/Info.plist`.
///
/// - `Ok(None)`: no hay fichero → el llamador usa los valores por defecto.
/// - `Err(_)`: fichero presente pero ilegible → el llamador lo registra
///   como `IssueKind::InvalidInfoPlist` y conserva los valores por defecto.
pub fn read_info_plist(contents: &Path) -> Result<Option<InfoPlist>, PlistError> {
    let path = contents.join("Info.plist");
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|source| PlistError::Unreadable {
        path: path.clone(),
        source,
    })?;
    let value =
        plist::Value::from_reader(Cursor::new(bytes)).map_err(|source| PlistError::Invalid {
            path: path.clone(),
            source,
        })?;
    let Some(dict) = value.into_dictionary() else {
        // Raíz no-dict: nada aprovechable, como si no hubiera metadatos.
        return Ok(Some(InfoPlist::default()));
    };
    Ok(Some(InfoPlist {
        bundle_name: get_string(&dict, "CFBundleName"),
        bundle_id: get_string(&dict, "CFBundleIdentifier"),
        platform_family: get_string(&dict, "DocSetPlatformFamily"),
        dash_index_path: get_string(&dict, "dashIndexFilePath"),
        is_dash_docset: dict.get("isDashDocset").and_then(|v| v.as_boolean()),
        dash_docset_family: get_string(&dict, "DashDocSetFamily"),
        version: get_string(&dict, "CFBundleShortVersionString")
            .or_else(|| get_string(&dict, "CFBundleVersion")),
    }))
}

/// Extrae un string no vacío del diccionario. Tipos inesperados → `None`.
fn get_string(dict: &plist::Dictionary, key: &str) -> Option<String> {
    dict.get(key)
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Aplica los metadatos a un `Docset` ya validado por el escáner.
///
/// El `id` no se toca: deriva de la carpeta y debe ser estable para las
/// URLs `opendoc://`. La página de inicio sigue el fallback acordado:
/// `dashIndexFilePath` → `index.html` si existe → `None` (T4 la completa
/// con la primera entrada del índice).
pub fn apply_to_docset(docset: &mut Docset, info: &InfoPlist) {
    if let Some(name) = &info.bundle_name {
        docset.name = name.clone();
    }
    if info.platform_family.is_some() {
        docset.platform = info.platform_family.clone();
    }
    if info.version.is_some() {
        docset.version = info.version.clone();
    }
    if info.bundle_id.is_some() {
        docset.bundle_id = info.bundle_id.clone();
    }
    if let Some(home) = &info.dash_index_path {
        docset.home_path = Some(home.clone());
    } else if docset
        .contents_path
        .join("Resources/Documents/index.html")
        .is_file()
    {
        docset.home_path = Some(String::from("index.html"));
    }
}

/// Escribe `contents` en `dir/Contents/Info.plist` (solo tests).
#[cfg(test)]
fn write_plist(dir: &Path, contents: &str) {
    fs::create_dir_all(dir.join("Contents")).expect("crear Contents");
    fs::write(dir.join("Contents/Info.plist"), contents).expect("escribir plist");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_css_plist() {
        let Some(contents) = super::super::fixture_or_skip("CSS.docset/Contents") else {
            return;
        };
        let info = read_info_plist(&contents)
            .expect("leer plist")
            .expect("debe existir");
        assert_eq!(info.bundle_name.as_deref(), Some("CSS"));
        assert_eq!(info.bundle_id.as_deref(), Some("css"));
        assert_eq!(info.platform_family.as_deref(), Some("css"));
        assert_eq!(
            info.dash_index_path.as_deref(),
            Some("developer.mozilla.org/en-US/docs/Web/CSS/Reference.html")
        );
        assert_eq!(info.dash_docset_family.as_deref(), Some("unsorteddashtoc"));
        // Este plist no trae versión ni isDashDocset: deben ser None, no error.
        assert_eq!(info.version, None);
        assert_eq!(info.is_dash_docset, None);
    }

    #[test]
    fn real_python_plist() {
        let Some(contents) = super::super::fixture_or_skip("Python_3.docset/Contents") else {
            return;
        };
        let info = read_info_plist(&contents)
            .expect("leer plist")
            .expect("debe existir");
        assert_eq!(info.bundle_name.as_deref(), Some("Python"));
        assert_eq!(info.bundle_id.as_deref(), Some("python"));
        assert_eq!(info.platform_family.as_deref(), Some("python"));
        assert_eq!(info.dash_index_path.as_deref(), Some("doc/index.html"));
        assert_eq!(info.version, None);
    }

    #[test]
    fn missing_plist_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let info = read_info_plist(dir.path()).expect("no debe fallar");
        assert_eq!(info, None);
    }

    #[test]
    fn corrupt_plist_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plist(dir.path(), "esto no es un plist \x00\x01");
        let err = read_info_plist(&dir.path().join("Contents")).expect_err("debe fallar");
        assert!(matches!(err, PlistError::Invalid { .. }));
    }

    #[test]
    fn empty_dict_plist_gives_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plist(
            dir.path(),
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict/></plist>"#,
        );
        let info = read_info_plist(&dir.path().join("Contents"))
            .expect("no debe fallar")
            .expect("debe existir");
        assert_eq!(info, InfoPlist::default());
    }

    #[test]
    fn non_dict_root_gives_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plist(
            dir.path(),
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><array><string>x</string></array></plist>"#,
        );
        let info = read_info_plist(&dir.path().join("Contents"))
            .expect("no debe fallar")
            .expect("debe existir");
        assert_eq!(info, InfoPlist::default());
    }

    #[test]
    fn wrong_types_are_tolerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plist(
            dir.path(),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleName</key><integer>42</integer>
<key>isDashDocset</key><string>sí</string>
<key>DocSetPlatformFamily</key><string>py</string>
<key>CFBundleVersion</key><string>3.12</string>
</dict></plist>"#,
        );
        let info = read_info_plist(&dir.path().join("Contents"))
            .expect("no debe fallar")
            .expect("debe existir");
        assert_eq!(info.bundle_name, None);
        assert_eq!(info.is_dash_docset, None);
        assert_eq!(info.platform_family.as_deref(), Some("py"));
        // CFBundleVersion como fallback de CFBundleShortVersionString.
        assert_eq!(info.version.as_deref(), Some("3.12"));
    }

    #[test]
    fn empty_strings_are_treated_as_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_plist(
            dir.path(),
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>CFBundleName</key><string></string>
</dict></plist>"#,
        );
        let info = read_info_plist(&dir.path().join("Contents"))
            .expect("no debe fallar")
            .expect("debe existir");
        assert_eq!(info.bundle_name, None);
    }
}
