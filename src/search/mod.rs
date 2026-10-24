//! Búsqueda en memoria (`search/`).
//!
//! La búsqueda interactiva se hace aquí, no con `LIKE` en SQL (SPEC §4.2).

pub mod filter;
pub mod index;
pub mod query;

pub use filter::{parse_query, ParsedQuery};
pub use index::{IndexedEntry, SearchIndex};
pub use query::{search, SearchResult};
