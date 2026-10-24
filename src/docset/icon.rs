//! Icono del docset (`icon.png` / `icon@2x.png` en la raíz, guía de Dash).
//!
//! Se resuelve una sola vez en el escaneo y viaja como data-URL en
//! `Docset`/`PendingTarix`: los PNG de 16/32px pesan ~1-5 KB. Vale para
//! tarix pendientes e instalados (el icono vive fuera del `.tgz`, en la
//! raíz del `.docset`, y `root_path` se conserva tras instalar).
//! Validación: fichero regular, ≤256 KB, magia PNG e IHDR con dimensiones
//! ≤512x512. Lo demás → `None` (la UI usa un genérico).

use std::path::Path;

/// Tope de bytes del icono (un PNG de 32px ronda los pocos KB).
pub const MAX_ICON_BYTES: u64 = 256 * 1024;
/// Lado máximo en píxeles (según IHDR).
pub const MAX_ICON_DIM: u32 = 512;

/// Magia PNG (`89 50 4E 47 0D 0A 1A 0A`).
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Data-URL del icono de un `.docset` o `None` si no hay uno válido.
/// Prefiere `icon.png` sobre `icon@2x.png` (guía de Dash: 16px / 32px).
pub fn icon_data_url_for(root: &Path) -> Option<String> {
    read_icon_bytes(root).map(|bytes| {
        format!(
            "data:image/png;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
        )
    })
}

/// Bytes validados del icono (`icon.png`, si no `icon@2x.png`).
pub fn read_icon_bytes(root: &Path) -> Option<Vec<u8>> {
    for name in ["icon.png", "icon@2x.png"] {
        let path = root.join(name);
        if let Some(bytes) = read_validated(&path) {
            return Some(bytes);
        }
    }
    None
}

/// Lee y valida un candidato: fichero, tamaño, magia PNG y dimensiones.
fn read_validated(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ICON_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return None;
    }
    let (w, h) = png_dimensions(&bytes)?;
    if w == 0 || h == 0 || w > MAX_ICON_DIM || h > MAX_ICON_DIM {
        return None;
    }
    Some(bytes)
}

/// `(ancho, alto)` del IHDR o `None` si no es un PNG con cabecera sana.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // Firma (8) + longitud IHDR (4) + tipo "IHDR" (4) + datos (13: w + h +…).
    if bytes.len() < 33 || bytes[..8] != PNG_MAGIC {
        return None;
    }
    if &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PNG mínimo con las dimensiones dadas (firma + IHDR + IEND; el CRC
    /// no se comprueba al leer, solo la cabecera).
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&PNG_MAGIC);
        out.extend_from_slice(&13_u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&[8, 2, 0, 0, 0]); // 8bit RGB
        out.extend_from_slice(&0_u32.to_be_bytes()); // CRC (ficticio)
        out.extend_from_slice(&0_u32.to_be_bytes()); // IEND vacía
        out.extend_from_slice(b"IEND");
        out
    }

    fn write_icon(dir: &Path, name: &str, bytes: &[u8]) {
        std::fs::write(dir.join(name), bytes).expect("escribir icono");
    }

    #[test]
    fn prefers_icon_png_over_2x() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_icon(dir.path(), "icon.png", &png(16, 16));
        write_icon(dir.path(), "icon@2x.png", &png(32, 32));
        let url = icon_data_url_for(dir.path()).expect("icono");
        assert!(url.starts_with("data:image/png;base64,"));
        // Decodifica al de 16px (el preferido), no al de 32px.
        let raw = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            url.trim_start_matches("data:image/png;base64,"),
        )
        .expect("base64");
        assert_eq!(png_dimensions(&raw), Some((16, 16)));
    }

    #[test]
    fn falls_back_to_2x_and_rejects_garbage() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_icon(dir.path(), "icon@2x.png", &png(32, 32));
        assert!(icon_data_url_for(dir.path()).is_some());
        // Magia rota.
        let dir2 = tempfile::tempdir().expect("tempdir");
        write_icon(dir2.path(), "icon.png", b"no soy un png");
        assert!(icon_data_url_for(dir2.path()).is_none());
        // Dimensiones excesivas.
        let dir3 = tempfile::tempdir().expect("tempdir");
        write_icon(dir3.path(), "icon.png", &png(1024, 16));
        assert!(icon_data_url_for(dir3.path()).is_none());
        // Dimensión cero.
        let dir4 = tempfile::tempdir().expect("tempdir");
        write_icon(dir4.path(), "icon.png", &png(0, 32));
        assert!(icon_data_url_for(dir4.path()).is_none());
        // Sin iconos.
        let dir5 = tempfile::tempdir().expect("tempdir");
        assert!(icon_data_url_for(dir5.path()).is_none());
    }

    #[test]
    fn oversized_file_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut big = png(16, 16);
        big.resize(MAX_ICON_BYTES as usize + 1, 0);
        write_icon(dir.path(), "icon.png", &big);
        assert!(icon_data_url_for(dir.path()).is_none());
    }
}
