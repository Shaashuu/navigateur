//! Fichiers JSON de Feather (paramètres, historique, favoris, session), tous
//! rangés dans le dossier de configuration du système.
//!
//! Ce module réunit ce que `settings.rs` et `library.rs` faisaient chacun de
//! leur côté (chemin du dossier, lecture, écriture sûre).

use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Serialize, de::DeserializeOwned};

/// Chemin d'un fichier du dossier de configuration
/// (par exemple `%APPDATA%\Feather\Feather\config\parametres.json` sur Windows).
pub fn path(name: &str) -> Option<PathBuf> {
    ProjectDirs::from("app", "Feather", "Feather").map(|d| d.config_dir().join(name))
}

/// Lit un fichier JSON ; `None` s'il manque ou s'il est illisible.
pub fn read_json<T: DeserializeOwned>(name: &str) -> Option<T> {
    let text = fs::read_to_string(path(name)?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Écrit d'abord un fichier temporaire puis le renomme, pour ne jamais
/// laisser un fichier à moitié écrit si le programme s'arrête.
/// `pretty` : JSON indenté, lisible à la main (paramètres).
pub fn write_json<T: Serialize>(name: &str, value: &T, pretty: bool) -> io::Result<()> {
    let path = path(name).ok_or_else(|| io::Error::other("dossier de configuration introuvable"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(tmp, path)
}
