//! Mise à jour intégrée : on regarde la dernière version publiée sur GitHub
//! (dépôt public `feather-releases`), on télécharge le fichier de notre système,
//! on vérifie son empreinte, puis on remplace le programme. Le remplacement ne
//! prend effet qu'au redémarrage, quand l'utilisateur clique sur « Redémarrer ».
//!
//! Le réseau passe par `curl`, présent d'origine sous Windows 10/11 et macOS :
//! pas de bibliothèque HTTP de plus dans Feather.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Version de ce programme.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const REPO: &str = "Shaashuu/feather-releases";
const DOWNLOAD_PREFIX: &str = "https://github.com/Shaashuu/feather-releases/releases/download/";

/// Une version plus récente, prête à être téléchargée.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Available {
    pub version: String,
    /// Adresse du fichier pour ce système (vide si aucun fichier ne convient).
    pub url: String,
    pub name: String,
    pub size: u64,
    /// Empreinte SHA-256 annoncée par GitHub, en hexadécimal.
    pub sha256: Option<String>,
    /// Page de la release, pour la lire ou télécharger à la main.
    pub page: String,
}

/// Programme à relancer pour finir la mise à jour.
#[derive(Debug)]
pub struct Restart {
    program: PathBuf,
    args: Vec<OsString>,
}

impl Restart {
    pub fn launch(self) -> io::Result<()> {
        let mut cmd = Command::new(self.program);
        cmd.args(self.args);
        hide_window(&mut cmd);
        cmd.spawn().map(|_| ())
    }
}

// --- Versions -------------------------------------------------------------

/// « v0.1.5 » ou « 0.1.5 » en trois nombres.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(k)) => c > k,
        _ => false,
    }
}

// --- Vérification ---------------------------------------------------------

/// Nom du fichier à télécharger pour ce système, par exemple `-windows.exe`.
fn asset_suffix() -> Option<&'static str> {
    if cfg!(windows) {
        Some("-windows.exe")
    } else if cfg!(target_os = "macos") {
        Some("-macos.dmg")
    } else {
        None
    }
}

/// Lit la réponse de GitHub (`releases/latest`). `None` si la version n'est pas
/// plus récente que `current`.
pub fn pick_release(release: &Value, current: &str, suffix: Option<&str>) -> Option<Available> {
    let tag = release.get("tag_name")?.as_str()?;
    if !is_newer(tag, current) {
        return None;
    }
    let version = tag.trim_start_matches('v').to_string();
    let page = release
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://github.com/"))
        .unwrap_or("https://github.com/Shaashuu/feather-releases/releases")
        .to_string();
    let asset = suffix.and_then(|suffix| {
        release
            .get("assets")?
            .as_array()?
            .iter()
            .find(|a| {
                let name = a.get("name").and_then(Value::as_str).unwrap_or("");
                name.ends_with(suffix) && name.starts_with("Feather-")
            })
    });
    let text = |key| asset.and_then(|a| a.get(key)).and_then(Value::as_str);
    let url = text("browser_download_url")
        .filter(|u| u.starts_with(DOWNLOAD_PREFIX))
        .unwrap_or("")
        .to_string();
    let sha256 = text("digest")
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|d| d.len() == 64 && d.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_lowercase);
    Some(Available {
        version,
        name: text("name").unwrap_or("").to_string(),
        size: asset
            .and_then(|a| a.get("size"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        url,
        sha256,
        page,
    })
}

/// Demande à GitHub la dernière version. `Ok(None)` : on est à jour.
pub fn check() -> Result<Option<Available>, String> {
    let url = api_url();
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sSL",
        "--fail",
        "--max-time",
        "20",
        "--proto",
        proto(),
        "-H",
        "Accept: application/vnd.github+json",
    ])
    .arg(&url);
    let output = run(cmd, "la vérification")?;
    let release: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "réponse inattendue du serveur de mises à jour".to_string())?;
    Ok(pick_release(&release, CURRENT, asset_suffix()))
}

/// Seul https est accepté ; les essais en développement peuvent viser un serveur local.
fn proto() -> &'static str {
    if cfg!(debug_assertions) && std::env::var("FEATHER_UPDATE_API").is_ok() {
        "=http,https"
    } else {
        "=https"
    }
}

fn api_url() -> String {
    // Pour les essais, une version de développement peut viser un autre serveur.
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("FEATHER_UPDATE_API") {
        return url;
    }
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

// --- Installation ---------------------------------------------------------

/// Télécharge et installe la mise à jour. Le programme en cours n'est pas
/// arrêté : il faut ensuite appeler `Restart::launch` puis quitter.
pub fn install(update: &Available) -> Result<Restart, String> {
    if asset_suffix().is_none() {
        return Err("La mise à jour automatique n'existe pas sur ce système.".into());
    }
    #[cfg(debug_assertions)]
    let allowed = update.url.starts_with(DOWNLOAD_PREFIX) || std::env::var("FEATHER_UPDATE_API").is_ok();
    #[cfg(not(debug_assertions))]
    let allowed = update.url.starts_with(DOWNLOAD_PREFIX);
    if update.url.is_empty() || !allowed {
        return Err("Aucun fichier pour ce système dans cette version.".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("programme introuvable : {e}"))?;
    install_for_os(update, &exe)
}

#[cfg(windows)]
fn install_for_os(update: &Available, exe: &Path) -> Result<Restart, String> {
    let dir = exe.parent().ok_or("dossier introuvable")?;
    let new = dir.join("feather-update.exe");
    let result = (|| {
        download(&update.url, &new, update.sha256.as_deref())?;
        swap_executable(exe, &new).map_err(|e| {
            format!("impossible de remplacer le programme ({e}). Téléchargez-le depuis la page de la version.")
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&new);
    }
    result?;
    Ok(Restart {
        program: exe.to_path_buf(),
        args: vec![],
    })
}

#[cfg(target_os = "macos")]
fn install_for_os(update: &Available, exe: &Path) -> Result<Restart, String> {
    // …/Feather.app/Contents/MacOS/feather
    let bundle = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or("Feather n'est pas dans une application (.app) : mise à jour impossible.")?
        .to_path_buf();
    let parent = bundle.parent().ok_or("dossier introuvable")?;
    let work = std::env::temp_dir().join(format!("feather-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("dossier temporaire : {e}"))?;
    let dmg = work.join("Feather.dmg");
    let mount = work.join("volume");
    let new_bundle = parent.join("Feather.app.new");
    let result = (|| {
        download(&update.url, &dmg, update.sha256.as_deref())?;
        fs::create_dir_all(&mount).map_err(|e| e.to_string())?;
        let mut attach = Command::new("hdiutil");
        attach
            .args(["attach", "-nobrowse", "-readonly", "-quiet", "-mountpoint"])
            .arg(&mount)
            .arg(&dmg);
        run(attach, "l'ouverture de l'image disque")?;
        let copy = (|| {
            let mut ditto = Command::new("ditto");
            ditto.arg(mount.join("Feather.app")).arg(&new_bundle);
            run(ditto, "la copie de l'application")?;
            // Un fichier téléchargé par Feather n'est pas en quarantaine, mais au cas où.
            let mut xattr = Command::new("xattr");
            xattr.args(["-dr", "com.apple.quarantine"]).arg(&new_bundle);
            let _ = run(xattr, "xattr");
            swap_executable(&bundle, &new_bundle).map_err(|e| {
                format!("impossible de remplacer l'application ({e}). Copiez-la à la main depuis la page de la version.")
            })
        })();
        let mut detach = Command::new("hdiutil");
        detach.args(["detach", "-quiet", "-force"]).arg(&mount);
        let _ = run(detach, "la fermeture de l'image disque");
        copy
    })();
    let _ = fs::remove_dir_all(&work);
    if result.is_err() {
        let _ = fs::remove_dir_all(&new_bundle);
    }
    result?;
    Ok(Restart {
        program: PathBuf::from("open"),
        args: vec!["-n".into(), bundle.into_os_string()],
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn install_for_os(_update: &Available, _exe: &Path) -> Result<Restart, String> {
    Err("La mise à jour automatique n'existe pas sur ce système.".into())
}

/// Met `new` à la place de `current` : l'ancien est mis de côté (`.old`), car un
/// programme en cours d'exécution peut être renommé mais pas supprimé.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn swap_executable(current: &Path, new: &Path) -> io::Result<()> {
    let old = old_path(current);
    remove_any(&old)?;
    fs::rename(current, &old)?;
    if let Err(err) = fs::rename(new, current) {
        let _ = fs::rename(&old, current); // on remet l'ancien
        return Err(err);
    }
    Ok(())
}

fn old_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".old");
    PathBuf::from(name)
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Au démarrage : efface l'ancienne version laissée par la dernière mise à jour.
pub fn clean_leftovers() {
    let Ok(exe) = std::env::current_exe() else { return };
    #[cfg(target_os = "macos")]
    let target = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
        .unwrap_or(exe);
    #[cfg(not(target_os = "macos"))]
    let target = exe;
    let _ = remove_any(&old_path(&target));
    if let Some(dir) = target.parent() {
        let _ = remove_any(&dir.join("Feather.app.new"));
        let _ = remove_any(&dir.join("feather-update.exe"));
    }
}

// --- Téléchargement -------------------------------------------------------

#[cfg(any(windows, target_os = "macos"))]
fn download(url: &str, dest: &Path, sha256: Option<&str>) -> Result<(), String> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sSL",
        "--fail",
        "--max-time",
        "300",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "-o",
    ])
    .arg(dest)
    .arg(url);
    run(cmd, "le téléchargement")?;
    let size = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if size < 100_000 {
        return Err("le fichier téléchargé est trop petit".into());
    }
    match sha256 {
        Some(expected) if file_sha256(dest).map_err(|e| e.to_string())? != expected => {
            Err("le fichier téléchargé est corrompu (empreinte différente)".into())
        }
        _ => Ok(()),
    }
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn file_sha256(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn run(mut cmd: Command, what: &str) -> Result<Output, String> {
    hide_window(&mut cmd);
    let output = cmd
        .output()
        .map_err(|_| format!("{what} a échoué (outil système introuvable)"))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!("{what} a échoué (pas de connexion ?)"))
    }
}

/// Sous Windows, évite qu'une fenêtre de console clignote.
fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.1.5"), Some((0, 1, 5)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        assert_eq!(parse_version("0.1"), None);
        assert_eq!(parse_version("0.1.5-beta"), None);
        assert!(is_newer("v0.1.5", "0.1.4"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.1.4", "0.1.4"));
        assert!(!is_newer("0.1.3", "0.1.4"));
        assert!(!is_newer("n'importe quoi", "0.1.4"));
    }

    fn release() -> Value {
        let base = "https://github.com/Shaashuu/feather-releases/releases/download/v0.1.5";
        json!({
            "tag_name": "v0.1.5",
            "html_url": "https://github.com/Shaashuu/feather-releases/releases/tag/v0.1.5",
            "assets": [
                { "name": "Feather-0.1.5-macos.dmg", "size": 1000,
                  "browser_download_url": format!("{base}/Feather-0.1.5-macos.dmg"),
                  "digest": format!("sha256:{}", "ab".repeat(32)) },
                { "name": "Feather-0.1.5-windows.exe", "size": 2000,
                  "browser_download_url": format!("{base}/Feather-0.1.5-windows.exe"),
                  "digest": null }
            ]
        })
    }

    #[test]
    fn choix_du_fichier() {
        let mac = pick_release(&release(), "0.1.4", Some("-macos.dmg")).unwrap();
        assert_eq!(mac.version, "0.1.5");
        assert_eq!(mac.name, "Feather-0.1.5-macos.dmg");
        assert_eq!(mac.size, 1000);
        assert_eq!(mac.sha256, Some("ab".repeat(32)));
        let win = pick_release(&release(), "0.1.4", Some("-windows.exe")).unwrap();
        assert!(win.url.ends_with("Feather-0.1.5-windows.exe"));
        assert_eq!(win.sha256, None);
        // Pas de système pris en charge : on connaît la version mais pas de fichier.
        assert_eq!(pick_release(&release(), "0.1.4", None).unwrap().url, "");
        // À jour.
        assert_eq!(pick_release(&release(), "0.1.5", Some("-macos.dmg")), None);
    }

    #[test]
    fn adresse_de_telechargement_douteuse() {
        let mut release = release();
        release["assets"][0]["browser_download_url"] = json!("https://exemple.fr/Feather-0.1.5-macos.dmg");
        let found = pick_release(&release, "0.1.4", Some("-macos.dmg")).unwrap();
        assert_eq!(found.url, "");
    }

    #[test]
    fn remplacement_du_programme() {
        let dir = std::env::temp_dir().join(format!("feather-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let current = dir.join("feather");
        let new = dir.join("feather-update");
        fs::write(&current, "ancien").unwrap();
        fs::write(&new, "nouveau").unwrap();
        // Un reste d'une mise à jour précédente ne gêne pas.
        fs::write(old_path(&current), "très ancien").unwrap();
        swap_executable(&current, &new).unwrap();
        assert_eq!(fs::read_to_string(&current).unwrap(), "nouveau");
        assert_eq!(fs::read_to_string(old_path(&current)).unwrap(), "ancien");
        assert!(!new.exists());
        // Échec : l'ancien est remis en place.
        assert!(swap_executable(&current, &dir.join("absent")).is_err());
        assert_eq!(fs::read_to_string(&current).unwrap(), "nouveau");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empreinte() {
        let path = std::env::temp_dir().join(format!("feather-sha-{}", std::process::id()));
        fs::write(&path, "abc").unwrap();
        assert_eq!(
            file_sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::remove_file(&path).unwrap();
    }
}
