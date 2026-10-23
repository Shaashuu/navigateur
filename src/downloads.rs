//! Téléchargements : où les ranger, et les retrouver ensuite sur le disque.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Progress,
    Done,
    Failed,
    /// Annulé par l'utilisateur : le fichier est supprimé dès que le moteur le libère.
    Cancelled,
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
    /// Octets déjà écrits sur le disque (mesurés chaque seconde).
    pub received: Option<u64>,
    /// Taille annoncée par le serveur, si on la connaît.
    pub total: Option<u64>,
    /// Vitesse moyenne depuis la mesure précédente, en octets par seconde.
    pub speed: Option<u64>,
    /// Moment de la dernière mesure.
    #[serde(skip)]
    probed: Option<Instant>,
}

impl Download {
    pub fn new(id: u32, url: String, path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| url.clone());
        Self { id, url, name, path, state: State::Progress, received: None, total: None, speed: None, probed: None }
    }

    /// Mesure la taille du fichier en cours d'écriture (chaque seconde, à
    /// l'heure `now`) et la vitesse depuis la mesure précédente.
    pub fn probe(&mut self, now: Instant) {
        let Some(size) = written_size(&self.path) else { return };
        if let (Some(before), Some(at)) = (self.received, self.probed) {
            self.speed = Some(rate(before, size, now.saturating_duration_since(at)));
        }
        self.received = Some(size);
        self.probed = Some(now);
    }
}

/// Octets par seconde, de `before` à `after` en `elapsed` : une mesure en
/// retard (programme occupé, ordinateur en veille) ne gonfle pas la vitesse.
pub fn rate(before: u64, after: u64, elapsed: Duration) -> u64 {
    let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX).max(1);
    let bytes = u128::from(after.saturating_sub(before)) * 1000 / u128::from(ms);
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// Taille du fichier en cours d'écriture. Selon le moteur, il est écrit sous
/// son nom final ou sous un nom provisoire à côté (« .crdownload » pour
/// WebView2, « .download » pour WebKit).
fn written_size(path: &Path) -> Option<u64> {
    let mut candidates = vec![path.to_path_buf()];
    for ext in ["crdownload", "download", "part"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".{ext}"));
        candidates.push(PathBuf::from(name));
    }
    candidates.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).max()
}

/// Taille annoncée par le serveur (requête HEAD, en arrière-plan). On n'envoie
/// pas les cookies du site : pour une page protégée, la taille reste inconnue.
pub fn content_length(url: &str) -> Option<u64> {
    if !crate::network::is_web(url) {
        return None;
    }
    let mut cmd = crate::network::command("curl");
    cmd.args(["-sIL", "--max-time", "10", "--proto", "=http,https", "--proto-redir", "=http,https"]).arg(url);
    let output = cmd.output().ok()?;
    // Après une redirection, c'est la dernière réponse qui compte.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<u64>().ok())?
        })
        .next_back()
        .filter(|n| *n > 0)
}

/// Supprime un fichier téléchargé (et son nom provisoire), après une annulation.
pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
    for ext in ["crdownload", "download", "part"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".{ext}"));
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
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

/// Extensions doubles gardées entières : « archive.tar.gz » → « archive (1).tar.gz ».
const DOUBLE_EXTENSIONS: [&str; 4] = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"];

pub fn unique(dir: &Path, name: &str) -> PathBuf {
    // On coupe au dernier point (et non au premier) : « Rapport v1.2.pdf »
    // devenait « Rapport v1 (1).2.pdf ».
    let lower = name.to_ascii_lowercase();
    let cut = DOUBLE_EXTENSIONS
        .iter()
        .find(|ext| lower.ends_with(*ext) && lower.len() > ext.len())
        .map(|ext| name.len() - ext.len())
        .or_else(|| name.rfind('.').filter(|&i| i > 0));
    let (base, ext) = match cut {
        Some(i) => (name[..i].to_string(), name[i..].to_string()),
        None => (name.to_string(), String::new()),
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
    let _ = Command::new(crate::network::system_program("open")).arg("-R").arg(path).spawn();
    // L'Explorateur attend /select,"chemin" : passé par `arg`, Rust entourait tout
    // l'argument de guillemets et un chemin avec des espaces n'était pas trouvé.
    #[cfg(windows)]
    let _ = {
        use std::os::windows::process::CommandExt;
        Command::new(crate::network::system_program("explorer")).raw_arg(format!("/select,\"{}\"", path.display())).spawn()
    };
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    if let Some(dir) = path.parent() {
        let _ = Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Ouvre le fichier avec l'application prévue par le système.
pub fn open(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new(crate::network::system_program("open")).arg(path).spawn();
    #[cfg(windows)]
    let _ = Command::new(crate::network::system_program("explorer")).arg(path).spawn();
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// macOS : marque un fichier téléchargé « venu d'Internet » (attribut de
/// quarantaine), comme le fait Safari. Sans lui, une application ou un .dmg
/// téléchargé avec Feather s'ouvrait sans le contrôle de Gatekeeper.
#[cfg(target_os = "macos")]
pub fn quarantine(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    // 0083 : téléchargé, à vérifier par Gatekeeper ; puis la date et l'application.
    let value = format!("0083;{secs:x};Feather;");
    let Ok(file) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return };
    // SAFETY : chaînes C valides, terminées par un zéro, pour la durée de l'appel.
    unsafe {
        libc::setxattr(
            file.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn quarantine(_path: &Path) {}

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

    #[test]
    fn progression_mesuree() {
        let dir = std::env::temp_dir().join(format!("feather-progres-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("film.mp4");
        let mut d = Download::new(1, "https://a.fr/film.mp4".into(), path.clone());
        assert_eq!(d.name, "film.mp4");
        let t = Instant::now();
        d.probe(t);
        assert_eq!(d.received, None);
        // Nom provisoire de WebView2.
        std::fs::write(dir.join("film.mp4.crdownload"), vec![0u8; 1000]).unwrap();
        d.probe(t + Duration::from_secs(1));
        assert_eq!((d.received, d.speed), (Some(1000), None));
        std::fs::write(dir.join("film.mp4.crdownload"), vec![0u8; 3000]).unwrap();
        d.probe(t + Duration::from_secs(2));
        assert_eq!((d.received, d.speed), (Some(3000), Some(2000)));
        // Mesure en retard : la vitesse reste comptée par seconde.
        std::fs::write(dir.join("film.mp4.crdownload"), vec![0u8; 7000]).unwrap();
        d.probe(t + Duration::from_secs(4));
        assert_eq!((d.received, d.speed), (Some(7000), Some(2000)));
        remove(&path);
        assert!(!dir.join("film.mp4.crdownload").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nom_libre_avec_points() {
        let dir = std::env::temp_dir().join(format!("feather-noms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["Rapport v1.2.pdf", "archive.tar.gz", ".bashrc", "LISEZMOI"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(unique(&dir, "Rapport v1.2.pdf"), dir.join("Rapport v1.2 (1).pdf"));
        assert_eq!(unique(&dir, "archive.tar.gz"), dir.join("archive (1).tar.gz"));
        assert_eq!(unique(&dir, ".bashrc"), dir.join(".bashrc (1)"));
        assert_eq!(unique(&dir, "LISEZMOI"), dir.join("LISEZMOI (1)"));
        assert_eq!(unique(&dir, "libre.txt"), dir.join("libre.txt"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
