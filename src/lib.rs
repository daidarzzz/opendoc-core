//! Lógica pura de OpenDoc, sin Tauri y sin red.
//!
//! Este crate contiene el escaneo de docsets, la lectura de índices SQLite
//! (`searchIndex` y Core Data), la extracción tarix, la resolución segura
//! de rutas y la búsqueda difusa en memoria. La app Tauri y el CLI lo
//! consumen por igual.

pub mod browse;
pub mod docset;
pub mod library;
pub mod persist;
pub mod profile;
pub mod resolve;
pub mod search;
pub mod settings;

#[doc(hidden)]
pub mod test_support;
