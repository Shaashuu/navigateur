//! Enregistrer une image de la page (clic droit) : dans les téléchargements,
//! ou directement dans l'app Photos sous macOS.
//!
//! L'image est téléchargée par `curl`, comme la mise à jour : pas de
//! bibliothèque HTTP de plus. Les cookies du site ne sont pas envoyés, mais
//! l'adresse de la page l'est (« Referer »), que demandent certains sites.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// L'extension du nom est-elle celle d'une image ?
pub fn has_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["jpg", "jpeg", "png", "gif", "webp", "bmp", "avif", "heic", "svg"].contains(&e.to_ascii_lowercase().as_str()))
}

/// Télécharge l'image dans `dest`.
pub fn fetch(url: &str, referer: &str, dest: &Path) -> Result<(), String> {
    if !crate::network::is_web(url) {
        return Err("adresse non prise en charge".into());
    }
    let mut cmd = Command::new("curl");
    cmd.args(["-sSL", "--fail", "--max-time", "120", "--proto", "=http,https", "--proto-redir", "=http,https"]);
    if crate::network::is_web(referer) {
        cmd.arg("-e").arg(referer);
    }
    cmd.arg("-o").arg(dest).arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().map_err(|e| format!("curl introuvable ({e})"))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(dest);
        return Err("le téléchargement de l'image a échoué".into());
    }
    Ok(())
}

/// Sans extension connue, on ajoute celle du format réel (« photo » → « photo.jpg »).
pub fn fix_extension(path: &Path) -> PathBuf {
    if has_image_extension(path) {
        return path.to_path_buf();
    }
    let head = std::fs::read(path).map(|b| b.into_iter().take(16).collect::<Vec<_>>()).unwrap_or_default();
    let Some(ext) = sniff(&head) else { return path.to_path_buf() };
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{ext}"));
    let target = PathBuf::from(name);
    if target.exists() || std::fs::rename(path, &target).is_err() {
        return path.to_path_buf();
    }
    target
}

/// Télécharge l'image puis l'ajoute à la photothèque (macOS). À appeler hors du
/// fil principal : on attend la réponse de l'utilisateur à la demande d'accès.
#[cfg(target_os = "macos")]
pub fn save_to_photos(url: &str, referer: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let tmp = std::env::temp_dir().join(format!("feather-image-{}-{}", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed)));
    fetch(url, referer, &tmp)?;
    let head = std::fs::read(&tmp).map(|b| b.into_iter().take(16).collect::<Vec<_>>()).unwrap_or_default();
    let Some(ext) = sniff(&head) else {
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
}
