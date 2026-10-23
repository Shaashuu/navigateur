//! Fichiers JSON de Feather (paramètres, historique, favoris, session), tous
//! rangés dans le dossier de configuration du système.
//!
//! Ce module réunit ce que `settings.rs` et `library.rs` faisaient chacun de
//! leur côté (chemin du dossier, lecture, écriture sûre).
//!
//! - L'écriture se fait dans un fil à part (`write_json` ne fait que mettre le
//!   texte en forme) : avant, réécrire 10 Mo de texte des pages figeait
//!   l'interface. `flush` attend la fin des écritures, avant de quitter.
//! - Chaque écriture garde la version précédente en `.bak`, et passe par un
//!   fichier temporaire vidé sur le disque (`sync_all`) avant d'être renommé.
//! - Un fichier illisible n'est plus écrasé : il est mis de côté
//!   (`.corrompu-<date>`), on reprend la copie `.bak`, et l'interface prévient
//!   (voir `take_warnings`).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

use directories::ProjectDirs;
use serde::{Serialize, de::DeserializeOwned};

/// Chemin d'un fichier du dossier de configuration
/// (par exemple `%APPDATA%\Feather\Feather\config\parametres.json` sur Windows).
pub fn path(name: &str) -> Option<PathBuf> {
    ProjectDirs::from("app", "Feather", "Feather").map(|d| d.config_dir().join(name))
}

/// Fichiers illisibles mis de côté depuis le lancement, pour l'interface.
static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Les avertissements pas encore montrés (fichiers abîmés mis de côté).
pub fn take_warnings() -> Vec<String> {
    WARNINGS.lock().map(|mut w| std::mem::take(&mut *w)).unwrap_or_default()
}

fn backup_of(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".bak");
    PathBuf::from(name)
}

/// Lit un fichier JSON ; `None` s'il manque ou s'il est illisible (la copie
/// `.bak` est alors essayée).
pub fn read_json<T: DeserializeOwned>(name: &str) -> Option<T> {
    read_json_checked(name).map(|(value, _)| value)
}

/// Comme `read_json`, en disant si la valeur vient du fichier lui-même (vrai)
/// ou de sa copie `.bak` (faux : fichier absent, abîmé ou illisible).
pub fn read_json_checked<T: DeserializeOwned>(name: &str) -> Option<(T, bool)> {
    read_at(&path(name)?, name)
}

fn read_at<T: DeserializeOwned>(path: &Path, name: &str) -> Option<(T, bool)> {
    // Une écriture en cours (fil d'écriture) doit être finie avant de relire.
    flush();
    let backup = backup_of(path);
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(value) => return Some((value, true)),
            Err(err) => {
                // Fichier abîmé (coupure de courant, format inconnu) : mis de côté,
                // jamais effacé, pour pouvoir le récupérer à la main.
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or_default();
                let mut aside = path.as_os_str().to_os_string();
                aside.push(format!(".corrompu-{secs}"));
                let _ = fs::rename(path, &aside);
                eprintln!("{name} illisible ({err}) : mis de côté");
                let restored = backup.is_file();
                if let Ok(mut w) = WARNINGS.lock() {
                    w.push(if restored {
                        format!("Le fichier {name} était abîmé : Feather a repris sa copie précédente.")
                    } else {
                        format!("Le fichier {name} était abîmé : il a été mis de côté dans le dossier de Feather.")
                    });
                }
            }
        },
        // Absent : peut-être un arrêt entre deux renommages ; la copie sert alors.
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    let text = fs::read_to_string(&backup).ok()?;
    serde_json::from_str(&text).ok().map(|value| (value, false))
}

/// Met la valeur en forme puis la confie au fil d'écriture (voir le module).
/// `pretty` : JSON indenté, lisible à la main (paramètres).
pub fn write_json<T: Serialize>(name: &str, value: &T, pretty: bool) -> io::Result<()> {
    let path = path(name).ok_or_else(|| io::Error::other("dossier de configuration introuvable"))?;
    let json = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(io::Error::other)?;
    let Some(tx) = writer() else { return write_now(&path, &json) };
    let tx = tx.lock().map_err(|_| io::Error::other("fil d'écriture arrêté"))?;
    match tx.send(Job::Write(path, json)) {
        Ok(()) => Ok(()),
        // Fil d'écriture arrêté : on écrit ici.
        Err(err) => match err.0 {
            Job::Write(path, json) => write_now(&path, &json),
            Job::Flush(_) => Ok(()),
        },
    }
}

/// Écriture sûre : fichier temporaire vidé sur le disque, ancienne version
/// gardée en `.bak`, puis renommage.
fn write_now(path: &Path, json: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    drop(file);
    if path.is_file() {
        let _ = fs::rename(path, backup_of(path));
    }
    fs::rename(tmp, path)
}

enum Job {
    Write(PathBuf, String),
    /// Prévient quand tout ce qui précède est écrit (voir `flush`).
    Flush(Sender<()>),
}

fn writer() -> Option<&'static Mutex<Sender<Job>>> {
    static WRITER: OnceLock<Option<Mutex<Sender<Job>>>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = channel::<Job>();
            std::thread::Builder::new()
                .name("ecriture".into())
                .spawn(move || {
                    for job in rx {
                        match job {
                            Job::Write(path, json) => {
                                if let Err(err) = write_now(&path, &json) {
                                    eprintln!("{} non enregistré : {err}", path.display());
                                }
                            }
                            Job::Flush(done) => {
                                let _ = done.send(());
                            }
                        }
                    }
                })
                .ok()
                .map(|_| Mutex::new(tx))
        })
        .as_ref()
}

/// Attend que tout ce qui a été confié au fil d'écriture soit sur le disque
/// (avant de quitter, de redémarrer, ou de relire un fichier).
pub fn flush() {
    let Some(tx) = writer() else { return };
    let (done, wait) = channel();
    let sent = tx.lock().map(|tx| tx.send(Job::Flush(done)).is_ok()).unwrap_or(false);
    if sent {
        let _ = wait.recv_timeout(std::time::Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fichier_abime_mis_de_cote() {
        let dir = std::env::temp_dir().join(format!("feather-stockage-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("essai.json");
        write_now(&file, "[1,2]").unwrap();
        write_now(&file, "[3]").unwrap();
        // L'ancienne version est gardée.
        assert_eq!(fs::read_to_string(backup_of(&file)).unwrap(), "[1,2]");
        assert_eq!(read_at::<Vec<u32>>(&file, "essai.json"), Some((vec![3], true)));
        // Fichier abîmé : mis de côté, la copie reprend le relais, un avertissement attend.
        fs::write(&file, "[3,").unwrap();
        assert_eq!(read_at::<Vec<u32>>(&file, "essai.json"), Some((vec![1, 2], false)));
        assert!(!file.exists());
        let aside = fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".corrompu-"));
        assert!(aside);
        assert!(take_warnings().iter().any(|w| w.contains("essai.json")));
        // Fichier absent (arrêt entre deux renommages) : la copie sert aussi.
        assert_eq!(read_at::<Vec<u32>>(&file, "essai.json"), Some((vec![1, 2], false)));
        fs::remove_dir_all(&dir).unwrap();
    }
}
