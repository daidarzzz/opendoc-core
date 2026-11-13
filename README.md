# opendoc-core

<img src="./logo-op3-black.svg" alt="OpenDoc logo" width="200"/>

The shared Rust library behind [OpenDoc](https://github.com/daidarzzz/opendoc)
and [opendoc-cli](https://github.com/daidarzzz/opendoc-cli). It runs offline
and has no Tauri or GUI dependencies.

- Scans Dash and Zeal docsets and reads standard and Core Data SQLite indexes.
- Searches symbols in memory by exact match, prefix or fuzzy match.
- Groups symbols by kind for browsing.
- Reuses extracted tarix archives and validates paths during extraction.
- Resolves document requests within the docset's `Documents/` directory.
- Converts DevDocs packages into docsets using bytes supplied by the caller.

## Load and search docsets

```toml
[dependencies]
opendoc-core = { git = "https://github.com/daidarzzz/opendoc-core", tag = "v0.2.0" }
```

```rust
use opendoc_core::library;
use opendoc_core::search::{search, SearchIndex};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let library = library::load(Path::new("docsets"), Path::new("tarix-cache"))?;
    let mut index = SearchIndex::build_for_search(library.entries);
    let hits = search(&mut index, "grid", None, 20);
    println!("{} matching symbols", hits.len());
    Ok(())
}
```

`library::load` reuses existing tarix cache entries and reports archives
that still need extraction. Reuse the search index across queries so it
can narrow the previous candidate set as the user types.

## Import DevDocs

Use `devdocs::parse_catalog` to read `docs.json`, then pass the matching
metadata and the downloaded `index.json` and `db.json` bytes to
`devdocs::convert`. The converter writes a standard docset with
`Info.plist`, `docSet.dsidx` and HTML pages under `Documents/`.

## Performance

Search runs against the in-memory index. It can distribute large candidate
sets across threads while preserving the same result ordering as a serial
search. Previous release-mode measurements on Windows used replicated
real docset entries.

| Entries | Index build | Slowest keystroke |
| --- | --- | --- |
| 300,000 | 64 ms | 17 ms |
| 1,000,000 | 230 ms | 48 ms |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local checks and a source map.
Run `cargo doc --no-deps --open` to browse the public API.

## License

[GNU General Public License v3.0 or later](LICENSE).
