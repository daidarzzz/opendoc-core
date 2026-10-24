//! Profiling opt-in de FASE 0 (solo diagnóstico, sin cambiar conducta).
//!
//! - Se activa con `OPENDOC_PROFILE=1` en el entorno al arrancar la app
//!   (dev o release). Sin la variable, silencio total.
//! - `mark()` solo formatea y escribe cuando está activo; desactivado es
//!   una lectura atómica (`OnceLock`) más los `Instant::now()` de los
//!   puntos de medida (coste despreciable, sin E/S ni locks).
//! - Los contadores de eventos son `fetch_add` incondicionales (una
//!   instrucción atómica por evento) y solo se leen al cerrar la
//!   operación; no alteran ningún flujo.
//! - Uso en GUI: `tauri dev` hereda el entorno del terminal; en release
//!   hay que lanzar el binario con la variable puesta (ver ayuda del
//!   comando o panel de control en Windows).

use std::fmt::Arguments;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};

/// `true` solo con `OPENDOC_PROFILE=1` (resto de valores = apagado).
static ENABLED: OnceLock<bool> = OnceLock::new();
/// Eventos `docset-progress` emitidos (se lee al cerrar la operación).
static EMITTED_EVENTS: AtomicU64 = AtomicU64::new(0);

/// ¿Está activo el profiling? (lectura única de entorno, cacheada).
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("OPENDOC_PROFILE")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

/// Marca temporal a stderr. `details` usa `format_args!` (perezoso: sin
/// coste cuando está apagado).
pub fn mark(phase: &str, details: Arguments<'_>) {
    if enabled() {
        eprintln!("[profile] phase={phase} {details}");
    }
}

/// Anota un evento de progreso emitido (contador global barato).
pub fn note_event() {
    EMITTED_EVENTS.fetch_add(1, Ordering::Relaxed);
}

/// Lee y reinicia el contador de eventos emitidos.
pub fn take_events() -> u64 {
    EMITTED_EVENTS.swap(0, Ordering::Relaxed)
}

/// Milisegundos entre dos instantes (formato de las marcas).
pub fn ms_since(start: std::time::Instant) -> u128 {
    start.elapsed().as_millis()
}
