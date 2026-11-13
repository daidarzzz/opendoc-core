//! In-memory symbol search using exact, prefix and fuzzy matching.
//!
//! Reuse a [`SearchIndex`] across queries to benefit from scratch-buffer
//! reuse and incremental candidate filtering when a query is extended.

pub mod filter;
pub mod index;
pub mod query;

pub use filter::{parse_query, ParsedQuery};
pub use index::{IndexedEntry, SearchIndex};
pub use query::{search, SearchResult};
