//! Persistencia atómica compartida (`settings.json`, `session.json`).
//!
//! - Escritura atómica: temporal en el mismo directorio + rename. El
//!   `.json` final siempre parsea (o no se toca el anterior).
//! - `save_if_changed`: solo escribe si el valor cambió (compara el
//!   struct parseado del disco con el nuevo).
//! - Copias `.bak` rotativas: la primera corrupción va a `<base>.json.bak`
//!   y esa copia **nunca se sobrescribe** (conserva la más antigua); las
//!   siguientes usan `<base>-<epoch>.json.bak` con un máximo de 3 copias
//!   en total (primaria + 2 fechadas). Todo best-effort: nunca tumba al
//!   llamador por un fallo de disco al respaldar.
//!
//! Todo recibe la ruta por parámetro: testeable sin Tauri.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Máximo de copias `.bak` por fichero (primaria + fechadas).
pub const MAX_BACKUPS: usize = 3;

/// Escritura atómica: temporal en el mismo directorio + rename.
pub fn save_atomic_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Respalda el fichero a `.bak` sin pisar nunca la copia primaria.
/// Si algo falla, no hace nada: el llamador sigue con defaults frescos.
pub fn backup_rotating(path: &Path) {
    if std::fs::read(path).is_err() {
        return;
    }
    let primary = path.with_extension("json.bak");
    if !primary.exists() {
        let _ = std::fs::rename(path, &primary);
        return;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dated = dated_backup_path(path, stamp);
    let _ = std::fs::rename(path, &dated);
    prune_dated_backups(path);
}

/// Guarda si el valor cambió. Devuelve `true` si escribió. Si lo que hay
/// no parsea, lo respalda (rotativo) antes de sobrescribir.
pub fn save_if_changed_json<T>(path: &Path, value: &T) -> std::io::Result<bool>
where
    T: Serialize + DeserializeOwned + PartialEq,
{
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<T>(&bytes) {
            Ok(current) if &current == value => return Ok(false),
            Ok(_) => {}
            Err(_) => backup_rotating(path),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    save_atomic_json(path, value)?;
    Ok(true)
}

/// `<base>-<epoch>.json.bak` junto al fichero original.
fn dated_backup_path(path: &Path, stamp: u64) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    let name = format!("{stem}-{stamp}.json.bak");
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Recorta las copias fechadas a `MAX_BACKUPS - 1` (la primaria siempre se
/// conserva). Solo mira `<stem>-*.json.bak` del mismo directorio.
fn prune_dated_backups(path: &Path) {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => return,
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string();
    let prefix = format!("{stem}-");
    let mut dated: Vec<PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".json.bak") {
            dated.push(entry.path());
        }
    }
    dated.sort();
    let keep = MAX_BACKUPS.saturating_sub(1);
    if dated.len() > keep {
        for old in dated.iter().take(dated.len() - keep) {
            let _ = std::fs::remove_file(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
    struct Dummy {
        #[serde(default)]
        v: u32,
    }

    #[test]
    fn corrupt_then_corrupt_again_keeps_oldest_bak() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("thing.json");
        std::fs::write(&path, "{ roto 1").expect("escribir");
        backup_rotating(&path);
        let primary = path.with_extension("json.bak");
        assert_eq!(std::fs::read_to_string(&primary).expect("bak"), "{ roto 1");
        // Segunda corrupción: la primaria no se pisa.
        std::fs::write(&path, "{ roto 2").expect("escribir");
        backup_rotating(&path);
        assert_eq!(
            std::fs::read_to_string(&primary).expect("bak"),
            "{ roto 1",
            "la copia más antigua se conserva"
        );
        assert!(!path.exists(), "el corrupto se movió");
    }

    #[test]
    fn backups_are_capped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("thing.json");
        for i in 0..10 {
            std::fs::write(&path, format!("{{ roto {i} }}")).expect("escribir");
            backup_rotating(&path);
            // Misma marca de segundo: espera para nombres distintos.
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let count = std::fs::read_dir(dir.path())
            .expect("leer")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json.bak"))
            .count();
        assert!(count <= MAX_BACKUPS, "máximo {MAX_BACKUPS}, hay {count}");
    }

    #[test]
    fn save_only_when_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("thing.json");
        let value = Dummy { v: 1 };
        assert!(save_if_changed_json(&path, &value).expect("primera vez"));
        assert!(!save_if_changed_json(&path, &value).expect("igual no escribe"));
        assert!(save_if_changed_json(&path, &Dummy { v: 2 }).expect("cambio sí"));
    }

    #[test]
    fn save_backs_up_garbage_before_overwriting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("thing.json");
        std::fs::write(&path, "{ roto").expect("escribir");
        let value = Dummy { v: 1 };
        assert!(save_if_changed_json(&path, &value).expect("guardar"));
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bak")).expect("bak"),
            "{ roto"
        );
        let raw = std::fs::read_to_string(&path).expect("leer");
        assert!(serde_json::from_str::<Dummy>(&raw).is_ok());
    }
}
