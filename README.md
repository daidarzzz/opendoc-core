# opendoc-core

<p>
  <img src="./logo-op3-black.svg" alt="opendoc-icon" width="200"/>
</p>

Núcleo compartido y biblioteca base para el ecosistema [OpenDoc](https://github.com/daidarzzz/opendoc) y [opendoc-cli](https://github.com/daidarzzz/opendoc-cli). Contiene la lógica pura del visor y buscador de documentación, sin dependencias de Tauri, sin interfaz gráfica y sin acceso a la red:

- Escaneo de docsets de Dash/Zeal (`.docset`).
- Lectura de `Info.plist` (metadatos, plataforma, página de inicio).
- Índices SQLite: esquema estándar (`searchIndex`) y Core Data (`ZTOKEN...`).
- Extracción de paquetes tarix / `.tgz` con validación estricta de rutas (prevención de path traversal, symlinks) y límites de descompresión.
- Búsqueda difusa en memoria ultrarrápida con `nucleo-matcher` (tiers: exacta > prefijo > difusa, con ponderación por tipo de símbolo).
- Navegación y conteo por tipos de símbolos (`browse`).
- Resolución segura de rutas dentro de `Documents/` (aislamiento contra escapes y normalización de enlaces).
- Carga centralizada de colecciones de docsets (`library::load`), incluyendo la detección y reutilización de cachés tarix ya extraídas.
- Persistencia tolerante de ajustes y utilidades seguras de disco.

## Relación con el ecosistema

- **`opendoc-core`**: este repositorio; provee los modelos de datos, escáner, índices, búsqueda y resolución de rutas.
- **`opendoc`**: la aplicación de escritorio principal (Tauri v2 + React + TypeScript), que añade el visor con `opendoc://` en `<iframe>` aislado, barra de pestañas, command palette, gestión de catálogo con descargas HTTP, temas y acentos de color.
- **`opendoc-cli`** (`odc`): cliente terminal que consume `opendoc-core` para buscar y abrir documentación en el navegador o visor sin levantar la interfaz de escritorio.

Tanto `opendoc` como `opendoc-cli` consumen este crate a través de etiquetas Git versionadas (`tag = "v0.1.1"`) para asegurar estabilidad y paridad exacta entre ambas herramientas.

## Uso

```toml
[dependencies]
opendoc-core = { git = "https://github.com/daidarzzz/opendoc-core", tag = "v0.1.1" }
```

```rust
use opendoc_core::library;
use opendoc_core::search::{search, SearchIndex};

let lib = library::load(std::path::Path::new("/ruta/docsets"), None)?;
let mut index = SearchIndex::build_for_search(lib.entries);
let hits = search(&mut index, "grid", None, 20);
```

## Pruebas

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

Los tests con fixtures reales (`CSS.docset`, `Python_3.docset`, `C++.docset`) se omiten automáticamente con un aviso si los archivos no están presentes. Se puede apuntar a una carpeta local de fixtures mediante la variable de entorno:

```bash
# Windows (PowerShell)
$env:OPENDOC_FIXTURES = "C:\ruta\opendoc\src-tauri\tests\fixtures"
# macOS/Linux
export OPENDOC_FIXTURES="/ruta/opendoc/src-tauri/tests/fixtures"
```

## Licencia

GNU General Public License v3.0 o posterior (GPL-3.0-or-later). Consulta `LICENSE`.
