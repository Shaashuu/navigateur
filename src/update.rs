//! Mise à jour intégrée : on regarde la dernière version publiée sur GitHub
//! (dépôt public `feather-releases`), on télécharge le fichier de notre système,
//! on vérifie son empreinte et sa signature, puis on remplace le programme. Le remplacement ne
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

/// Clé publique Ed25519 (base64) qui signe les fichiers publiés : la clé secrète
/// n'existe que dans le secret GitHub `FEATHER_SIGNATURE` (voir
/// `outils/cle-signature.py` et build.yml). L'empreinte annoncée par GitHub
/// prouve seulement que le fichier n'est pas abîmé ; la signature prouve qu'il
/// vient bien de nous, même si le compte GitHub ou un jeton était volé.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
const PUBLIC_KEY: &str = "WOIfoSPOb+EFKvrVFylv+Hfa8i+31x62yhvXatPi7Gc=";

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
    /// Adresse de la signature du fichier (`<fichier>.sig`), vide s'il n'y en a pas.
    pub signature_url: String,
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
    /// Ne consomme pas `self` : si le lancement échoue, on peut réessayer.
    pub fn launch(&self) -> io::Result<()> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
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
            // Le nom porte la version : un fichier signé d'une ancienne version ne
            // peut pas passer pour la nouvelle.
            .find(|a| a.get("name").and_then(Value::as_str) == Some(&format!("Feather-{version}{suffix}")))
    });
    let text = |key| asset.and_then(|a| a.get(key)).and_then(Value::as_str);
    let url = text("browser_download_url")
        .filter(|u| u.starts_with(DOWNLOAD_PREFIX))
        .unwrap_or("")
        .to_string();
    // La signature est publiée à côté du fichier, sous le même nom suivi de « .sig ».
    let signature_url = text("name")
        .and_then(|name| {
            release.get("assets")?.as_array()?.iter().find(|a| {
                a.get("name").and_then(Value::as_str) == Some(&format!("{name}.sig"))
            })
        })
        .and_then(|a| a.get("browser_download_url")?.as_str())
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
        signature_url,
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
        "--proto-redir",
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
    // Sans empreinte annoncée, rien ne garantit que le fichier est bien celui
    // publié : on refuse plutôt que d'installer un programme non vérifié.
    let Some(sha256) = update.sha256.as_deref() else {
        return Err("Cette version n'annonce pas d'empreinte : installez-la depuis la page de la version.".into());
    };
    if update.signature_url.is_empty() {
        return Err("Cette version n'est pas signée : installez-la depuis la page de la version.".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("programme introuvable : {e}"))?;
    install_for_os(update, sha256, &exe)
}

/// Fichier où la mise à jour est téléchargée : sa taille, mesurée chaque
/// seconde, donne la progression affichée dans les paramètres.
pub fn download_path() -> Option<PathBuf> {
    if cfg!(windows) {
        Some(std::env::current_exe().ok()?.parent()?.join("feather-update.exe"))
    } else if cfg!(target_os = "macos") {
        Some(work_dir().join("Feather.dmg"))
    } else {
        None
    }
}

/// Dossier temporaire de la mise à jour sous macOS (image disque et volume monté).
fn work_dir() -> PathBuf {
    std::env::temp_dir().join(format!("feather-update-{}", std::process::id()))
}

#[cfg(windows)]
fn install_for_os(update: &Available, sha256: &str, exe: &Path) -> Result<Restart, String> {
    let new = download_path().ok_or("dossier introuvable")?;
    let result = (|| {
        download(update, &new, sha256)?;
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
fn install_for_os(update: &Available, sha256: &str, exe: &Path) -> Result<Restart, String> {
    // …/Feather.app/Contents/MacOS/feather
    let bundle = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or("Feather n'est pas dans une application (.app) : mise à jour impossible.")?
        .to_path_buf();
    let parent = bundle.parent().ok_or("dossier introuvable")?;
    let work = work_dir();
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("dossier temporaire : {e}"))?;
    let dmg = download_path().ok_or("dossier introuvable")?;
    let mount = work.join("volume");
    let new_bundle = parent.join("Feather.app.new");
    let result = (|| {
        download(update, &dmg, sha256)?;
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
fn install_for_os(_update: &Available, _sha256: &str, _exe: &Path) -> Result<Restart, String> {
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

/// Le texte signé pour un fichier publié : son nom (qui porte la version), puis
/// son empreinte SHA-256 en hexadécimal. build.yml signe exactement ce texte.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn signed_message(name: &str, sha256: &str) -> String {
    format!("{name}\n{sha256}")
}

/// Vérifie la signature (base64) du fichier `name` d'empreinte `sha256` avec la
/// clé publique `key` (base64).
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn verify_signature(name: &str, sha256: &str, signature: &str, key: &str) -> Result<(), String> {
    let bad = || "la signature du fichier ne correspond pas : mise à jour refusée".to_string();
    let key = crate::extensions::base64_decode(key.trim())
        .and_then(|k| ed25519_compact::PublicKey::from_slice(&k).ok())
        .ok_or("clé de vérification absente : installez la version depuis sa page")?;
    let signature = crate::extensions::base64_decode(signature.trim())
        .and_then(|s| ed25519_compact::Signature::from_slice(&s).ok())
        .ok_or_else(bad)?;
    key.verify(signed_message(name, sha256), &signature).map_err(|_| bad())
}

#[cfg(any(windows, target_os = "macos"))]
fn download(update: &Available, dest: &Path, sha256: &str) -> Result<(), String> {
    let url = &update.url;
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
    let actual = file_sha256(dest).map_err(|e| e.to_string())?;
    if actual != sha256 {
        return Err("le fichier téléchargé est corrompu (empreinte différente)".into());
    }
    // La signature, quelques dizaines d'octets, lue directement.
    let mut sig = Command::new("curl");
    sig.args(["-sSL", "--fail", "--max-time", "30", "--max-filesize", "4096", "--proto", "=https", "--proto-redir", "=https"])
        .arg(&update.signature_url);
    let signature = run(sig, "le téléchargement de la signature")?;
    verify_signature(&update.name, &actual, &String::from_utf8_lossy(&signature.stdout), PUBLIC_KEY)
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
    // « Pas de connexion ? » n'a de sens que pour curl, pas pour hdiutil ou ditto.
    let network = cmd.get_program() == "curl";
    let output = cmd
        .output()
        .map_err(|_| format!("{what} a échoué (outil système introuvable)"))?;
    if output.status.success() {
        Ok(output)
    } else if network {
        Err(format!("{what} a échoué (pas de connexion ?)"))
    } else {
        Err(format!("{what} a échoué"))
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
                { "name": "Feather-0.1.5-macos.dmg.sig", "size": 88,
                  "browser_download_url": format!("{base}/Feather-0.1.5-macos.dmg.sig") },
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
        assert!(mac.signature_url.ends_with("Feather-0.1.5-macos.dmg.sig"));
        assert_eq!(win.signature_url, "");
        // Pas de système pris en charge : on connaît la version mais pas de fichier.
        assert_eq!(pick_release(&release(), "0.1.4", None).unwrap().url, "");
        // À jour.
        assert_eq!(pick_release(&release(), "0.1.5", Some("-macos.dmg")), None);
    }

    #[test]
    fn installation_refusee_sans_empreinte() {
        let win = pick_release(&release(), "0.1.4", Some("-windows.exe")).unwrap();
        assert_eq!(win.sha256, None);
        let err = install(&win).unwrap_err();
        // Sur Linux, l'installation n'existe pas du tout ; ailleurs, c'est l'empreinte qui manque.
        assert!(err.contains("empreinte") || err.contains("n'existe pas"), "{err}");
    }

    #[test]
    fn fichier_d_une_autre_version() {
        // Un ancien fichier (signé) mis dans une release plus récente n'est pas pris.
        let mut release = release();
        release["assets"][0]["name"] = json!("Feather-0.1.2-macos.dmg");
        let found = pick_release(&release, "0.1.4", Some("-macos.dmg")).unwrap();
        assert_eq!(found.url, "");
    }

    // Signature produite par les commandes de build.yml (openssl), avec la graine
    // 00 01 02 … 1f : vérifie que la signature de la publication et celle que
    // Feather contrôle parlent bien du même texte.
    const TEST_KEY: &str = "A6EHv/POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=";
    const TEST_SHA: &str = "91e3faafd322bcdf160f3f0ce886acb092b9b9e2a1e8526b40f21a8898a8700b";
    const TEST_SIG: &str = "gQd3p7OCtIL+Pq1xvD4aAilTPHQ1YlyZ5yncuv/7rXrv7RB4fNikDi1KS4HidpI3MW0xKIh7tVIgNAtbNiXLCw==\n";

    #[test]
    fn cle_publique_valide() {
        // La clé de Feather doit être un point Ed25519 valide, sinon aucune mise
        // à jour ne pourrait être vérifiée.
        let key = crate::extensions::base64_decode(PUBLIC_KEY).expect("base64");
        assert!(ed25519_compact::PublicKey::from_slice(&key).is_ok());
    }

    #[test]
    fn signature() {
        let name = "Feather-0.1.5-macos.dmg";
        assert_eq!(verify_signature(name, TEST_SHA, TEST_SIG, TEST_KEY), Ok(()));
        // Autre fichier, autre nom (donc autre version), signature abîmée, autre clé.
        let other = TEST_SHA.replace('9', "8");
        assert!(verify_signature(name, &other, TEST_SIG, TEST_KEY).is_err());
        assert!(verify_signature("Feather-0.1.4-macos.dmg", TEST_SHA, TEST_SIG, TEST_KEY).is_err());
        assert!(verify_signature(name, TEST_SHA, &TEST_SIG.replace('g', "h"), TEST_KEY).is_err());
        assert!(verify_signature(name, TEST_SHA, "pas une signature", TEST_KEY).is_err());
        let other_key = "O2onvM62pC1io6jQKm8Nc2UyFXcd4kOmOsBIoYtZ2ik=";
        assert!(verify_signature(name, TEST_SHA, TEST_SIG, other_key).is_err());
        // Sans clé publique, rien ne s'installe.
        assert!(verify_signature(name, TEST_SHA, TEST_SIG, "").is_err());
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
