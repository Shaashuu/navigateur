//! Enregistrer une image de la page (clic droit) : dans les téléchargements,
//! ou directement dans l'app Photos sous macOS.
//!
//! L'image est téléchargée par `curl`, comme la mise à jour : pas de
//! bibliothèque HTTP de plus. Les cookies du site ne sont pas envoyés, mais
//! la page d'origine l'est (« Referer »), que demandent certains sites, et
//! seulement ce que le moteur enverrait lui-même (voir `referer_for`).

use std::path::{Path, PathBuf};

/// Formats reconnus d'après les premiers octets du fichier, avec leur extension.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"\xFF\xD8\xFF") {
        Some("jpg")
    } else if starts(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if starts(b"BM") {
        Some("bmp")
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        match &bytes[8..12] {
            b"avif" | b"avis" => Some("avif"),
            b"heic" | b"heix" | b"mif1" | b"msf1" => Some("heic"),
            _ => None,
        }
    } else {
        None
    }
}

/// Nom du fichier d'après l'adresse : le dernier morceau du chemin, sans les
/// caractères interdits dans un nom de fichier. « image » à défaut.
pub fn file_name(url: &str) -> String {
    let last = url::Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(str::to_string)))
        .unwrap_or_default();
    let decoded = percent_decode(&last);
    let clean: String = decoded
        .chars()
        .map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() { "image".to_string() } else { clean.chars().take(120).collect() }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Télécharge l'image dans `dest`.
pub fn fetch(url: &str, referer: &str, dest: &Path) -> Result<(), String> {
    if !crate::network::is_web(url) {
        return Err("adresse non prise en charge".into());
    }
    let mut cmd = crate::network::command("curl");
    cmd.args(["-sSL", "--fail", "--max-time", "120", "--max-filesize", "104857600", "--proto", "=http,https", "--proto-redir", "=http,https"]);
    if let Some(referer) = referer_for(url, referer) {
        cmd.arg("-e").arg(referer);
    }
    cmd.arg("-o").arg(dest).arg(url);
    let output = cmd.output().map_err(|e| format!("curl introuvable ({e})"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(dest);
        return Err("le téléchargement de l'image a échoué".into());
    }
    Ok(())
}

/// Le « Referer » envoyé avec l'image, comme le ferait le moteur (règle par
/// défaut des navigateurs, `strict-origin-when-cross-origin`) : l'adresse
/// complète de la page pour une image du même site, seulement le site
/// (« https://exemple.fr/ ») pour une image d'ailleurs, et rien du tout d'une
/// page https vers une image http. L'adresse de la page peut contenir une
/// recherche ou un identifiant : elle ne part pas chez un autre site.
fn referer_for(image: &str, page: &str) -> Option<String> {
    if !crate::network::is_web(page) {
        return None;
    }
    let mut page = url::Url::parse(page).ok()?;
    let image = url::Url::parse(image).ok()?;
    if page.scheme() == "https" && image.scheme() != "https" {
        return None;
    }
    page.set_fragment(None);
    if page.origin() == image.origin() {
        return Some(page.to_string());
    }
    Some(format!("{}/", page.origin().ascii_serialization()))
}

/// Les premiers octets du fichier.
fn head(path: &Path) -> Vec<u8> {
    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf[..n].to_vec()
}

/// Une image SVG (texte) : reconnue à son début, après un éventuel « <?xml ».
fn is_svg(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg"))
}

/// Vérifie que le fichier téléchargé est bien une image et lui donne
/// l'extension de son vrai format. Le nom vient de l'adresse, que la page
/// choisit : sans ça, « photo.exe » finissait tel quel dans les téléchargements.
/// Un fichier qui n'est pas une image est supprimé.
pub fn checked_name(path: &Path) -> Result<PathBuf, String> {
    let bytes = head(path);
    let Some(ext) = sniff(&bytes).or_else(|| is_svg(&bytes).then_some("svg")) else {
        let _ = std::fs::remove_file(path);
        return Err("ce fichier n'est pas une image".into());
    };
    let current = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let same = match current.as_deref() {
        Some("jpeg") => ext == "jpg",
        Some("heif") => ext == "heic",
        Some(e) => e == ext,
        None => false,
    };
    if same {
        return Ok(path.to_path_buf());
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
    let dir = path.parent().unwrap_or(Path::new("."));
    let target = crate::downloads::unique(dir, &format!("{stem}.{ext}"));
    match std::fs::rename(path, &target) {
        Ok(()) => Ok(target),
        Err(e) => {
            let _ = std::fs::remove_file(path);
            Err(e.to_string())
        }
    }
}

/// Télécharge l'image puis l'ajoute à la photothèque (macOS). À appeler hors du
/// fil principal : on attend la réponse de l'utilisateur à la demande d'accès.
#[cfg(target_os = "macos")]
pub fn save_to_photos(url: &str, referer: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let tmp = std::env::temp_dir().join(format!("feather-image-{}-{}", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed)));
    fetch(url, referer, &tmp)?;
    let Some(ext) = sniff(&head(&tmp)) else {
        let _ = std::fs::remove_file(&tmp);
        return Err("ce format d'image n'est pas accepté par Photos".into());
    };
    // Photos reconnaît le format grâce à l'extension.
    let file = tmp.with_extension(ext);
    std::fs::rename(&tmp, &file).map_err(|e| e.to_string())?;
    let result = photos::add(&file);
    let _ = std::fs::remove_file(&file);
    result
}

#[cfg(target_os = "macos")]
mod photos {
    use std::path::Path;
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2_foundation::{NSString, NSURL};
    use objc2_photos::{PHAccessLevel, PHAssetChangeRequest, PHAuthorizationStatus, PHPhotoLibrary};

    /// Demande l'accès « ajout seulement » (la première fois, macOS pose la question).
    fn authorize() -> Result<(), String> {
        let status = unsafe { PHPhotoLibrary::authorizationStatusForAccessLevel(PHAccessLevel::AddOnly) };
        let status = if status == PHAuthorizationStatus::NotDetermined {
            let (tx, rx) = mpsc::channel();
            let handler = RcBlock::new(move |s: PHAuthorizationStatus| {
                let _ = tx.send(s);
            });
            unsafe { PHPhotoLibrary::requestAuthorizationForAccessLevel_handler(PHAccessLevel::AddOnly, &handler) };
            rx.recv().unwrap_or(PHAuthorizationStatus::Denied)
        } else {
            status
        };
        if status == PHAuthorizationStatus::Authorized || status == PHAuthorizationStatus::Limited {
            Ok(())
        } else {
            Err("Feather n'a pas le droit d'ajouter des photos (Réglages Système → Confidentialité et sécurité → Photos)".into())
        }
    }

    pub fn add(file: &Path) -> Result<(), String> {
        authorize()?;
        let path = NSString::from_str(&file.to_string_lossy());
        let url = NSURL::fileURLWithPath(&path);
        let change = RcBlock::new(move || {
            let _ = unsafe { PHAssetChangeRequest::creationRequestForAssetFromImageAtFileURL(&url) };
        });
        let library = unsafe { PHPhotoLibrary::sharedPhotoLibrary() };
        unsafe { library.performChangesAndWait_error(RcBlock::as_ptr(&change)) }
            .map_err(|e| format!("Photos a refusé l'image ({})", e.localizedDescription()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referer_comme_le_moteur() {
        let page = "https://exemple.fr/compte?jeton=123#photos";
        // Même site : l'adresse complète, sans le fragment.
        assert_eq!(referer_for("https://exemple.fr/img/a.png", page).as_deref(), Some("https://exemple.fr/compte?jeton=123"));
        // Autre site : seulement l'origine de la page.
        assert_eq!(referer_for("https://cdn.images.fr/a.png", page).as_deref(), Some("https://exemple.fr/"));
        assert_eq!(referer_for("https://exemple.fr:8443/a.png", page).as_deref(), Some("https://exemple.fr/"));
        // De https vers http : rien.
        assert_eq!(referer_for("http://exemple.fr/a.png", page), None);
        // D'une page http, l'origine reste envoyée vers un autre site.
        assert_eq!(referer_for("https://cdn.fr/a.png", "http://site.fr/p?q=1").as_deref(), Some("http://site.fr/"));
        // Pas une page web (accueil, fichier) : rien.
        assert_eq!(referer_for("https://cdn.fr/a.png", "file:///Users/moi/page.html"), None);
        assert_eq!(referer_for("https://cdn.fr/a.png", crate::network::HOME_URL), None);
    }

    #[test]
    fn formats_reconnus() {
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0\0\x10JFIF"), Some("jpg"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n\0\0"), Some("png"));
        assert_eq!(sniff(b"GIF89a\x01\0"), Some("gif"));
        assert_eq!(sniff(b"RIFF\x24\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff(b"\0\0\0\x1cftypavif\0\0"), Some("avif"));
        assert_eq!(sniff(b"<svg xmlns"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn nom_du_fichier() {
        assert_eq!(file_name("https://ex.fr/img/chat%20noir.jpg?w=200"), "chat noir.jpg");
        assert_eq!(file_name("https://ex.fr/"), "image");
        assert_eq!(file_name("https://ex.fr/a/b%2Fc.png"), "b_c.png");
        assert_eq!(file_name("https://ex.fr/photo"), "photo");
        assert_eq!(file_name("https://ex.fr/fin%"), "fin%");
    }

    #[test]
    fn extension_du_vrai_format() {
        let dir = std::env::temp_dir().join(format!("feather-test-images-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        // Une « image » qui se dit .exe reçoit l'extension de son vrai format.
        let renamed = checked_name(&write("piege.exe", b"\x89PNG\r\n\x1a\n\0\0")).unwrap();
        assert_eq!(renamed.file_name().unwrap(), "piege.png");
        // Le bon nom est gardé, « .jpeg » compris.
        let kept = checked_name(&write("chat.jpeg", b"\xFF\xD8\xFF\xE0")).unwrap();
        assert_eq!(kept.file_name().unwrap(), "chat.jpeg");
        assert_eq!(checked_name(&write("logo", b"<?xml version='1.0'?><svg>")).unwrap().file_name().unwrap(), "logo.svg");
        // Ce qui n'est pas une image est refusé et supprimé.
        let fake = write("faux.jpg", b"MZ\x90\0");
        assert!(checked_name(&fake).is_err());
        assert!(!fake.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
