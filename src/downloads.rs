//! Téléchargements : où les ranger, et les retrouver ensuite sur le disque.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Progress,
    Done,
    Failed,
}

/// Un téléchargement de cette session (la liste n'est pas gardée au redémarrage).
#[derive(Clone, Debug, Serialize)]
pub struct Download {
    pub id: u32,
    pub url: String,
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub state: State,
}

/// Le moteur propose un chemin dans le dossier Téléchargements du système. Si
/// l'utilisateur a choisi un autre dossier (qui existe), on y range le fichier,
/// sans écraser un fichier du même nom.
pub fn destination(suggested: &Path, chosen_dir: &str) -> PathBuf {
    let chosen = Path::new(chosen_dir);
    let dir = if !chosen_dir.is_empty() && chosen.is_dir() {
        chosen.to_path_buf()
    } else if suggested.is_absolute() && suggested.parent() != std::env::current_dir().ok().as_deref() {
        return suggested.to_path_buf();
    } else {
        // Sans dossier Téléchargements connu, le moteur propose le dossier courant
        // (souvent celui du programme) : on range plutôt le fichier dans ~/Downloads.
        match directories::UserDirs::new() {
            Some(dirs) => dirs
                .download_dir()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| dirs.home_dir().join("Downloads")),
            None => return suggested.to_path_buf(),
        }
    };
    let _ = std::fs::create_dir_all(&dir);
    let name = suggested
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "telechargement".to_string());
    unique(&dir, &name)
}

fn unique(dir: &Path, name: &str) -> PathBuf {
    let (base, ext) = match name.split_once('.') {
        Some((base, ext)) if !base.is_empty() => (base.to_string(), format!(".{ext}")),
        _ => (name.to_string(), String::new()),
    };
    let mut path = dir.join(name);
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{base} ({n}){ext}"));
        n += 1;
    }
    path
}

/// Montre le fichier dans le Finder ou l'Explorateur.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    if let Some(dir) = path.parent() {
        let _ = Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Ouvre le fichier avec l'application prévue par le système.
pub fn open(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(path).spawn();
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(path).spawn();
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dossier_choisi_et_nom_libre() {
        let dir = std::env::temp_dir().join(format!("feather-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.pdf"), b"").unwrap();
        let got = destination(Path::new("/x/Downloads/a.pdf"), dir.to_str().unwrap());
        assert_eq!(got, dir.join("a (1).pdf"));
        // Dossier vide ou introuvable : on garde la proposition du moteur.
        // (un chemin absolu sur tous les systèmes : sous Windows « /x » ne l'est pas)
        let suggested = std::env::temp_dir().join("feather-ailleurs").join("a.pdf");
        assert_eq!(destination(&suggested, ""), suggested);
        assert_eq!(destination(&suggested, "/nulle/part"), suggested);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
