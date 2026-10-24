# opendoc-core

Lógica pura de [OpenDoc](https://github.com/daidarzzz/opendoc), sin Tauri y
sin red:

- Escaneo de docsets de Dash/Zeal (`.docset`).
- Lectura de `Info.plist`.
- Índices SQLite `searchIndex` y Core Data (`ZTOKEN...`).
- Extracción tarix / `.tgz` con validación de rutas y topes.
- Búsqueda difusa en memoria con `nucleo-matcher`.
- Resolución segura de rutas dentro de `Documents/`.
- Ajustes persistentes y utilidades de disco.
- Carga compartida de una carpeta de docsets (`library::load`), incluida la
  reutilización de cachés tarix ya extraídas.

## Uso

```toml
[dependencies]
opendoc-core = { git = "https://github.com/daidarzzz/opendoc-core", tag = "v0.1.0" }
```

```rust
use opendoc_core::library;
use opendoc_core::search::{search, SearchIndex};

let lib = library::load(std::path::Path::new("/ruta/docsets"), None)?;
let mut index = SearchIndex::build_for_search(lib.entries);
let hits = search(&mut index, "grid", None, 20);
```

## Tests

```bash
cargo test
```

Los tests con fixtures reales (`CSS.docset`, `Python_3.docset`, `C++.docset`)
se saltan si no están disponibles. Puedes apuntar a una carpeta concreta:

```bash
# Windows
set OPENDOC_FIXTURES=C:\ruta\opendoc\src-tauri\tests\fixtures
# macOS/Linux
export OPENDOC_FIXTURES=/ruta/opendoc/src-tauri/tests/fixtures
```

## Licencia

GPL-3.0-or-later. Ver `LICENSE`.
