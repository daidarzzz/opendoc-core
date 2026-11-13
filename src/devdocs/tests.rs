use super::*;
use crate::library;
use crate::resolve::resolve_request;
use serde_json::json;

fn meta() -> DevDocsMeta {
    DevDocsMeta {
        name: "Demo".into(),
        slug: "demo~1".into(),
        release: Some("1.2.3".into()),
        mtime: Some(1_700_000_000),
        attribution: Some("&copy; 2020 <b>Demo</b> authors.<br>MIT License".into()),
        ..DevDocsMeta::default()
    }
}

fn package() -> (Vec<u8>, Vec<u8>) {
    let index = json!({"entries": [
        {"name": "Home", "path": "index", "type": "Guide"},
        {"name": "grid", "path": "properties/grid", "type": "Layout"},
        {"name": "@media", "path": "at-rules/@media", "type": "At-rules"},
        {"name": "os.path.join", "path": "library/os#os.path.join", "type": "Files"},
        {"name": "ghost", "path": "library/os#nope", "type": "Files"},
    ]});
    let db = json!({
        "index": "<h1>Demo</h1><p><a href=\"properties/grid\">grid</a> <a href=\"at-rules/@media\">m</a> <a href=\"https://example.com\">ext</a></p>",
        "properties/grid": "<h1>`grid` property</h1><a href=\"../index\">home</a> <a href=\"grid-area\">dead</a> <a href=\"../library/os#module-os\">os</a>",
        "at-rules/@media": "<h1>@media</h1><a href=\"#top\">same</a>",
        "library/os": "<h1>os</h1><dl><dt id=\"os.path.join\">join</dt></dl><a href=\"#os.path.join\">j</a><a href=\"../index#nowhere\">n</a>",
        "../../escape": "<h1>escape</h1>",
        "CON": "<h1>reserved</h1>",
    });
    (index.to_string().into_bytes(), db.to_string().into_bytes())
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join("Contents/Resources/Documents").join(rel)).expect("page")
}

#[test]
fn converts_and_loads_as_regular_docset() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("docsets/Demo.docset");
    let (index, db) = package();
    let report = convert(&index, &db, &meta(), &dest).expect("convert");

    assert_eq!(report.pages, 6);
    assert_eq!(report.entries, 5);
    assert_eq!(report.entries_dropped, 0);
    assert_eq!(report.entry_anchors_missing, 1); // library/os#nope
    assert_eq!(report.anchors_synthesized, 1); // #module-os
    assert_eq!(report.links_dead, 1); // grid-area
    assert!(report.links_internal >= 4);

    // Sin restos temporales junto al destino.
    let leftovers: Vec<_> = fs::read_dir(dest.parent().expect("parent"))
        .expect("read")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, ["Demo.docset"]);

    // El core lo trata como un docset más.
    let lib = library::load(&tmp.path().join("docsets"), &tmp.path().join("cache")).expect("load");
    assert!(lib.issues.is_empty(), "{:?}", lib.issues);
    assert_eq!(lib.docsets.len(), 1);
    let d = &lib.docsets[0];
    assert_eq!(d.name, "Demo");
    assert_eq!(d.version.as_deref(), Some("1.2.3"));
    assert_eq!(d.home_path.as_deref(), Some("index.html"));
    assert_eq!(lib.entries.len(), 5);
    assert!(lib.entries.iter().any(|e| e.kind == "Layout"));

    // Todas las entradas resuelven a un fichero real.
    let docs = d.contents_path.join("Resources/Documents");
    for e in &lib.entries {
        resolve_request(&docs, &e.path).unwrap_or_else(|err| panic!("{}: {err}", e.path));
    }
}

#[test]
fn links_are_rewritten_to_real_files() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("Demo.docset");
    let (index, db) = package();
    convert(&index, &db, &meta(), &dest).expect("convert");

    let home = read(&dest, "index.html");
    assert!(home.contains("href=\"properties/grid.html\""), "{home}");
    assert!(home.contains("href=\"at-rules/@media.html\""), "{home}");
    assert!(home.contains("href=\"https://example.com\""));
    assert!(home.contains("<title>Demo</title>"));
    assert!(home.contains("href=\"_opendoc-devdocs.css\""));

    let grid = read(&dest, "properties/grid.html");
    assert!(grid.contains("href=\"../index.html\""), "{grid}");
    assert!(
        grid.contains("href=\"../library/os.html#module-os\""),
        "{grid}"
    );
    assert!(
        grid.contains("href=\"#\""),
        "dead link must be disabled: {grid}"
    );
    assert!(
        grid.contains("<h1>grid property</h1>"),
        "backticks removed: {grid}"
    );
    assert!(grid.contains("href=\"../_opendoc-devdocs.css\""));

    // El ancla de cabecera perdida se recrea en la página destino.
    let os = read(&dest, "library/os.html");
    assert!(os.contains("<span id=\"module-os\"></span>"), "{os}");
}

#[test]
fn info_plist_carries_provenance() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("Demo.docset");
    let (index, db) = package();
    convert(&index, &db, &meta(), &dest).expect("convert");
    let value = plist::Value::from_file(dest.join("Contents/Info.plist")).expect("plist");
    let dict = value.into_dictionary().expect("dict");
    assert_eq!(dict["OpenDocSource"].as_string(), Some("devdocs"));
    assert_eq!(dict["OpenDocDevDocsSlug"].as_string(), Some("demo~1"));
    assert_eq!(
        dict["OpenDocDevDocsMtime"].as_unsigned_integer(),
        Some(1_700_000_000)
    );
    assert_eq!(installed_mtime(&dest), Some(1_700_000_000));
    assert_eq!(installed_mtime(tmp.path()), None);
    assert_eq!(
        dict["OpenDocAttribution"].as_string(),
        Some("© 2020 Demo authors. MIT License")
    );
}

#[test]
fn hostile_keys_cannot_escape_the_docset() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("out/Demo.docset");
    let (index, db) = package();
    convert(&index, &db, &meta(), &dest).expect("convert");
    // Nada fuera de `out/Demo.docset` (ni `escape.html` junto al destino).
    assert!(!tmp.path().join("escape.html").exists());
    assert!(!tmp.path().join("out/escape.html").exists());
    let mut files = Vec::new();
    collect(&dest, &mut files);
    assert!(files.iter().all(|f| f.starts_with(&dest)));
    assert!(files.iter().any(|f| f.ends_with("_/_/escape.html")));
    assert!(files.iter().any(|f| f.ends_with("_CON.html")));
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).expect("dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn invalid_packages_are_rejected_without_leftovers() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("Demo.docset");
    let ok_index = json!({"entries": [{"name": "a", "path": "a", "type": "T"}]}).to_string();
    let ok_db = json!({"a": "<h1>a</h1>"}).to_string();

    assert!(matches!(
        convert(b"nope", ok_db.as_bytes(), &meta(), &dest),
        Err(DevDocsError::InvalidIndex(_))
    ));
    assert!(matches!(
        convert(ok_index.as_bytes(), b"[]", &meta(), &dest),
        Err(DevDocsError::InvalidDb(_))
    ));
    assert!(matches!(
        convert(ok_index.as_bytes(), b"{}", &meta(), &dest),
        Err(DevDocsError::Empty("pages"))
    ));
    let none = json!({"entries": []}).to_string();
    assert!(matches!(
        convert(none.as_bytes(), ok_db.as_bytes(), &meta(), &dest),
        Err(DevDocsError::Empty("index entries"))
    ));
    // 1 de 2 entradas sin página (50 %) → paquete incompleto.
    let half = json!({"entries": [
        {"name": "a", "path": "a", "type": "T"},
        {"name": "b", "path": "missing", "type": "T"},
    ]})
    .to_string();
    assert!(matches!(
        convert(half.as_bytes(), ok_db.as_bytes(), &meta(), &dest),
        Err(DevDocsError::TooManyMissing {
            dropped: 1,
            total: 2
        })
    ));
    let mut bad = meta();
    bad.slug = "../evil".into();
    assert!(matches!(
        convert(ok_index.as_bytes(), ok_db.as_bytes(), &bad, &dest),
        Err(DevDocsError::InvalidSlug(_))
    ));
    assert!(matches!(
        convert(
            ok_index.as_bytes(),
            ok_db.as_bytes(),
            &meta(),
            &tmp.path().join("plain")
        ),
        Err(DevDocsError::InvalidDest(_))
    ));
    // Nada se creó.
    assert_eq!(fs::read_dir(tmp.path()).expect("read").count(), 0);
}

#[test]
fn failed_conversion_keeps_previous_docset() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("Demo.docset");
    let (index, db) = package();
    convert(&index, &db, &meta(), &dest).expect("first");
    let before = read(&dest, "index.html");
    let bad = convert(b"{", b"{}", &meta(), &dest);
    assert!(bad.is_err());
    assert_eq!(read(&dest, "index.html"), before);
}

#[test]
fn reinstall_replaces_atomically() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("Demo.docset");
    let (index, db) = package();
    convert(&index, &db, &meta(), &dest).expect("first");
    let db2 = json!({"index": "<h1>v2</h1>"}).to_string();
    let index2 =
        json!({"entries": [{"name": "Home", "path": "index", "type": "Guide"}]}).to_string();
    let report = convert(index2.as_bytes(), db2.as_bytes(), &meta(), &dest).expect("second");
    assert_eq!(report.pages, 1);
    assert!(read(&dest, "index.html").contains("v2"));
    assert!(!dest.with_extension("docset.old").exists());
}

#[test]
fn catalog_parses_real_shape() {
    let docs = br#"[{"name":"Python","slug":"python~3.12","type":"sphinx","version":"3.12","release":"3.12.9","mtime":1741250406,"db_size":18129117,"attribution":"x","links":{"home":"h"}},
                    {"name":"Angular","slug":"angular","version":"","mtime":1}]"#;
    let list = parse_catalog(docs).expect("catalog");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].slug, "python~3.12");
    assert_eq!(list[0].db_size, Some(18_129_117));
    assert_eq!(list[1].attribution, None);
    assert_eq!(
        index_url("python~3.12"),
        "https://documents.devdocs.io/python~3.12/index.json"
    );
    assert!(parse_catalog(b"{}").is_err());
}

#[test]
fn equivalent_anchors_are_repaired_in_links_and_index() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("Demo.docset");
    let index = json!({"entries": [
        {"name": "cast", "path": "api/cast#polars.DataFrame.cast", "type": "Method"},
        {"name": "intro", "path": "intro#Introduction", "type": "Guide"},
    ]})
    .to_string();
    let db = json!({
        "index": "<h1>i</h1><a href=\"api/cast#polars.DataFrame.cast\">c</a><a href=\"intro#Introduction\">i</a><a href=\"intro#unknown\">u</a>",
        "api/cast": "<h1 id=\"polars-dataframe-cast\">cast</h1><a href=\"#polars.DataFrame.cast\">self</a>",
        "intro": "<h1 id=\"Introduction-2\">Intro</h1>",
    })
    .to_string();
    let report = convert(index.as_bytes(), db.as_bytes(), &meta(), &dest).expect("convert");
    assert_eq!(report.entry_anchors_missing, 0);
    assert_eq!(report.links_anchor_missing, 1); // solo `#unknown`
    let home = read(&dest, "index.html");
    assert!(
        home.contains("href=\"api/cast.html#polars-dataframe-cast\""),
        "{home}"
    );
    assert!(
        home.contains("href=\"intro.html#Introduction-2\""),
        "{home}"
    );
    assert!(home.contains("href=\"intro.html#unknown\""), "{home}");
    assert!(read(&dest, "api/cast.html").contains("href=\"#polars-dataframe-cast\""));
    let lib = library::load(tmp.path(), &tmp.path().join("c")).expect("load");
    assert!(lib
        .entries
        .iter()
        .any(|e| e.path == "api/cast.html#polars-dataframe-cast"));
}

#[test]
fn malformed_html_never_panics() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dest = tmp.path().join("Demo.docset");
    let index = json!({"entries": [{"name": "é", "path": "ñ/ü", "type": ""}]}).to_string();
    let weird = [
        "<a href=\"ñ%é%\">x</a><a href=\"%",
        "<a",
        "<a href",
        "<a href=",
        "<a href=\"unterminated",
        "<a  href = 'ü' / = >é</a><h1>`x",
        "<a href=\"#%E0%A4%A\">z</a><h1></h1>",
        "日本語<a href=\"../../../..//../x\">y</a>",
    ];
    let mut db = serde_json::Map::new();
    db.insert("ñ/ü".into(), json!("<h1>ok</h1>"));
    for (i, w) in weird.iter().enumerate() {
        db.insert(format!("w{i}"), json!(w));
    }
    let db = serde_json::Value::Object(db).to_string();
    let report = convert(index.as_bytes(), db.as_bytes(), &meta(), &dest).expect("convert");
    assert_eq!(report.pages, weird.len() + 1);
    let lib = library::load(tmp.path(), &tmp.path().join("c")).expect("load");
    assert!(lib.issues.is_empty());
    assert_eq!(lib.entries[0].kind, "Guide"); // tipo vacío → Guide
}
