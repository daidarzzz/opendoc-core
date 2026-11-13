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

Tanto `opendoc` como `opendoc-cli` consumen este crate a través de etiquetas Git versionadas (`tag = "v0.2.0"`) para asegurar estabilidad y paridad exacta entre ambas herramientas.

## Uso

```toml
[dependencies]
opendoc-core = { git = "https://github.com/daidarzzz/opendoc-core", tag = "v0.2.0" }
```

```rust
use opendoc_core::library;
use opendoc_core::search::{search, SearchIndex};

use std::path::Path;

// Segundo argumento: carpeta de caché de los tarix (solo se reutiliza,
// nunca se extrae aquí; ver `library::LoadOptions`).
let lib = library::load(Path::new("/ruta/docsets"), Path::new("/ruta/tarix-cache"))?;
let mut index = SearchIndex::build_for_search(lib.entries);
// Reutiliza el mismo índice entre teclas: si la consulta amplía la
// anterior (`gr` → `grid`), solo se revisan sus coincidencias.
let hits = search(&mut index, "grid", None, 20);
```

### Importar DevDocs
`devdocs::convert` transforma un paquete de DevDocs (`index.json` + `db.json`) en un
`.docset` estándar, de modo que búsqueda, navegación y CLI lo tratan como cualquier otro.
Sanea las rutas, reescribe los enlaces internos, repara anclas y es atómico (ante un
fallo conserva el docset anterior). La descarga HTTP la hace quien lo consume.

```rust
use opendoc_core::devdocs::{convert, parse_catalog};
// `docs.json` → metadatos; `index.json` y `db.json` descargados por el llamador.
let meta = &parse_catalog(&docs_json)?[0];
let report = convert(&index_json, &db_json, meta, Path::new("/ruta/docsets/CSS.docset"))?;
```

### Rendimiento

La búsqueda recorre el índice en memoria, sin SQL. Al escribir, cada tecla que amplía la consulta solo revisa las coincidencias de la anterior, y con más de 40.000 candidatos el recorrido se reparte entre hilos (hasta 8). El resultado es idéntico al de un recorrido completo en serie; los tests lo comparan consulta a consulta, también con docsets reales.

Medido en release (Windows, entradas reales replicadas):

| Entradas | Construir índice | Peor tecla |
|---|---|---|
| 300.000 | 64 ms | 17 ms |
| 1.000.000 | 230 ms | 48 ms |

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
