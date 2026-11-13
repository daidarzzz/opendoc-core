//! Shared offline documentation engine for OpenDoc and its CLI.
//!
//! Use [`library::load`] to discover docsets and read their symbol indexes,
//! [`search`] to query those entries, and [`resolve::resolve_request`] to map
//! a document request to a local file. [`devdocs::convert`] imports downloaded
//! DevDocs packages into the same docset format.
//!
//! All disk operations are synchronous. Desktop and server consumers should
//! run scans, imports and archive extraction on a blocking worker.
//!
//! ```no_run
//! use opendoc_core::library;
//! use opendoc_core::search::{search, SearchIndex};
//! use std::path::Path;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let library = library::load(Path::new("docsets"), Path::new("tarix-cache"))?;
//!     let mut index = SearchIndex::build_for_search(library.entries);
//!     let hits = search(&mut index, "grid", None, 20);
//!     println!("{} matching symbols", hits.len());
//!     Ok(())
//! }
//! ```

pub mod browse;
pub mod devdocs;
pub mod docset;
pub mod library;
pub mod persist;
pub mod profile;
pub mod resolve;
pub mod search;
pub mod settings;

#[doc(hidden)]
pub mod test_support;
