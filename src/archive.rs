//! Décompression des extensions (`.crx`, `.xpi`, `.zip`), en Rust.
//!
//! Avant, l'outil du système (`tar`, `ditto`, `unzip`) décompressait d'abord
//! et `extensions::check_tree` vérifiait après coup : une archive piégée (qui
//! gonfle à des gigaoctets, ou dont un nom remonte avec `../`) avait déjà
//! écrit sur le disque. Ici, tout le répertoire de l'archive est lu et vérifié
//! avant d'écrire le moindre fichier (noms, nombre, tailles annoncées), puis
//! chaque fichier est arrêté à la taille qu'il annonce : une archive qui ment
//! sur ses tailles est refusée à la limite, pas après.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::bufread::DeflateDecoder;

const DAMAGED: &str = "archive abîmée";
const TOO_BIG: &str = "extension trop grosse";

/// Un fichier (ou un dossier) annoncé par le répertoire de l'archive.
struct Entry {
    path: PathBuf,
    dir: bool,
    deflate: bool,
    crc: u32,
    packed: u64,
    size: u64,
    /// Début de son en-tête local dans l'archive.
    header: u64,
}

/// `len` octets à partir de `at`, s'ils existent (une position annoncée peut
/// être n'importe quoi).
fn slice(bytes: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    bytes.get(at..at.checked_add(len)?)
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(slice(bytes, at, 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(slice(bytes, at, 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(slice(bytes, at, 8)?.try_into().ok()?))
}

/// Décompresse `zip` dans `dest` (qui existe déjà) : au plus `max_files`
/// entrées et `max_size` octets en tout. Rien n'est écrit si un nom, le nombre
/// d'entrées ou les tailles annoncées sont refusés.
pub fn extract(zip: &[u8], dest: &Path, max_size: u64, max_files: usize) -> Result<(), String> {
    let entries = directory(zip, max_files)?;
    let announced = entries.iter().try_fold(0u64, |sum, e| sum.checked_add(e.size)).ok_or(TOO_BIG)?;
    if announced > max_size {
        return Err(TOO_BIG.into());
    }
    for entry in &entries {
        let target = dest.join(&entry.path);
        let folder = if entry.dir { Some(target.as_path()) } else { target.parent() };
        if let Some(folder) = folder {
            fs::create_dir_all(folder).map_err(|e| format!("écriture impossible ({e})"))?;
        }
        if !entry.dir {
            write_entry(zip, entry, &target)?;
        }
    }
    Ok(())
}

/// Lit le répertoire central (à la fin de l'archive) : toutes les entrées,
/// vérifiées, sans rien décompresser.
fn directory(zip: &[u8], max_files: usize) -> Result<Vec<Entry>, String> {
    // Fin du répertoire : 22 octets, suivis d'un commentaire d'au plus 64 Ko.
    let last = zip.len().saturating_sub(22);
    let end = (last.saturating_sub(0xffff)..=last)
        .rev()
        .find(|&at| zip[at..].starts_with(b"PK\x05\x06"))
        .ok_or("ce fichier n'est pas une archive zip")?;
    let count = u64::from(u16_at(zip, end + 10).ok_or(DAMAGED)?);
    let size = u64::from(u32_at(zip, end + 12).ok_or(DAMAGED)?);
    let offset = u64::from(u32_at(zip, end + 16).ok_or(DAMAGED)?);
    // Nombre d'entrées, début réel du répertoire, et décalage des positions annoncées.
    let (count, start, shift) = if count == 0xffff || size == 0xffff_ffff || offset == 0xffff_ffff {
        // ZIP64 : les vraies valeurs sont dans un second enregistrement, désigné juste avant.
        let locator = end.checked_sub(20).filter(|&at| zip[at..].starts_with(b"PK\x06\x07")).ok_or(DAMAGED)?;
        let record = u64_at(zip, locator + 8).and_then(|r| usize::try_from(r).ok()).ok_or(DAMAGED)?;
        if !zip.get(record..).is_some_and(|r| r.starts_with(b"PK\x06\x06")) {
            return Err(DAMAGED.into());
        }
        if (u32_at(zip, record + 16), u32_at(zip, record + 20)) != (Some(0), Some(0)) {
            return Err("archive en plusieurs morceaux".into());
        }
        (u64_at(zip, record + 32).ok_or(DAMAGED)?, u64_at(zip, record + 48).ok_or(DAMAGED)?, 0)
    } else {
        if (u16_at(zip, end + 4), u16_at(zip, end + 6)) != (Some(0), Some(0)) {
            return Err("archive en plusieurs morceaux".into());
        }
        // Octets ajoutés avant l'archive (rare) : les positions annoncées sont décalées d'autant.
        let start = (end as u64).checked_sub(size).ok_or(DAMAGED)?;
        (count, start, start.checked_sub(offset).ok_or(DAMAGED)?)
    };
    if count > max_files as u64 {
        return Err(TOO_BIG.into());
    }
    let mut at = usize::try_from(start).map_err(|_| DAMAGED)?;
    let mut entries = Vec::new();
    for _ in 0..count {
        let fixed = slice(zip, at, 46).filter(|f| f.starts_with(b"PK\x01\x02")).ok_or(DAMAGED)?;
        let field16 = |at: usize| u16_at(fixed, at).ok_or(DAMAGED);
        let field32 = |at: usize| u32_at(fixed, at).ok_or(DAMAGED);
        let (system, flags, method) = (field16(4)? >> 8, field16(8)?, field16(10)?);
        let crc = field32(16)?;
        let (mut packed, mut size) = (u64::from(field32(20)?), u64::from(field32(24)?));
        let (name_len, extra_len, comment_len) =
            (usize::from(field16(28)?), usize::from(field16(30)?), usize::from(field16(32)?));
        let attributes = field32(38)?;
        let mut header = u64::from(field32(42)?);
        let name = zip.get(at + 46..at + 46 + name_len).ok_or(DAMAGED)?;
        let extra = zip.get(at + 46 + name_len..at + 46 + name_len + extra_len).ok_or(DAMAGED)?;
        at += 46 + name_len + extra_len + comment_len;
        if [size, packed, header].contains(&0xffff_ffff) {
            zip64(extra, [&mut size, &mut packed, &mut header]).ok_or(DAMAGED)?;
        }
        let name = String::from_utf8_lossy(name);
        if flags & 1 != 0 {
            return Err("archive protégée par un mot de passe".into());
        }
        let deflate = match method {
            0 => false,
            8 => true,
            _ => return Err(format!("archive compressée d'une façon inconnue ({method})")),
        };
        // Lien symbolique (attributs Unix, ou macOS) : il pourrait viser hors du dossier.
        if matches!(system, 3 | 19) && (attributes >> 16) & 0o170_000 == 0o120_000 {
            return Err("l'archive contient un lien symbolique".into());
        }
        let dir = name.ends_with(['/', '\\']);
        let path = safe_path(&name)
            .filter(|p| dir || !p.as_os_str().is_empty())
            .ok_or_else(|| format!("l'archive contient un chemin interdit (« {} »)", name.chars().take(80).collect::<String>()))?;
        let header = header.checked_add(shift).ok_or(DAMAGED)?;
        entries.push(Entry { path, dir, deflate, crc, packed, size, header });
    }
    Ok(entries)
}

/// Champ « ZIP64 » (0x0001) d'une entrée : taille, taille compressée et
/// position qui ne tenaient pas sur 32 bits (marquées 0xFFFFFFFF), dans cet ordre.
fn zip64(extra: &[u8], values: [&mut u64; 3]) -> Option<()> {
    let mut rest = extra;
    while rest.len() >= 4 {
        let (id, len) = (u16_at(rest, 0)?, usize::from(u16_at(rest, 2)?));
        let data = rest.get(4..4 + len)?;
        if id == 1 {
            let mut at = 0;
            for value in values {
                if *value == 0xffff_ffff {
                    *value = u64_at(data, at)?;
                    at += 8;
                }
            }
            return Some(());
        }
        rest = &rest[4 + len..];
    }
    None
}

/// Chemin relatif sûr pour un nom de l'archive, ou `None` : chemin absolu,
/// lecteur (« C: »), « .. » (ou « .. » déguisé, comme « .. » suivi d'espaces,
/// que Windows lit pareil), nom réservé de Windows, caractère de contrôle.
/// Les « . » sont ignorés ; « ./ » seul donne le dossier lui-même (vide).
fn safe_path(name: &str) -> Option<PathBuf> {
    let name = name.strip_suffix(['/', '\\']).unwrap_or(name);
    let mut path = PathBuf::new();
    for part in name.split(['/', '\\']) {
        if part == "." {
            continue;
        }
        if part.trim_end_matches([' ', '.']).is_empty()
            || part.contains(':')
            || part.chars().any(char::is_control)
            || reserved(part)
        {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

/// Noms de périphériques de Windows (« CON », « nul.js »…) : y écrire
/// n'écrit pas un fichier.
fn reserved(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or_default().trim_end().to_ascii_lowercase();
    let numbered = stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit();
    numbered || matches!(stem.as_str(), "con" | "prn" | "aux" | "nul" | "conin$" | "conout$")
}

/// Décompresse une entrée dans `target`, sans dépasser la taille annoncée, et
/// vérifie son empreinte (CRC-32).
fn write_entry(zip: &[u8], entry: &Entry, target: &Path) -> Result<(), String> {
    let at = usize::try_from(entry.header).map_err(|_| DAMAGED)?;
    let local = slice(zip, at, 30).filter(|h| h.starts_with(b"PK\x03\x04")).ok_or(DAMAGED)?;
    let skip = 30 + usize::from(u16_at(local, 26).ok_or(DAMAGED)?) + usize::from(u16_at(local, 28).ok_or(DAMAGED)?);
    let packed = usize::try_from(entry.packed).map_err(|_| DAMAGED)?;
    let data = slice(zip, at + skip, packed).ok_or(DAMAGED)?;
    let reader: Box<dyn Read> = if entry.deflate { Box::new(DeflateDecoder::new(data)) } else { Box::new(data) };
    // Un octet de plus que l'annonce suffit à voir qu'elle ment.
    let mut reader = reader.take(entry.size + 1);
    let mut file = fs::File::create(target).map_err(|e| format!("écriture impossible ({e})"))?;
    let mut crc = flate2::Crc::new();
    let mut written = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|_| DAMAGED)?;
        if n == 0 {
            break;
        }
        crc.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| format!("écriture impossible ({e})"))?;
        written += n as u64;
    }
    if written != entry.size || crc.sum() != entry.crc {
        return Err(DAMAGED.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une entrée d'archive de test : nom, contenu, compressée ou non, type Unix.
    struct F<'a>(&'a str, &'a [u8], bool, u32);

    const FILE: u32 = 0o100_644;
    const LINK: u32 = 0o120_777;

    /// Archive zip écrite à la main ; `zip64` : tailles et positions rangées
    /// dans les champs ZIP64, comme le font certains outils.
    fn zip_of(files: &[F], zip64: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for F(name, data, deflate, mode) in files {
            let packed = if *deflate {
                let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                enc.write_all(data).unwrap();
                enc.finish().unwrap()
            } else {
                data.to_vec()
            };
            let mut crc = flate2::Crc::new();
            crc.update(data);
            let offset = out.len() as u32;
            let method: u16 = if *deflate { 8 } else { 0 };
            out.extend(b"PK\x03\x04");
            for v in [20u16, 0, method, 0, 0] {
                out.extend(v.to_le_bytes());
            }
            out.extend(crc.sum().to_le_bytes());
            out.extend((packed.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend(&packed);

            let big = |v: u32| if zip64 { u32::MAX } else { v };
            let mut extra = Vec::new();
            if zip64 {
                extra.extend(1u16.to_le_bytes());
                extra.extend(24u16.to_le_bytes());
                for v in [data.len() as u64, packed.len() as u64, u64::from(offset)] {
                    extra.extend(v.to_le_bytes());
                }
            }
            central.extend(b"PK\x01\x02");
            for v in [3 << 8 | 20u16, 20, 0, method, 0, 0] {
                central.extend(v.to_le_bytes());
            }
            central.extend(crc.sum().to_le_bytes());
            central.extend(big(packed.len() as u32).to_le_bytes());
            central.extend(big(data.len() as u32).to_le_bytes());
            for v in [name.len() as u16, extra.len() as u16, 0, 0, 0] {
                central.extend(v.to_le_bytes());
            }
            central.extend((mode << 16).to_le_bytes());
            central.extend(big(offset).to_le_bytes());
            central.extend(name.as_bytes());
            central.extend(&extra);
        }
        let (start, count) = (out.len() as u64, files.len() as u64);
        out.extend(&central);
        if zip64 {
            let record = out.len() as u64;
            out.extend(b"PK\x06\x06");
            out.extend(44u64.to_le_bytes());
            for v in [45u16, 45] {
                out.extend(v.to_le_bytes());
            }
            out.extend([0u8; 8]);
            for v in [count, count, central.len() as u64, start] {
                out.extend(v.to_le_bytes());
            }
            out.extend(b"PK\x06\x07");
            out.extend(0u32.to_le_bytes());
            out.extend(record.to_le_bytes());
            out.extend(1u32.to_le_bytes());
        }
        out.extend(b"PK\x05\x06");
        out.extend([0u8; 4]);
        let small = |v: u64, max: u64| if zip64 { max } else { v };
        for _ in 0..2 {
            out.extend((small(count, 0xffff) as u16).to_le_bytes());
        }
        out.extend((small(central.len() as u64, 0xffff_ffff) as u32).to_le_bytes());
        out.extend((small(start, 0xffff_ffff) as u32).to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("feather-archive-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn files_in(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    fn position(zip: &[u8], sig: &[u8]) -> usize {
        zip.windows(sig.len()).position(|w| w == sig).unwrap()
    }

    #[test]
    fn archive_ordinaire() {
        let script = "console.log('bonjour');\n".repeat(200);
        let files = [
            F("ext/", b"", false, 0o040_755),
            F("ext/manifest.json", br#"{"name":"Essai"}"#, true, FILE),
            F("ext/js/a.js", script.as_bytes(), true, FILE),
            F("ext/./img/vide.png", b"", false, FILE),
            F("ext/LISEZ-MOI", b"texte", false, FILE),
        ];
        for zip64 in [false, true] {
            let dir = temp(&format!("ordinaire-{zip64}"));
            // Octets ajoutés devant l'archive (zip classique seulement) : retrouvés quand même.
            let mut zip = if zip64 { Vec::new() } else { b"prefixe".to_vec() };
            zip.extend(zip_of(&files, zip64));
            extract(&zip, &dir, 1 << 20, 100).unwrap();
            assert_eq!(fs::read_to_string(dir.join("ext/manifest.json")).unwrap(), r#"{"name":"Essai"}"#);
            assert_eq!(fs::read_to_string(dir.join("ext/js/a.js")).unwrap(), script);
            assert_eq!(fs::read(dir.join("ext/img/vide.png")).unwrap(), b"");
            assert_eq!(fs::read_to_string(dir.join("ext/LISEZ-MOI")).unwrap(), "texte");
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn chemins_interdits() {
        for bad in [
            "../evasion.js", "/etc/evasion", "a/../../evasion", "C:/evasion", "c:evasion", "a\\..\\..\\evasion",
            ".. /evasion", "a/.../evasion", "a//b", "con.js", "a/NUL", "lpt1.txt", "x\u{1}y", "fichier.js:flux",
        ] {
            let dir = temp("interdits");
            // Le premier fichier, sans problème, n'est pas écrit non plus : tout est vérifié avant.
            let zip = zip_of(&[F("ok.js", b"ok", false, FILE), F(bad, b"mal", false, FILE)], false);
            let err = extract(&zip, &dir, 1 << 20, 100).unwrap_err();
            assert!(err.contains("chemin interdit"), "{bad} : {err}");
            assert_eq!(files_in(&dir), 0, "{bad}");
            fs::remove_dir_all(&dir).unwrap();
        }
        assert_eq!(safe_path("./"), Some(PathBuf::new()));
        assert_eq!(safe_path("a/b.js"), Some(Path::new("a").join("b.js")));
        assert_eq!(safe_path("com.js"), Some(PathBuf::from("com.js")));
        assert_eq!(safe_path("console.js"), Some(PathBuf::from("console.js")));
    }

    #[test]
    fn lien_symbolique_refuse() {
        let dir = temp("lien");
        let zip = zip_of(&[F("lien", b"/etc/passwd", false, LINK)], false);
        assert!(extract(&zip, &dir, 1 << 20, 100).unwrap_err().contains("lien symbolique"));
        assert_eq!(files_in(&dir), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn archives_demesurees() {
        let dir = temp("demesurees");
        let zeros = vec![0u8; 200_000];
        let zip = zip_of(&[F("zeros.bin", &zeros, true, FILE)], false);
        // Taille annoncée trop grande : refusée avant d'écrire.
        assert_eq!(extract(&zip, &dir, 100_000, 100).unwrap_err(), TOO_BIG);
        assert_eq!(files_in(&dir), 0);
        // Trop de fichiers.
        let many = zip_of(&[F("a", b"a", false, FILE), F("b", b"b", false, FILE), F("c", b"c", false, FILE)], false);
        assert_eq!(extract(&many, &dir, 1 << 20, 2).unwrap_err(), TOO_BIG);
        assert_eq!(files_in(&dir), 0);
        // L'archive ment (10 octets annoncés, 200 000 réels) : arrêtée à 11 octets.
        let mut lying = zip.clone();
        let at = position(&lying, b"PK\x01\x02") + 24;
        lying[at..at + 4].copy_from_slice(&10u32.to_le_bytes());
        assert_eq!(extract(&lying, &dir, 100_000, 100).unwrap_err(), DAMAGED);
        assert!(fs::metadata(dir.join("zeros.bin")).unwrap().len() <= 11);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn archives_abimees() {
        let dir = temp("abimees");
        let zip = zip_of(&[F("a.js", b"contenu du fichier", true, FILE)], false);
        // Empreinte fausse.
        let mut bad_crc = zip.clone();
        let at = position(&bad_crc, b"PK\x01\x02") + 16;
        bad_crc[at] ^= 1;
        assert_eq!(extract(&bad_crc, &dir, 1 << 20, 100).unwrap_err(), DAMAGED);
        // Coupée au milieu, ou pas une archive.
        assert!(extract(&zip[..zip.len() / 2], &dir, 1 << 20, 100).is_err());
        assert!(extract(b"PK\x05\x06", &dir, 1 << 20, 100).is_err());
        assert!(extract(b"<html>", &dir, 1 << 20, 100).is_err());
        assert!(extract(b"", &dir, 1 << 20, 100).is_err());
        // Méthode inconnue (9 : deflate64) ou chiffrée.
        for (field, value) in [(10, 9u16), (8, 1)] {
            let mut odd = zip.clone();
            let at = position(&odd, b"PK\x01\x02") + field;
            odd[at..at + 2].copy_from_slice(&value.to_le_bytes());
            assert!(extract(&odd, &dir, 1 << 20, 100).is_err());
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
