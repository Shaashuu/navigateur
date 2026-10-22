//! Extensions de navigateur (point 13 de la feuille de route).
//!
//! On colle le lien d'une extension du Chrome Web Store ou de Firefox Add-ons
//! (ou on choisit un fichier `.crx`, `.xpi`, `.zip`) : Feather la télécharge,
//! la décompresse dans le dossier de configuration (`extensions/`) et la
//! confie au moteur du système.
//!
//! - Windows : WebView2 sait faire tourner les extensions Chrome (et Edge),
//!   décompressées. Les extensions Firefox n'y marchent qu'en partie : celles
//!   qui utilisent l'espace `browser.*` sans équivalent `chrome.*` échouent.
//! - macOS 15.4 ou plus récent : WebKit (`WKWebExtension`) fait tourner les
//!   extensions Chrome et Firefox, comme Safari. Plus ancien : pas d'extensions.
//! - Linux : WebKitGTK ne sait pas faire tourner d'extensions ; rien à faire.
//!
//! Aucun des deux moteurs n'affiche la petite fenêtre d'une extension (celle
//! de son bouton) : Feather l'ouvre lui-même par-dessus la page.
//!
//! Le téléchargement passe par `curl` (https seulement) et la décompression
//! par les outils du système (`tar` de Windows, `ditto` de macOS), comme la
//! mise à jour : aucune dépendance ajoutée.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;
use wry::WebViewBuilder;

use crate::adblock;
use crate::storage;

/// Fichier qui liste les extensions installées.
const FILE: &str = "extensions.json";
/// Taille maximale d'une extension téléchargée ou choisie (octets).
pub const MAX_ARCHIVE: usize = 100 * 1024 * 1024;
/// Taille maximale d'une extension décompressée, et nombre de fichiers.
const MAX_UNPACKED: u64 = 300 * 1024 * 1024;
const MAX_FILES: usize = 20_000;
/// Icône envoyée à l'interface : au-delà, on garde la pastille avec l'initiale.
const MAX_ICON: u64 = 96 * 1024;
/// Version de Chrome annoncée au Chrome Web Store (certaines extensions en exigent une récente).
const CHROME_VERSION: &str = "138.0";

/// D'où vient une extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Chrome,
    Firefox,
    Fichier,
    Dossier,
}

/// Une extension installée (gardée dans `extensions.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Extension {
    /// Identifiant Feather : celui du Chrome Web Store quand on le connaît.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    pub source: Source,
    pub enabled: bool,
    /// Dossier de l'extension, relatif au dossier `extensions`.
    pub dir: String,
    /// Petite fenêtre du bouton, options, icône : chemins dans le dossier de l'extension.
    #[serde(default)]
    pub popup: Option<String>,
    #[serde(default)]
    pub options: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// L'extension lit et modifie toutes les pages.
    #[serde(default)]
    pub all_sites: bool,
    /// Extension écrite pour Firefox.
    #[serde(default)]
    pub firefox: bool,
    /// Identifiant donné par WebView2 (Windows), pour les adresses `chrome-extension://`.
    #[serde(default)]
    pub engine_id: Option<String>,
    /// Où chercher les nouvelles versions : identifiant du Chrome Web Store ou
    /// nom court sur Firefox Add-ons (`None` : fichier ou dossier).
    #[serde(default)]
    pub origin: Option<String>,
    /// Autorisations, sites et autorisations facultatives du manifeste, montrés
    /// avant l'installation (voir `Pending`).
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
}

impl Extension {
    /// D'où vient la mise à jour. Les extensions installées avant 0.5.8 n'ont pas
    /// `origin` : celles du Chrome Web Store gardent leur identifiant.
    fn update_origin(&self) -> Option<Origin> {
        match self.source {
            Source::Chrome => Some(Origin::Chrome(self.origin.clone().unwrap_or_else(|| self.id.clone()))),
            Source::Firefox => self.origin.clone().map(Origin::Firefox),
            Source::Fichier | Source::Dossier => None,
        }
    }

    /// La nouvelle version demande-t-elle plus que l'ancienne ? Sinon, elle
    /// s'installe sans rien demander.
    fn asks_more_than(&self, old: &Extension) -> bool {
        let more = |new: &[String], old: &[String]| new.iter().any(|p| !old.contains(p));
        (self.all_sites && !old.all_sites)
            || more(&self.permissions, &old.permissions)
            || more(&self.hosts, &old.hosts)
            || more(&self.optional, &old.optional)
    }

    pub fn path(&self) -> Option<PathBuf> {
        // Séparateurs du système (WebView2 reçoit un chemin Windows propre).
        Some(self.dir.split('/').fold(root()?, |path, part| path.join(part)))
    }
}

/// Ce que l'on demande d'installer.
#[derive(Debug, PartialEq)]
pub enum Origin {
    /// Identifiant du Chrome Web Store (32 lettres de a à p).
    Chrome(String),
    /// Nom court (ou identifiant) d'une extension sur addons.mozilla.org.
    Firefox(String),
    /// Dossier d'une extension décompressée (développeurs).
    Folder(PathBuf),
}

pub enum Request {
    Origin(Origin),
    /// Fichier choisi dans le panneau : son nom et son contenu.
    File { name: String, bytes: Vec<u8> },
}

/// Dossier des extensions, dans le dossier de configuration.
fn root() -> Option<PathBuf> {
    storage::path("extensions")
}

/// Les extensions sont-elles possibles ici ? Sinon, la raison, pour le panneau.
pub fn support() -> Result<(), &'static str> {
    engine::support()
}

/// Bloqueurs de publicités connus, par identifiant du Chrome Web Store : uBlock
/// Origin Lite, uBlock Origin, AdGuard, AdBlock, Adblock Plus, Ghostery.
const AD_BLOCKER_IDS: &[&str] = &[
    "ddkjiahejlhfcafbddmgiahcphecmpfh",
    "cjpalhdlnbpafiamejdnhcphjbkeiagm",
    "bgnkhhnnamicmpeenaelnjfhikgbkllg",
    "gighmmpiobklfepjocnamgkkbiglidom",
    "cfhdojbkjhnklbpkdaibdccddilifddb",
    "mlomiejdfkolichcflejclcbmpeaniij",
];

/// L'extension est un bloqueur de publicités (identifiant connu, ou nom).
pub fn blocks_ads(ext: &Extension) -> bool {
    let name = ext.name.to_lowercase();
    AD_BLOCKER_IDS.contains(&ext.id.as_str())
        || ["ublock", "adblock", "adguard", "ghostery"].iter().any(|n| name.contains(n))
}

/// Page d'une extension sur le Chrome Web Store (bouton « Ajouter à Feather »,
/// voir assets/page.js) : son identifiant, sinon `None`.
pub fn store_page(url: &str) -> Option<String> {
    if crate::network::host_of(url).as_deref() != Some("chromewebstore.google.com") || !url.starts_with("https://") {
        return None;
    }
    match parse_input(url) {
        Ok(Origin::Chrome(id)) => Some(id),
        _ => None,
    }
}

fn is_store_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// Comprend ce que l'utilisateur a collé : lien du Chrome Web Store, de
/// Firefox Add-ons, identifiant d'extension Chrome, ou dossier.
pub fn parse_input(input: &str) -> Result<Origin, String> {
    let input = input.trim();
    if is_store_id(input) {
        return Ok(Origin::Chrome(input.to_string()));
    }
    let with_scheme = if input.contains("://") { input.to_string() } else { format!("https://{input}") };
    if let Ok(url) = Url::parse(&with_scheme)
        && matches!(url.scheme(), "https" | "http")
        && let Some(host) = url.host_str()
    {
        let segments: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
        match host {
            "chromewebstore.google.com" | "chrome.google.com" => {
                if let Some(id) = segments.iter().rev().find(|s| is_store_id(s)) {
                    return Ok(Origin::Chrome(id.to_string()));
                }
                return Err("Ce lien du Chrome Web Store ne contient pas d'identifiant d'extension.".into());
            }
            "addons.mozilla.org" => {
                let slug = segments.iter().position(|s| *s == "addon").and_then(|i| segments.get(i + 1));
                if let Some(slug) = slug {
                    let slug = url::form_urlencoded::parse(slug.as_bytes()).next().map(|(k, _)| k.into_owned()).unwrap_or_default();
                    if !slug.is_empty() && slug.len() <= 200 {
                        return Ok(Origin::Firefox(slug));
                    }
                }
                return Err("Ce lien de Firefox Add-ons ne mène pas à une extension.".into());
            }
            _ => {}
        }
    }
    let path = Path::new(input);
    if path.is_absolute() && path.join("manifest.json").is_file() {
        return Ok(Origin::Folder(path.to_path_buf()));
    }
    Err("Collez le lien d'une extension du Chrome Web Store ou de Firefox Add-ons.".into())
}

fn chrome_url(id: &str) -> String {
    format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={CHROME_VERSION}&acceptformat=crx2,crx3&x=id%3D{id}%26uc"
    )
}

fn firefox_url(slug: &str) -> String {
    let slug: String = url::form_urlencoded::byte_serialize(slug.as_bytes()).collect();
    format!("https://addons.mozilla.org/firefox/downloads/latest/{slug}/latest.xpi")
}

/// Télécharge l'extension (https seulement, redirections comprises).
fn download(url: &str, path: &Path) -> Result<(), String> {
    let mut cmd = crate::network::command("curl");
    cmd.args(["-sSL", "--fail", "--max-time", "180", "--proto", "=https", "--proto-redir", "=https"])
        .args(["--max-filesize", &MAX_ARCHIVE.to_string(), "-o"])
        .arg(path)
        .arg(url);
    let out = cmd.output().map_err(|e| format!("curl introuvable : {e}"))?;
    if !out.status.success() {
        let _ = fs::remove_file(path);
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.contains("404") {
            "extension introuvable sur la boutique".into()
        } else {
            format!("téléchargement impossible ({err})")
        });
    }
    Ok(())
}

fn stamp() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default()
}

/// Télécharge (ou lit), décompresse et range une extension. Appelé hors du fil
/// principal : rien n'est encore confié au moteur.
pub fn prepare(request: Request) -> Result<Extension, String> {
    let root = root().ok_or("dossier de configuration introuvable")?;
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let tmp = root.join(format!(".nouvelle-{}", stamp()));
    let result = prepare_in(request, &root, &tmp);
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::remove_file(tmp.with_extension("zip"));
    let _ = fs::remove_file(tmp.with_extension("telechargement"));
    result
}

fn prepare_in(request: Request, root: &Path, tmp: &Path) -> Result<Extension, String> {
    let fetch = |url: &str| -> Result<Vec<u8>, String> {
        let file = tmp.with_extension("telechargement");
        download(url, &file)?;
        let bytes = fs::read(&file).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&file);
        Ok(bytes)
    };
    let (source, hint, key) = match request {
        Request::Origin(Origin::Chrome(id)) => {
            let bytes = fetch(&chrome_url(&id))?;
            let key = unpack(&bytes, tmp)?;
            // Le fichier reçu doit bien être l'extension demandée.
            if key.as_deref().map(id_from_key).as_deref() != Some(id.as_str()) {
                return Err("fichier reçu inattendu".into());
            }
            (Source::Chrome, id, key)
        }
        Request::Origin(Origin::Firefox(slug)) => {
            let bytes = fetch(&firefox_url(&slug))?;
            unpack(&bytes, tmp)?;
            (Source::Firefox, slug, None)
        }
        Request::Origin(Origin::Folder(path)) => {
            copy_dir(&path, tmp)?;
            (Source::Dossier, path.display().to_string(), None)
        }
        Request::File { name, bytes } => {
            if bytes.len() > MAX_ARCHIVE {
                return Err("fichier trop gros".into());
            }
            let key = unpack(&bytes, tmp)?;
            (Source::Fichier, name, key)
        }
    };
    check_tree(tmp)?;
    let base = manifest_root(tmp)?;
    let info = read_manifest(&base)?;
    let id = match (&source, &key) {
        (Source::Chrome, _) => hint.clone(),
        (_, Some(key)) => id_from_key(key),
        (Source::Firefox, None) => format!("ff-{}", short_hash(info.gecko_id.as_deref().unwrap_or(&hint))),
        _ => format!("local-{}", short_hash(&format!("{}{}", info.name, hint))),
    };
    // Avec sa clé, une extension Chrome garde sous Windows le même identifiant
    // que dans Chrome : les sites qui lui parlent (Proton, Dashlane…) la retrouvent.
    if let Some(key) = &key
        && !info.has_key
    {
        add_key(&base, key)?;
    }
    let folder = format!("{id}-{}", stamp());
    let dest = root.join(&folder);
    fs::rename(tmp, &dest).map_err(|e| e.to_string())?;
    let dir = match base.strip_prefix(tmp) {
        Ok(sub) if !sub.as_os_str().is_empty() => format!("{folder}/{}", sub.to_string_lossy().replace('\\', "/")),
        _ => folder,
    };
    let firefox = info.gecko_id.is_some() && (source == Source::Firefox || hint.ends_with(".xpi"));
    let origin = matches!(source, Source::Chrome | Source::Firefox).then(|| hint.clone());
    Ok(Extension {
        id,
        name: info.name,
        description: info.description,
        version: info.version,
        source,
        enabled: true,
        dir,
        popup: info.popup,
        options: info.options,
        icon: info.icon,
        all_sites: info.all_sites,
        firefox,
        engine_id: None,
        origin,
        permissions: info.permissions,
        hosts: info.hosts,
        optional: info.optional,
    })
}

fn short_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Identifiant Chrome d'une clé publique : les 16 premiers octets de son
/// empreinte SHA-256, chaque demi-octet écrit de « a » à « p ».
pub fn id_from_key(key: &[u8]) -> String {
    Sha256::digest(key)
        .iter()
        .take(16)
        .flat_map(|b| [b >> 4, b & 15])
        .map(|n| (b'a' + n) as char)
        .collect()
}

/// Sépare une archive d'extension en zip et clé publique (fichiers `.crx`).
/// Un `.xpi` ou un `.zip` est déjà un zip, sans clé.
pub fn crx_zip(bytes: &[u8]) -> Result<(&[u8], Option<Vec<u8>>), String> {
    const BAD: &str = "ce fichier n'est pas une extension (.crx, .xpi ou .zip)";
    if bytes.starts_with(b"PK\x03\x04") {
        return Ok((bytes, None));
    }
    if !bytes.starts_with(b"Cr24") || bytes.len() < 16 {
        return Err(BAD.into());
    }
    let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let (zip_start, key) = match u32_at(4) {
        Some(2) => {
            let (klen, slen) = (u32_at(8).ok_or(BAD)?, u32_at(12).ok_or(BAD)?);
            let key = bytes.get(16..16 + klen).ok_or(BAD)?.to_vec();
            (16 + klen + slen, Some(key))
        }
        Some(3) => {
            let hlen = u32_at(8).ok_or(BAD)?;
            let header = bytes.get(12..12 + hlen).ok_or(BAD)?;
            (12 + hlen, crx3_key(header))
        }
        _ => return Err(BAD.into()),
    };
    let zip = bytes.get(zip_start..).filter(|z| z.starts_with(b"PK\x03\x04")).ok_or(BAD)?;
    Ok((zip, key))
}

/// Champs « longueur + octets » d'un message protobuf (les autres sont sautés).
fn proto_fields(mut buf: &[u8]) -> Vec<(u64, &[u8])> {
    fn varint(buf: &mut &[u8]) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = buf.split_first()?;
            *buf = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
    let mut fields = Vec::new();
    while !buf.is_empty() {
        let Some(tag) = varint(&mut buf) else { break };
        let skip = match tag & 7 {
            0 => {
                if varint(&mut buf).is_none() {
                    break;
                }
                0
            }
            1 => 8,
            5 => 4,
            2 => {
                let Some(len) = varint(&mut buf).and_then(|l| usize::try_from(l).ok()) else { break };
                let Some(value) = buf.get(..len) else { break };
                fields.push((tag >> 3, value));
                len
            }
            _ => break,
        };
        let Some(rest) = buf.get(skip..) else { break };
        buf = rest;
    }
    fields
}

/// Clé publique de l'en-tête d'un `.crx` (version 3) : celle qui correspond à
/// l'identifiant signé (`CrxFileHeader` : 2 = preuves RSA, 10000 = données signées).
fn crx3_key(header: &[u8]) -> Option<Vec<u8>> {
    let mut keys = Vec::new();
    let mut crx_id = None;
    for (field, value) in proto_fields(header) {
        match field {
            2 => keys.extend(proto_fields(value).into_iter().find(|(f, _)| *f == 1).map(|(_, k)| k)),
            10000 => crx_id = proto_fields(value).into_iter().find(|(f, _)| *f == 1).map(|(_, id)| id),
            _ => {}
        }
    }
    let key = match crx_id {
        Some(id) => keys.into_iter().find(|k| Sha256::digest(k).get(..16) == Some(id))?,
        None => keys.into_iter().next()?,
    };
    Some(key.to_vec())
}

/// Écrit le zip à côté, le décompresse dans `dest` et renvoie la clé du `.crx`.
fn unpack(bytes: &[u8], dest: &Path) -> Result<Option<Vec<u8>>, String> {
    let (zip, key) = crx_zip(bytes)?;
    let zip_path = dest.with_extension("zip");
    fs::write(&zip_path, zip).map_err(|e| e.to_string())?;
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let result = extract(&zip_path, dest);
    let _ = fs::remove_file(&zip_path);
    result.map(|()| key)
}

/// Décompresse avec l'outil du système. `tar` (bsdtar) refuse les chemins
/// absolus et les « .. » ; `check_tree` vérifie ensuite ce qui est sorti.
fn extract(zip: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let mut cmd = {
        let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut cmd = crate::network::command(&system.join("System32").join("tar.exe").to_string_lossy());
        cmd.arg("-xf").arg(zip).arg("-C").arg(dest);
        cmd
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut cmd = crate::network::command("/usr/bin/ditto");
        cmd.args(["-x", "-k"]).arg(zip).arg(dest);
        cmd
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = {
        let mut cmd = crate::network::command("unzip");
        cmd.args(["-q", "-o"]).arg(zip).arg("-d").arg(dest);
        cmd
    };
    let out = cmd.output().map_err(|e| format!("décompression impossible : {e}"))?;
    if !out.status.success() {
        return Err(format!("archive illisible ({})", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Refuse les liens symboliques (ils pourraient viser hors du dossier) et les
/// archives démesurées.
fn check_tree(dir: &Path) -> Result<(), String> {
    let mut total = 0u64;
    let mut count = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("l'archive contient un lien symbolique".into());
            }
            count += 1;
            if kind.is_dir() {
                stack.push(entry.path());
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
            if total > MAX_UNPACKED || count > MAX_FILES {
                return Err("extension trop grosse".into());
            }
        }
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Le dossier qui contient `manifest.json` : la racine, ou son seul sous-dossier.
fn manifest_root(dir: &Path) -> Result<PathBuf, String> {
    if dir.join("manifest.json").is_file() {
        return Ok(dir.to_path_buf());
    }
    let entries: Vec<_> = fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().collect();
    if let [only] = entries.as_slice()
        && only.path().join("manifest.json").is_file()
    {
        return Ok(only.path());
    }
    Err("pas de manifest.json : ce n'est pas une extension".into())
}

/// Ce que Feather retient du `manifest.json`.
#[derive(Debug, Default, PartialEq)]
pub struct Info {
    pub name: String,
    pub description: String,
    pub version: String,
    pub popup: Option<String>,
    pub options: Option<String>,
    pub icon: Option<String>,
    pub all_sites: bool,
    pub gecko_id: Option<String>,
    pub has_key: bool,
    /// Autorisations (`tabs`, `cookies`…), sites, et autorisations facultatives.
    pub permissions: Vec<String>,
    pub hosts: Vec<String>,
    pub optional: Vec<String>,
}

/// Retire les commentaires `//` et `/* */` (Chrome les tolère dans le manifeste).
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for n in chars.by_ref() {
                    if last == '*' && n == '/' {
                        break;
                    }
                    last = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim_start_matches('\u{feff}');
    serde_json::from_str(text).or_else(|_| serde_json::from_str(&strip_comments(text))).ok()
}

/// Remplace `__MSG_nom__` par le texte traduit (français d'abord).
fn localize(text: &str, dir: &Path, default_locale: Option<&str>) -> String {
    let Some(key) = text.strip_prefix("__MSG_").and_then(|t| t.strip_suffix("__")) else {
        return text.to_string();
    };
    let key = key.to_lowercase();
    for locale in ["fr", "fr_FR"].into_iter().chain(default_locale).chain(["en", "en_US"]) {
        let Some(Value::Object(messages)) = read_json(&dir.join("_locales").join(locale).join("messages.json")) else {
            continue;
        };
        if let Some(message) = messages
            .iter()
            .find(|(k, _)| k.to_lowercase() == key)
            .and_then(|(_, v)| v.get("message"))
            .and_then(Value::as_str)
        {
            return message.to_string();
        }
    }
    text.to_string()
}

/// Un chemin du manifeste, s'il reste dans le dossier de l'extension.
fn inner_path(dir: &Path, path: Option<&str>) -> Option<String> {
    let path = path?.trim().trim_start_matches('/');
    let clean = path.split(['?', '#']).next()?;
    if clean.is_empty() || clean.split(['/', '\\']).any(|p| p == "..") || !dir.join(clean).is_file() {
        return None;
    }
    Some(path.to_string())
}

/// L'icône la mieux adaptée à un bouton (au moins 32 pixels, la plus petite).
fn pick_icon(icons: Option<&Value>) -> Option<String> {
    match icons? {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => {
            let mut sizes: Vec<(u32, &str)> =
                map.iter().filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?))).collect();
            sizes.sort();
            sizes.iter().find(|(s, _)| *s >= 32).or(sizes.last()).map(|(_, p)| p.to_string())
        }
        _ => None,
    }
}

pub fn read_manifest(dir: &Path) -> Result<Info, String> {
    let manifest = read_json(&dir.join("manifest.json")).ok_or("manifest.json illisible")?;
    let locale = manifest.get("default_locale").and_then(Value::as_str);
    let text = |key: &str| manifest.get(key).and_then(Value::as_str).map(|t| localize(t, dir, locale));
    let name = text("name").filter(|n| !n.trim().is_empty()).ok_or("manifest.json sans nom")?;
    let action = ["action", "browser_action", "page_action"].iter().find_map(|k| manifest.get(*k));
    let popup = inner_path(dir, action.and_then(|a| a.get("default_popup")).and_then(Value::as_str));
    let options = inner_path(
        dir,
        manifest.pointer("/options_ui/page").or_else(|| manifest.get("options_page")).and_then(Value::as_str),
    );
    let icon = pick_icon(action.and_then(|a| a.get("default_icon")))
        .and_then(|p| inner_path(dir, Some(&p)))
        .or_else(|| pick_icon(manifest.get("icons")).and_then(|p| inner_path(dir, Some(&p))));
    let all = ["<all_urls>", "*://*/*", "http://*/*", "https://*/*"];
    let lists = ["/permissions", "/host_permissions"]
        .iter()
        .filter_map(|p| manifest.pointer(p).and_then(Value::as_array))
        .flatten()
        .chain(
            manifest
                .get("content_scripts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| c.get("matches").and_then(Value::as_array))
                .flatten(),
        );
    let all_sites = lists.filter_map(Value::as_str).any(|p| all.contains(&p));
    // Les sites sont rangés à part : dans un manifeste version 2, ils sont mêlés
    // aux autorisations (`"https://*.proton.me/*"`).
    let strings = |pointers: &[&str]| -> Vec<String> {
        let mut out: Vec<String> = pointers
            .iter()
            .filter_map(|p| manifest.pointer(p).and_then(Value::as_array))
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        out.dedup();
        out
    };
    let is_host = |p: &String| p.contains("://") || p == "<all_urls>";
    let declared = strings(&["/permissions"]);
    let mut hosts: Vec<String> = declared.iter().filter(|p| is_host(p)).cloned().collect();
    hosts.extend(strings(&["/host_permissions"]));
    for script in manifest.get("content_scripts").and_then(Value::as_array).into_iter().flatten() {
        hosts.extend(script.get("matches").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string));
    }
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|h| seen.insert(h.clone()));
    let permissions = declared.into_iter().filter(|p| !is_host(p)).collect();
    let optional = strings(&["/optional_permissions", "/optional_host_permissions"]);
    let gecko_id = ["/browser_specific_settings/gecko", "/applications/gecko"]
        .iter()
        .find_map(|p| manifest.pointer(p))
        .map(|g| g.get("id").and_then(Value::as_str).unwrap_or("").to_string());
    let mut description = text("description").unwrap_or_default();
    if description.chars().count() > 160 {
        description = description.chars().take(157).collect::<String>() + "…";
    }
    Ok(Info {
        name: name.chars().take(80).collect(),
        description,
        version: text("version").unwrap_or_default(),
        popup,
        options,
        icon,
        all_sites,
        gecko_id,
        has_key: manifest.get("key").is_some(),
        permissions,
        hosts,
        optional,
    })
}

/// Ajoute la clé publique au manifeste (champ `key`, en base64).
fn add_key(dir: &Path, key: &[u8]) -> Result<(), String> {
    let path = dir.join("manifest.json");
    let Some(Value::Object(mut manifest)) = read_json(&path) else {
        return Err("manifest.json illisible".into());
    };
    manifest.insert("key".into(), Value::String(base64_encode(key)));
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

/// Compléments chargés avant l'arrière-plan des extensions (voir le fichier).
const COMPAT_JS: &str = include_str!("../assets/extension-compat.js");
const COMPAT_FILE: &str = "feather-compat.js";
/// Arrière-plan de remplacement : les compléments, puis celui de l'extension.
const WRAPPER_FILE: &str = "feather-background.js";

/// Sur Mac, fait passer l'arrière-plan de l'extension après les compléments de
/// Feather. Le manifeste désigne alors `feather-background.js`, qui charge
/// `feather-compat.js` puis le fichier d'origine. Ce fichier est réécrit à
/// chaque chargement (une nouvelle version de Feather y met la sienne) ; le
/// manifeste, lui, n'est modifié qu'une fois.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn add_compat(dir: &Path) -> Result<(), String> {
    let path = dir.join("manifest.json");
    let Some(Value::Object(mut manifest)) = read_json(&path) else {
        return Err("manifest.json illisible".into());
    };
    fs::write(dir.join(COMPAT_FILE), COMPAT_JS).map_err(|e| e.to_string())?;
    // La fenêtre (popup) et la page d'options reçoivent aussi les compléments.
    if let Ok(info) = read_manifest(dir) {
        for page in [info.popup, info.options].into_iter().flatten() {
            if let Some(file) = page.split(['?', '#']).next() {
                compat_page(&dir.join(file))?;
            }
        }
    }
    let Some(Value::Object(background)) = manifest.get_mut("background") else { return Ok(()) };
    if let Some(worker) = background.get("service_worker").and_then(Value::as_str) {
        let worker = worker.trim_start_matches(['.', '/']);
        // Déjà enveloppé à un lancement précédent : l'original est relu dans l'enveloppe.
        let wrapped = worker == WRAPPER_FILE;
        let original = if wrapped {
            let text = fs::read_to_string(dir.join(WRAPPER_FILE)).map_err(|e| e.to_string())?;
            wrapped_original(&text).ok_or("arrière-plan d'origine introuvable dans feather-background.js")?
        } else {
            format!("/{worker}")
        };
        let module = background.get("type").and_then(Value::as_str) == Some("module");
        fs::write(dir.join(WRAPPER_FILE), wrapper_js(&original, module)?).map_err(|e| e.to_string())?;
        if wrapped {
            return Ok(());
        }
        background.insert("service_worker".into(), Value::String(WRAPPER_FILE.into()));
    } else if let Some(Value::Array(scripts)) = background.get_mut("scripts") {
        // Manifeste v2 (ou arrière-plan de Firefox) : les compléments en premier.
        if scripts.first().and_then(Value::as_str) == Some(COMPAT_FILE) {
            return Ok(());
        }
        scripts.insert(0, Value::String(COMPAT_FILE.into()));
    } else {
        return Ok(());
    }
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

/// Contenu de `feather-background.js` : les compléments, puis l'arrière-plan
/// d'origine (`original` : chemin depuis la racine de l'extension).
///
/// Arrière-plan classique (Proton Pass) : une erreur au premier niveau du
/// fichier d'origine ne l'empêche plus de s'installer. WebKit le refusait
/// entier, et l'extension ne répondait plus à rien (« difficulties
/// activating ») ; ce qui a été préparé avant l'erreur marche désormais, et
/// l'erreur reste signalée (console, liste des erreurs du moteur, panneau).
/// Un module ne peut pas être chargé ainsi : `import()` est interdit dans un
/// service worker.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn wrapper_js(original: &str, module: bool) -> Result<String, String> {
    // Échappé pour JavaScript ; premier guillemet double du fichier (voir `wrapped_original`).
    let original = serde_json::to_string(original).map_err(|e| e.to_string())?;
    // WebKit garde avec l'arrière-plan les fichiers qu'il charge : l'empreinte
    // des compléments change ce fichier-ci quand ils changent, et l'arrière-plan
    // est alors réinstallé avec les nouveaux.
    let version = short_hash(COMPAT_JS);
    Ok(if module {
        format!("// Compléments de Feather {version}\nimport '/{COMPAT_FILE}';\nimport {original};\n")
    } else {
        format!(
            r"// Compléments de Feather {version}
importScripts('/{COMPAT_FILE}');
try {{
  importScripts({original});
}} catch (e) {{
  // Filet de Feather : l'arrière-plan s'installe avec ce qu'il a préparé avant l'erreur.
  if (typeof reportError === 'function') reportError(e); else console.error(e);
}}
"
        )
    })
}

/// L'arrière-plan d'origine noté dans `feather-background.js` : le premier
/// texte entre guillemets doubles, dans toutes les versions de l'enveloppe.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn wrapped_original(wrapper: &str) -> Option<String> {
    let start = wrapper.find('"')?;
    serde_json::Deserializer::from_str(&wrapper[start..]).into_iter::<String>().next()?.ok()
}

/// Balise qui charge les compléments en tête d'une page de l'extension.
const COMPAT_TAG: &str = "<script src=\"/feather-compat.js\"></script>";

/// Ajoute les compléments en premier script d'une page (juste après `<head>`,
/// sinon au tout début). Sans effet la deuxième fois.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn compat_page(path: &Path) -> Result<(), String> {
    let html = fs::read_to_string(path).map_err(|e| e.to_string())?;
    if html.contains(COMPAT_TAG) {
        return Ok(());
    }
    let lower = html.to_ascii_lowercase();
    // `<head>` ou `<head …>`, pas `<header>`.
    let at = lower
        .match_indices("<head")
        .find(|(i, _)| matches!(lower.as_bytes().get(i + 5), Some(b'>' | b' ' | b'\t' | b'\n' | b'\r')))
        .and_then(|(i, _)| lower[i..].find('>').map(|end| i + end + 1))
        .unwrap_or(0);
    let text = format!("{}{COMPAT_TAG}{}", &html[..at], &html[at..]);
    fs::write(path, text).map_err(|e| e.to_string())
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Décode du base64 (fichier choisi dans le panneau, envoyé en `data:` URL).
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for b in text.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if b == b'=' {
            break;
        }
        let v = B64.iter().position(|&c| c == b)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Icône en `data:` URL, pour l'interface (qui ne lit pas les fichiers).
fn icon_data(ext: &Extension) -> Option<String> {
    let path = ext.path()?.join(ext.icon.as_ref()?);
    if fs::metadata(&path).ok()?.len() > MAX_ICON {
        return None;
    }
    let mime = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    };
    Some(format!("data:{mime};base64,{}", base64_encode(&fs::read(path).ok()?)))
}

/// Ce que veut dire une autorisation, en clair. `None` : sans conséquence pour
/// l'utilisateur (stockage, minuteries, menus…), on ne la montre pas.
pub fn permission_label(permission: &str) -> Option<String> {
    if permission.contains("://") || permission == "<all_urls>" {
        return Some("Lire et modifier d'autres sites".into());
    }
    let text = match permission {
        "tabs" => "Voir vos onglets et leurs adresses",
        "history" => "Lire et effacer votre historique",
        "bookmarks" => "Lire et modifier vos favoris",
        "topSites" => "Voir les sites que vous visitez le plus",
        "cookies" => "Lire et modifier les cookies des sites",
        "webRequest" | "webRequestBlocking" => "Observer les requêtes des pages",
        "declarativeNetRequest" | "declarativeNetRequestWithHostAccess" => "Bloquer ou modifier des requêtes",
        "declarativeNetRequestFeedback" => "Voir les requêtes qu'elle a bloquées",
        "webNavigation" => "Suivre les pages que vous visitez",
        "scripting" | "userScripts" => "Ajouter des scripts dans les pages",
        "clipboardRead" => "Lire le presse-papiers",
        "clipboardWrite" => "Écrire dans le presse-papiers",
        "downloads" => "Gérer vos téléchargements",
        "nativeMessaging" => "Parler à une application de l'ordinateur",
        "notifications" => "Afficher des notifications",
        "geolocation" => "Connaître votre position",
        "management" => "Gérer vos autres extensions",
        "privacy" => "Changer les réglages de confidentialité",
        "proxy" => "Faire passer votre navigation par un autre serveur",
        "debugger" => "Contrôler entièrement les pages (débogueur)",
        "identity" => "Vous connecter avec votre compte",
        "browsingData" => "Effacer vos données de navigation",
        "pageCapture" | "tabCapture" | "desktopCapture" => "Capturer le contenu des pages ou de l'écran",
        "contentSettings" => "Changer les autorisations des sites",
        "storage" | "unlimitedStorage" | "alarms" | "contextMenus" | "menus" | "activeTab" | "offscreen" | "idle"
        | "sidePanel" | "commands" | "runtime" | "i18n" | "power" | "fontSettings" | "tts" | "search" | "sessions"
        | "background" => return None,
        // Inconnue : montrée telle quelle, plutôt que cachée.
        other => return Some(other.chars().take(60).collect()),
    };
    Some(text.to_string())
}

/// Ce que montre la fiche, sans doublons (`webRequest` et `webRequestBlocking`…).
fn labels(permissions: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for label in permissions.iter().filter_map(|p| permission_label(p)) {
        if !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

/// Les sites d'une liste de motifs (`https://*.proton.me/*` → `*.proton.me`).
pub fn site_labels(hosts: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for h in hosts {
        let host = h.split_once("://").map_or(h.as_str(), |(_, rest)| rest);
        let host = host.split('/').next().unwrap_or(host).to_string();
        if !host.is_empty() && !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// Site qu'une extension active déclare pour lui parler depuis ses pages
/// (`externally_connectable`, comme `https://*.proton.me/*` pour Proton Pass).
#[derive(Debug, PartialEq)]
struct Declared {
    /// `https`, `http`, ou `*` pour les deux.
    scheme: String,
    host: String,
    /// `*.proton.me` : le domaine et tous ses sous-domaines.
    subdomains: bool,
}

/// Sites déclarés par les extensions actives (voir `declared_site`).
static DECLARED: RwLock<Vec<Declared>> = RwLock::new(Vec::new());

/// Lit un motif de `externally_connectable.matches`. Les motifs trop larges
/// (tous les sites, `*.com`) sont ignorés, comme dans Chrome.
fn declared(pattern: &str) -> Option<Declared> {
    let (scheme, rest) = pattern.split_once("://")?;
    if !matches!(scheme, "https" | "http" | "*") {
        return None;
    }
    let host = rest.split('/').next()?.split(':').next()?.to_ascii_lowercase();
    let (subdomains, host) = match host.strip_prefix("*.") {
        Some(base) => (true, base.to_string()),
        None => (false, host),
    };
    if host.contains('*') || !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
        return None;
    }
    Some(Declared { scheme: scheme.into(), host, subdomains })
}

/// Vrai si une extension active a déclaré ce site pour lui parler. Sous macOS,
/// ces sites reçoivent l'identité Safari (voir `tab_manager::wants_safari`) :
/// la page de connexion de Proton (account.proton.me) ne parle à l'extension
/// que dans Chrome ou Safari. Avec l'identité de WebKit seule, elle attendait
/// une réponse par un autre chemin, qui ne venait jamais (« Extension timed
/// out » au bout de 15 s), et la connexion à Proton Pass échouait.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn declared_site(url: &str) -> bool {
    DECLARED.read().is_ok_and(|list| site_matches(&list, url))
}

/// Vrai si l'adresse est sur l'un des sites de `list`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn site_matches(list: &[Declared], url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else { return false };
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else { return false };
    let scheme = url.scheme();
    list.iter().any(|d| {
        (d.scheme == scheme || (d.scheme == "*" && matches!(scheme, "http" | "https")))
            && (host == d.host || (d.subdomains && host.strip_suffix(d.host.as_str()).is_some_and(|sub| sub.ends_with('.'))))
    })
}

/// `true` si `new` est une version plus récente que `old` (« 1.10.2 » > « 1.9 »).
pub fn is_newer(new: &str, old: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split(['.', '-']).map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parts(new), parts(old));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Version publiée par le Chrome Web Store, lue dans sa réponse XML
/// (`<updatecheck … version="1.2.3" status="ok"/>`).
fn chrome_latest(xml: &str) -> Option<String> {
    let check = &xml[xml.find("<updatecheck")?..];
    let check = &check[..check.find('>')?];
    if !check.contains("status=\"ok\"") {
        return None;
    }
    let start = check.find(" version=\"")? + 10;
    let version = &check[start..];
    Some(version[..version.find('"')?].to_string())
}

/// Dernière version publiée de l'extension (requête bloquante, https seulement).
fn latest_version(origin: &Origin) -> Option<String> {
    let url = match origin {
        Origin::Chrome(id) => format!(
            "https://clients2.google.com/service/update2/crx?response=updatecheck&prodversion={CHROME_VERSION}&acceptformat=crx3&x=id%3D{id}%26uc"
        ),
        Origin::Firefox(slug) => {
            let slug: String = url::form_urlencoded::byte_serialize(slug.as_bytes()).collect();
            format!("https://addons.mozilla.org/api/v5/addons/addon/{slug}/")
        }
        Origin::Folder(_) => return None,
    };
    let mut cmd = crate::network::command("curl");
    cmd.args(["-sSL", "--fail", "--max-time", "30", "--proto", "=https", "--proto-redir", "=https", "--max-filesize", "2000000"])
        .arg(url);
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let body = String::from_utf8_lossy(&out.stdout);
    match origin {
        Origin::Chrome(_) => chrome_latest(&body),
        _ => serde_json::from_str::<Value>(&body)
            .ok()?
            .pointer("/current_version/version")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Une semaine entre deux recherches de nouvelles versions.
const UPDATE_EVERY: u64 = 7 * 24 * 3600;
/// Un bloqueur tout juste installé (ou mis à jour) prépare ses listes :
/// uBlock Origin Lite met environ 30 secondes. Le bloqueur intégré reste actif
/// en attendant.
const BLOCKER_WARMUP: std::time::Duration = std::time::Duration::from_secs(45);

/// Erreurs du moteur gardées par extension (voir `extension_errors`).
const MAX_ERRORS: usize = 20;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// Extension téléchargée qui attend l'accord de l'utilisateur : la fiche du
/// panneau montre ce qu'elle pourra lire et faire.
pub struct Pending {
    pub ext: Extension,
    /// Mise à jour d'une extension déjà installée (qui demande plus qu'avant).
    pub update: bool,
}

/// Les extensions installées, et ce que montre le panneau.
#[derive(Default)]
pub struct Extensions {
    list: Vec<Extension>,
    icons: HashMap<String, String>,
    /// Ce que le moteur a refusé, par extension.
    problems: HashMap<String, String>,
    /// Erreurs notées par le moteur pendant que l'extension tourne (macOS :
    /// arrière-plan refusé, erreur de script…), montrées dans le panneau.
    errors: HashMap<String, Vec<String>>,
    /// Chargées par le moteur, et depuis quand (voir `ad_blocker`).
    loaded: HashMap<String, std::time::Instant>,
    /// Installées ou mises à jour pendant cette session : leur bloqueur se prépare.
    fresh: std::collections::HashSet<String>,
    /// Extension à confirmer (voir `Pending`).
    pub pending: Option<Pending>,
    /// Dernière recherche de nouvelles versions (secondes depuis 1970).
    checked: u64,
    /// Recherche de nouvelles versions en cours.
    pub checking: bool,
    /// Installation en cours (texte affiché), dernière erreur.
    pub busy: Option<String>,
    pub error: Option<String>,
    /// Petite fenêtre ouverte : l'extension et (Windows) sa webview.
    pub popup: Option<Popup>,
    /// Déjà confiées au moteur (voir `start_extensions`).
    started: bool,
}

pub struct Popup {
    pub id: String,
    #[cfg(windows)]
    pub webview: wry::WebView,
    /// Coin haut gauche de la fenêtre, gardé quand elle change de taille.
    #[cfg(windows)]
    pub anchor: (f64, f64),
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    list: Vec<Extension>,
    #[serde(default)]
    checked: u64,
}

impl Extensions {
    pub fn load() -> Self {
        let mut saved: Saved = storage::read_json(FILE).unwrap_or_default();
        saved.list.retain(|e| e.path().is_some_and(|p| p.join("manifest.json").is_file()));
        // Installées avant 0.5.8 : leurs autorisations sont relues dans le manifeste,
        // pour qu'une mise à jour ne demande que ce qui est vraiment nouveau.
        for ext in &mut saved.list {
            if ext.permissions.is_empty()
                && ext.hosts.is_empty()
                && ext.optional.is_empty()
                && let Some(info) = ext.path().and_then(|p| read_manifest(&p).ok())
            {
                ext.permissions = info.permissions;
                ext.hosts = info.hosts;
                ext.optional = info.optional;
            }
        }
        let mut me = Self { list: saved.list, checked: saved.checked, ..Self::default() };
        for ext in &me.list {
            if let Some(icon) = icon_data(ext) {
                me.icons.insert(ext.id.clone(), icon);
            }
        }
        me.sweep();
        me.sync_declared();
        me
    }

    pub fn save(&self) {
        let saved = Saved { list: self.list.clone(), checked: self.checked };
        if let Err(err) = storage::write_json(FILE, &saved, true) {
            eprintln!("liste des extensions non enregistrée : {err}");
        }
    }

    /// Efface les dossiers qui ne servent plus (anciennes versions, essais ratés).
    fn sweep(&self) {
        let Some(root) = root() else { return };
        let Ok(entries) = fs::read_dir(&root) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let used = self.list.iter().any(|e| e.dir.split('/').next() == Some(name.as_str()));
            if !used {
                let _ = if entry.path().is_dir() { fs::remove_dir_all(entry.path()) } else { fs::remove_file(entry.path()) };
            }
        }
    }

    pub fn list(&self) -> &[Extension] {
        &self.list
    }

    pub fn get(&self, id: &str) -> Option<&Extension> {
        self.list.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Extension> {
        self.list.iter_mut().find(|e| e.id == id)
    }

    /// Ajoute (ou remplace) une extension ; renvoie l'ancienne version.
    pub fn insert(&mut self, ext: Extension) -> Option<Extension> {
        self.problems.remove(&ext.id);
        self.errors.remove(&ext.id);
        match icon_data(&ext) {
            Some(icon) => self.icons.insert(ext.id.clone(), icon),
            None => self.icons.remove(&ext.id),
        };
        let old = match self.list.iter().position(|e| e.id == ext.id) {
            Some(i) => Some(std::mem::replace(&mut self.list[i], ext)),
            None => {
                self.list.push(ext);
                None
            }
        };
        self.save();
        self.sync_declared();
        old
    }

    pub fn remove(&mut self, id: &str) -> Option<Extension> {
        let i = self.list.iter().position(|e| e.id == id)?;
        self.icons.remove(id);
        self.problems.remove(id);
        self.errors.remove(id);
        let ext = self.list.remove(i);
        self.save();
        self.sync_declared();
        Some(ext)
    }

    /// Relit les sites déclarés par les extensions actives (voir `declared_site`) :
    /// seul le Mac s'en sert.
    fn sync_declared(&self) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let sites = self
            .list
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| read_json(&e.path()?.join("manifest.json")))
            .flat_map(|manifest| {
                let matches = manifest.pointer("/externally_connectable/matches").and_then(Value::as_array).cloned();
                matches.into_iter().flatten().filter_map(|p| declared(p.as_str()?))
            })
            .collect();
        if let Ok(mut list) = DECLARED.write() {
            *list = sites;
        }
    }

    pub fn set_problem(&mut self, id: &str, problem: Option<String>) {
        match problem {
            Some(p) => self.problems.insert(id.to_string(), p),
            None => self.problems.remove(id),
        };
    }

    /// Nom du bloqueur de publicités qui agit sur toutes les pages : activé,
    /// chargé sans erreur par le moteur, et prêt. Le bloqueur intégré lui laisse
    /// la place. Avant, il se mettait en pause dès l'activation : rien ne
    /// bloquait pendant que l'extension préparait ses listes, ou si elle ne se
    /// chargeait pas.
    pub fn ad_blocker(&self) -> Option<&str> {
        if support().is_err() {
            return None;
        }
        let ready = |e: &Extension| {
            self.loaded
                .get(&e.id)
                .is_some_and(|since| !self.fresh.contains(&e.id) || since.elapsed() >= BLOCKER_WARMUP)
        };
        self.list
            .iter()
            .find(|e| e.enabled && e.all_sites && blocks_ads(e) && !self.problems.contains_key(&e.id) && ready(e))
            .map(|e| e.name.as_str())
    }

    /// Le moteur a chargé l'extension (ou l'a refusée).
    fn set_loaded(&mut self, id: &str, ok: bool) {
        if ok {
            self.loaded.entry(id.to_string()).or_insert_with(std::time::Instant::now);
        } else {
            self.loaded.remove(id);
        }
    }

    pub fn popup_id(&self) -> Option<&str> {
        self.popup.as_ref().map(|p| p.id.as_str())
    }

    /// Ce que l'interface reçoit : le panneau et les boutons de la colonne.
    pub fn snapshot(&self) -> Value {
        let list: Vec<Value> = self
            .list
            .iter()
            .map(|e| {
                json!({
                    "id": e.id,
                    "name": e.name,
                    "description": e.description,
                    "version": e.version,
                    "source": e.source,
                    "enabled": e.enabled,
                    "popup": e.popup.is_some(),
                    "options": e.options.is_some(),
                    "icon": self.icons.get(&e.id),
                    "all_sites": e.all_sites,
                    "firefox": e.firefox,
                    "problem": self.problems.get(&e.id),
                    "errors": self.errors.get(&e.id),
                })
            })
            .collect();
        json!({
            "supported": support().is_ok(),
            "reason": support().err(),
            "platform": engine::PLATFORM,
            "busy": self.busy,
            "error": self.error,
            "popup": self.popup_id(),
            "list": list,
            "pending": self.pending.as_ref().map(|p| {
                let e = &p.ext;
                json!({
                    "name": e.name,
                    "version": e.version,
                    "update": p.update,
                    "icon": icon_data(e),
                    "all_sites": e.all_sites,
                    "sites": if e.all_sites { Vec::new() } else { site_labels(&e.hosts) },
                    "permissions": labels(&e.permissions),
                    "optional": labels(&e.optional),
                })
            }),
        })
    }
}

/// Prépare une webview pour les extensions. Sous Windows, toutes les webviews
/// doivent avoir le même réglage (sinon WebView2 refuse de les créer) ; sous
/// macOS, seules les pages non privées (`attach`) reçoivent les extensions, et
/// une page d'une extension (`url` : ses options…) prend la configuration de celle-ci.
pub fn prepare_builder<'a>(builder: WebViewBuilder<'a>, attach: bool, url: &str) -> WebViewBuilder<'a> {
    engine::prepare_builder(builder, attach, url)
}

pub use engine::{add, close_popup, open_popup, options_url, page_url, popup_shown, remove, replace, set_enabled, start, sync_tabs};

/// Un onglet non privé tel que les extensions le voient (macOS, voir `sync_tabs`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct TabView<'a> {
    pub id: u32,
    pub title: &'a str,
    pub url: &'a str,
    pub loading: bool,
    pub pinned: bool,
    pub audible: bool,
    pub muted: bool,
    pub active: bool,
    pub webview: Option<&'a wry::WebView>,
}

/// Adresse qu'une extension peut ouvrir dans un onglet : une page web, ou une
/// page de l'extension elle-même (Windows : chrome-extension, macOS : webkit-extension).
pub fn opens_in_tab(url: &str) -> bool {
    crate::network::is_web(url) || url.starts_with("chrome-extension://") || url.starts_with("webkit-extension://")
}

/// Ce que la petite fenêtre (Windows) envoie : sa taille, ou sa fermeture.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct PopupMessage {
    #[serde(default)]
    pub close: bool,
    pub w: Option<f64>,
    pub h: Option<f64>,
}

/// Script de la petite fenêtre (Windows) : `window.close()` et Échap la ferment,
/// et elle donne la taille de son contenu, comme dans Chrome.
#[cfg_attr(not(windows), allow(dead_code))]
pub const POPUP_SCRIPT: &str = r#"
(() => {
  if (window.top !== window) return;
  const post = (m) => { try { window.ipc.postMessage(JSON.stringify(m)); } catch (e) {} };
  window.close = () => post({ close: true });
  addEventListener('keydown', (e) => { if (e.key === 'Escape') post({ close: true }); }, true);
  let last = '';
  const size = () => {
    const b = document.body;
    if (!b) return;
    const r = b.getBoundingClientRect(), s = getComputedStyle(b);
    const w = Math.ceil(r.right + (parseFloat(s.marginRight) || 0));
    const h = Math.ceil(r.bottom + (parseFloat(s.marginBottom) || 0));
    const key = w + 'x' + h;
    if (key !== last && w > 0 && h > 0) { last = key; post({ w, h }); }
  };
  addEventListener('load', () => { size(); new ResizeObserver(size).observe(document.documentElement); });
})();
"#;

/// Où et à quelle taille poser la petite fenêtre, à côté du bouton de
/// l'extension (`button` : x, y, largeur, hauteur), dans une fenêtre `win`.
pub fn popup_frame(button: (f64, f64, f64, f64), size: (f64, f64), win: (f64, f64), bar: &str) -> (f64, f64, f64, f64) {
    let (bx, by, bw, bh) = button;
    let w = size.0.clamp(120.0, 800.0).min(win.0 - 16.0).max(1.0);
    let h = size.1.clamp(60.0, 600.0).min(win.1 - 16.0).max(1.0);
    let (x, y) = match bar {
        "right" => (bx - 8.0 - w, by),
        "top" => (bx + bw - w, by + bh + 8.0),
        _ => (bx + bw + 8.0, by),
    };
    let x = x.clamp(8.0, (win.0 - w - 8.0).max(8.0));
    let y = y.clamp(8.0, (win.1 - h - 8.0).max(8.0));
    (x, y, w, h)
}

/// Ce que le panneau demande (voir `ChromeMessage`), côté `App`.
impl crate::App {
    /// Lien collé dans le panneau : téléchargement et décompression en arrière-plan.
    pub(crate) fn install_extension(&mut self, input: &str) {
        match parse_input(input) {
            Ok(origin) => {
                let busy = match &origin {
                    Origin::Chrome(_) => "Téléchargement depuis le Chrome Web Store…",
                    Origin::Firefox(_) => "Téléchargement depuis Firefox Add-ons…",
                    Origin::Folder(_) => "Copie du dossier…",
                };
                self.start_prepare(Request::Origin(origin), busy);
            }
            Err(err) => self.extensions.error = Some(err),
        }
    }

    /// Fichier choisi dans le panneau, reçu en `data:` URL.
    pub(crate) fn install_extension_file(&mut self, name: &str, data: &str) {
        let bytes = data.split_once(',').map_or(data, |(_, b)| b);
        match base64_decode(bytes) {
            Some(bytes) if bytes.len() <= MAX_ARCHIVE => {
                self.start_prepare(Request::File { name: name.to_lowercase(), bytes }, "Installation du fichier…");
            }
            Some(_) => self.extensions.error = Some("Fichier trop gros.".into()),
            None => self.extensions.error = Some("Fichier illisible.".into()),
        }
    }

    /// Confie au moteur les extensions installées, une seule fois : après la
    /// première page chargée (ou 5 s après le lancement), et non plus avant de
    /// l'ouvrir. L'arrière-plan des extensions (Proton Pass…) ne retarde plus
    /// l'affichage ; le bloqueur intégré agit en attendant (voir `ad_blocker`).
    pub(crate) fn start_extensions(&mut self) {
        if self.extensions.started {
            return;
        }
        self.extensions.started = true;
        start(&self.chrome, self.extensions.list(), &self.proxy);
        // Les onglets déjà ouverts sont décrits aux extensions (macOS).
        self.push_state();
    }

    fn start_prepare(&mut self, request: Request, busy: &str) {
        if let Err(reason) = support() {
            self.extensions.error = Some(reason.into());
            return;
        }
        // Installation avant la première page : le moteur doit être prêt.
        self.start_extensions();
        if self.extensions.busy.is_some() {
            return;
        }
        self.extensions.busy = Some(busy.into());
        self.extensions.error = None;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = prepare(request).map(Box::new);
            let _ = proxy.send_event(crate::UserEvent::ExtensionPrepared(result));
        });
    }

    /// L'extension est rangée sur le disque : l'utilisateur voit ce qu'elle
    /// demande et confirme (voir `confirm_extension`).
    pub(crate) fn extension_prepared(&mut self, result: Result<Box<Extension>, String>) {
        self.extensions.busy = None;
        match result {
            Ok(ext) => self.extension_ready(*ext, false),
            Err(err) => self.extensions.error = Some(format!("Installation impossible : {err}.")),
        }
    }

    /// Nouvelle version téléchargée par la recherche hebdomadaire (voir
    /// `check_extension_updates`). Un échec ne s'affiche pas : on réessaiera.
    pub(crate) fn extension_updated(&mut self, result: Result<Box<Extension>, String>) {
        match result {
            Ok(ext) => self.extension_ready(*ext, true),
            Err(err) => eprintln!("mise à jour d'extension impossible : {err}"),
        }
    }

    fn extension_ready(&mut self, ext: Extension, automatic: bool) {
        let old = self.extensions.get(&ext.id).cloned();
        // Une mise à jour qui ne demande rien de plus s'installe seule ; une
        // installation, ou une mise à jour qui demande plus, attend l'accord.
        if let Some(old) = &old
            && !ext.asks_more_than(old)
        {
            self.install_prepared(ext);
            return;
        }
        if let Some(previous) = self.extensions.pending.take() {
            remove_folder_later(&previous.ext);
        }
        let name = ext.name.clone();
        self.extensions.pending = Some(Pending { ext, update: old.is_some() });
        if automatic {
            let text = serde_json::to_string(&format!(
                "La mise à jour de {name} demande de nouvelles autorisations : ouvrez Extensions pour l'accepter."
            ))
            .unwrap_or_default();
            self.chrome_call(&format!("navigateur.showNotice({text})"));
        }
    }

    /// Réponse de la fiche : installer l'extension en attente, ou l'abandonner.
    pub(crate) fn confirm_extension(&mut self, accept: bool) {
        let Some(pending) = self.extensions.pending.take() else { return };
        if accept {
            self.install_prepared(pending.ext);
        } else {
            remove_folder_later(&pending.ext);
        }
    }

    /// Confie au moteur une extension acceptée (nouvelle, ou nouvelle version).
    fn install_prepared(&mut self, mut ext: Extension) {
        if self.extensions.popup_id() == Some(ext.id.as_str()) {
            self.close_extension_popup();
        }
        if let Some(old) = self.extensions.get(&ext.id) {
            // Mise à jour : on garde le choix « activée » de l'ancienne version.
            ext.enabled = old.enabled;
        }
        // Un bloqueur prépare ses listes : le bloqueur intégré reste actif en attendant.
        self.extensions.fresh.insert(ext.id.clone());
        self.extensions.set_loaded(&ext.id, false);
        match self.extensions.insert(ext.clone()) {
            Some(old) => {
                replace(&old, &ext, &self.proxy);
                remove_folder_later(&old);
            }
            None => add(&ext, &self.proxy),
        }
        self.sync_ad_blocker();
    }

    /// Une fois par semaine, en arrière-plan : les extensions du Chrome Web Store
    /// et de Firefox Add-ons ont-elles une nouvelle version ? Avant, elles
    /// restaient figées, failles comprises.
    pub(crate) fn check_extension_updates(&mut self) {
        let due = now_secs().saturating_sub(self.extensions.checked) >= UPDATE_EVERY;
        if !due || self.extensions.checking || support().is_err() {
            return;
        }
        let wanted: Vec<(Origin, String)> =
            self.extensions.list.iter().filter_map(|e| Some((e.update_origin()?, e.version.clone()))).collect();
        self.extensions.checked = now_secs();
        self.extensions.save();
        if wanted.is_empty() {
            return;
        }
        self.extensions.checking = true;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            for (origin, version) in wanted {
                let Some(latest) = latest_version(&origin) else { continue };
                if !is_newer(&latest, &version) {
                    continue;
                }
                let result = prepare(Request::Origin(origin)).map(Box::new);
                let _ = proxy.send_event(crate::UserEvent::ExtensionUpdated(result));
            }
            let _ = proxy.send_event(crate::UserEvent::ExtensionUpdatesChecked);
        });
    }

    /// Un bloqueur de publicités en extension met le bloqueur intégré en pause
    /// (voir `adblock::set_replaced`), et le réveille quand il s'en va.
    pub(crate) fn sync_ad_blocker(&mut self) {
        if adblock::set_replaced(self.extensions.ad_blocker().is_some()) {
            self.reapply_ads();
        }
    }

    /// Réponse du moteur : l'identifiant WebView2, ou ce qui n'a pas marché.
    pub(crate) fn extension_loaded(&mut self, id: &str, result: Result<Option<String>, String>) {
        self.extensions.set_loaded(id, result.is_ok());
        match result {
            Ok(engine_id) => {
                self.extensions.set_problem(id, None);
                if let Some(engine_id) = engine_id
                    && let Some(ext) = self.extensions.get_mut(id)
                    && ext.engine_id.as_ref() != Some(&engine_id)
                {
                    ext.engine_id = Some(engine_id);
                    self.extensions.save();
                }
            }
            Err(err) => self.extensions.set_problem(id, Some(err)),
        }
        self.sync_ad_blocker();
    }

    /// Le moteur a noté de nouvelles erreurs (ou en a effacé) : le panneau
    /// les montre, le banc les écrit. Avant, une extension qui s'arrêtait au
    /// démarrage (Proton Pass) ne laissait aucune trace dans Feather.
    pub(crate) fn extension_errors(&mut self) {
        for (id, errors) in engine::errors() {
            crate::banc::erreurs_moteur(&id, &errors);
            if errors.is_empty() {
                self.extensions.errors.remove(&id);
            } else {
                // Quelques-unes suffisent au panneau, raccourcies.
                let shown = errors.iter().take(MAX_ERRORS).map(|e| e.chars().take(300).collect()).collect();
                self.extensions.errors.insert(id, shown);
            }
        }
        self.push_state();
    }

    pub(crate) fn set_extension(&mut self, id: &str, enabled: bool) {
        if self.extensions.popup_id() == Some(id) {
            self.close_extension_popup();
        }
        let Some(ext) = self.extensions.get_mut(id) else { return };
        ext.enabled = enabled;
        let ext = ext.clone();
        self.extensions.save();
        self.extensions.sync_declared();
        set_enabled(&ext, enabled);
        self.sync_ad_blocker();
    }

    pub(crate) fn remove_extension(&mut self, id: &str) {
        if self.extensions.popup_id() == Some(id) {
            self.close_extension_popup();
        }
        if let Some(ext) = self.extensions.remove(id) {
            remove(&ext);
            remove_folder_later(&ext);
        }
        self.extensions.set_loaded(id, false);
        self.sync_ad_blocker();
    }

    /// Page d'options de l'extension, dans un nouvel onglet.
    pub(crate) fn extension_options(&mut self, id: &str) {
        let url = self.extensions.get(id).and_then(options_url);
        match url {
            Some(url) => self.open_tab(&url, false),
            None => self.extensions.error = Some("Les options de cette extension ne sont pas encore prêtes.".into()),
        }
    }

    /// Clic sur le bouton d'une extension dans la colonne (`rect` : le bouton).
    pub(crate) fn extension_action(&mut self, id: &str, rect: (f64, f64, f64, f64)) {
        // Deuxième clic sur le même bouton : la fenêtre se ferme (sous macOS, la
        // bulle a pu se fermer seule entre-temps : on la rouvre alors).
        let again = self.extensions.popup_id() == Some(id) && popup_shown();
        self.close_extension_popup();
        if again {
            return;
        }
        let Some(ext) = self.extensions.get(id).cloned() else { return };
        let size = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
        let bar = serde_json::to_value(self.settings().bar_position).ok();
        let bar = bar.as_ref().and_then(Value::as_str).unwrap_or("left").to_string();
        let frame = popup_frame(rect, (380.0, 520.0), (size.width, size.height), &bar);
        match open_popup(&ext, rect, frame, &bar, &self.chrome) {
            Ok(Some(url)) => self.open_popup_webview(&ext.id, &url, frame),
            Ok(None) => {
                #[cfg(target_os = "macos")]
                {
                    self.extensions.popup = Some(Popup { id: ext.id.clone() });
                }
            }
            Err(err) => {
                let text = serde_json::to_string(&err).unwrap_or_default();
                self.chrome_call(&format!("navigateur.showNotice({text})"));
            }
        }
    }

    #[cfg(windows)]
    fn open_popup_webview(&mut self, id: &str, url: &str, frame: (f64, f64, f64, f64)) {
        use tao::dpi::{LogicalPosition, LogicalSize};
        let (x, y, w, h) = frame;
        let proxy = self.proxy.clone();
        let opener = self.proxy.clone();
        let builder = WebViewBuilder::new()
            .with_bounds(wry::Rect { position: LogicalPosition::new(x, y).into(), size: LogicalSize::new(w, h).into() })
            .with_url(url)
            .with_devtools(cfg!(debug_assertions))
            .with_initialization_script(POPUP_SCRIPT)
            .with_ipc_handler(move |request| {
                let _ = proxy.send_event(crate::UserEvent::ExtensionPopup(request.into_body()));
            })
            // Un lien ouvert par la fenêtre part dans un nouvel onglet (et la fenêtre se ferme).
            .with_new_window_req_handler(move |url, _features| {
                let _ = opener.send_event(crate::UserEvent::ExtensionOpen(url));
                wry::NewWindowResponse::Deny
            });
        match self.surface.attach(prepare_builder(builder, true, ""), &self.window) {
            Ok(webview) => {
                let _ = webview.focus();
                self.extensions.popup = Some(Popup { id: id.to_string(), webview, anchor: (x, y) });
            }
            Err(err) => eprintln!("fenêtre de l'extension impossible : {err}"),
        }
    }

    #[cfg(not(windows))]
    fn open_popup_webview(&mut self, _id: &str, _url: &str, _frame: (f64, f64, f64, f64)) {}

    /// Message de la petite fenêtre (Windows) : sa taille, ou sa fermeture.
    pub(crate) fn extension_popup_message(&mut self, raw: &str) {
        let Ok(message) = serde_json::from_str::<PopupMessage>(raw) else { return };
        if message.close {
            self.close_extension_popup();
        } else {
            self.resize_popup(&message);
        }
    }

    /// La fenêtre prend la taille de son contenu, dans les limites de Chrome (800×600).
    #[cfg(windows)]
    fn resize_popup(&self, message: &PopupMessage) {
        use tao::dpi::{LogicalPosition, LogicalSize};
        let (Some(w), Some(h), Some(popup)) = (message.w, message.h, self.extensions.popup.as_ref()) else { return };
        let (x, y) = popup.anchor;
        let size = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
        let w = w.clamp(120.0, 800.0).min(size.width - x - 8.0).max(120.0);
        let h = h.clamp(60.0, 600.0).min(size.height - y - 8.0).max(60.0);
        let _ = popup.webview.set_bounds(wry::Rect {
            position: LogicalPosition::new(x, y).into(),
            size: LogicalSize::new(w, h).into(),
        });
    }

    #[cfg(not(windows))]
    fn resize_popup(&self, _message: &PopupMessage) {}

    /// Lien ouvert par la fenêtre d'une extension : une page web, ou une page
    /// de l'extension elle-même (Windows).
    pub(crate) fn extension_open(&mut self, url: &str) {
        if opens_in_tab(url) {
            self.close_extension_popup();
            self.open_tab(url, false);
        }
    }

    /// Les onglets non privés, décrits aux extensions (macOS : `tabs.query`,
    /// l'onglet d'un script, l'onglet affiché…).
    pub(crate) fn sync_extension_tabs(&self) {
        let active = self.tabs.active_id();
        sync_tabs(self.tabs.iter().filter(|t| !t.private).map(|t| TabView {
            id: t.id,
            title: &t.title,
            url: t.url(),
            loading: t.loading,
            pinned: t.pinned,
            audible: t.audible,
            muted: t.muted,
            active: Some(t.id) == active,
            webview: t.webview(),
        }));
    }

    /// Ferme la petite fenêtre d'extension, s'il y en a une.
    pub(crate) fn close_extension_popup(&mut self) {
        if self.extensions.popup.take().is_some() {
            close_popup();
            self.push_state();
        }
    }
}

/// Efface le dossier d'une extension remplacée ou retirée, quand le moteur l'a lâché.
/// Un dossier encore pris sera effacé au prochain démarrage (voir `sweep`).
fn remove_folder_later(ext: &Extension) {
    let Some(root) = root() else { return };
    let Some(folder) = ext.dir.split('/').next().filter(|f| !f.is_empty() && *f != "." && *f != "..") else { return };
    let path = root.join(folder);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let _ = fs::remove_dir_all(path);
    });
}

#[cfg(windows)]
mod engine {
    use std::cell::RefCell;

    use tao::event_loop::EventLoopProxy;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_13, ICoreWebView2BrowserExtension, ICoreWebView2Profile7,
    };
    use webview2_com::{
        BrowserExtensionEnableCompletedHandler, BrowserExtensionRemoveCompletedHandler,
        ProfileAddBrowserExtensionCompletedHandler, ProfileGetBrowserExtensionsCompletedHandler, take_pwstr,
    };
    use windows::core::{BOOL, HSTRING, Interface, PWSTR};
    use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

    use super::Extension;
    use crate::UserEvent;

    pub const PLATFORM: &str = "windows";

    thread_local! {
        /// Profil WebView2 commun aux pages non privées (celui de l'interface).
        static PROFILE: RefCell<Option<ICoreWebView2Profile7>> = const { RefCell::new(None) };
    }

    pub fn support() -> Result<(), &'static str> {
        Ok(())
    }

    pub fn prepare_builder<'a>(builder: WebViewBuilder<'a>, _attach: bool, _url: &str) -> WebViewBuilder<'a> {
        builder.with_browser_extensions_enabled(true)
    }

    fn profile() -> Option<ICoreWebView2Profile7> {
        PROFILE.with_borrow(|p| p.clone())
    }

    /// Les extensions que le profil connaît : (identifiant WebView2, objet).
    fn installed(done: impl FnOnce(Vec<(String, ICoreWebView2BrowserExtension)>) + 'static) {
        let Some(profile) = profile() else { return };
        let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |result, list| {
            let mut found = Vec::new();
            if let (Ok(()), Some(list)) = (result, list) {
                // SAFETY : liste valide donnée par WebView2, lue sur son fil.
                unsafe {
                    let mut count = 0u32;
                    let _ = list.Count(&mut count);
                    for i in 0..count {
                        let Ok(ext) = list.GetValueAtIndex(i) else { continue };
                        let mut id = PWSTR::null();
                        if ext.Id(&mut id).is_ok() {
                            found.push((take_pwstr(id), ext));
                        }
                    }
                }
            }
            done(found);
            Ok(())
        }));
        // SAFETY : profil valide, rappel conservé par WebView2 jusqu'à son appel.
        unsafe {
            let _ = profile.GetBrowserExtensions(&handler);
        }
    }

    /// Au démarrage : retrouve le profil, puis confie à WebView2 les extensions
    /// qu'il ne connaît pas (il garde les autres d'une fois sur l'autre).
    pub fn start(chrome: &WebView, list: &[Extension], proxy: &EventLoopProxy<UserEvent>) {
        let core = chrome.webview();
        // SAFETY : appels COM sur le fil de la webview.
        let profile = unsafe { core.cast::<ICoreWebView2_13>().and_then(|c| c.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile7>()) };
        let Ok(profile) = profile else {
            eprintln!("extensions indisponibles : WebView2 trop ancien");
            return;
        };
        PROFILE.with_borrow_mut(|p| *p = Some(profile));
        let list = list.to_vec();
        let proxy = proxy.clone();
        installed(move |found| {
            for ext in list {
                match found.iter().find(|(id, _)| Some(id) == ext.engine_id.as_ref()) {
                    Some((id, engine)) => {
                        enable(engine, ext.enabled);
                        let _ = proxy.send_event(UserEvent::ExtensionLoaded { id: ext.id.clone(), result: Ok(Some(id.clone())) });
                    }
                    None => add(&ext, &proxy),
                }
            }
        });
    }

    pub fn sync_tabs<'a>(_tabs: impl Iterator<Item = super::TabView<'a>>) {}

    fn enable(engine: &ICoreWebView2BrowserExtension, on: bool) {
        // SAFETY : extension valide, rappel conservé par WebView2.
        unsafe {
            let mut current = BOOL::default();
            if engine.IsEnabled(&mut current).is_ok() && current.as_bool() == on {
                return;
            }
            let _ = engine.Enable(on, &BrowserExtensionEnableCompletedHandler::create(Box::new(|_| Ok(()))));
        }
    }

    pub fn add(ext: &Extension, proxy: &EventLoopProxy<UserEvent>) {
        let (Some(profile), Some(path)) = (profile(), ext.path()) else { return };
        let id = ext.id.clone();
        let enabled = ext.enabled;
        let done = proxy.clone();
        let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |result, engine| {
            let result = match (result, engine) {
                (Ok(()), Some(engine)) => {
                    if !enabled {
                        enable(&engine, false);
                    }
                    let mut eid = PWSTR::null();
                    // SAFETY : extension valide donnée par WebView2.
                    unsafe { engine.Id(&mut eid) }.map(|()| Some(take_pwstr(eid))).map_err(|e| e.message())
                }
                (Err(e), _) => Err(e.message()),
                _ => Err("refusée par WebView2".into()),
            };
            let _ = done.send_event(UserEvent::ExtensionLoaded { id, result });
            Ok(())
        }));
        // SAFETY : profil valide, chemin en UTF-16, rappel conservé par WebView2.
        unsafe {
            if let Err(err) = profile.AddBrowserExtension(&HSTRING::from(path.as_path()), &handler) {
                let _ = proxy.send_event(UserEvent::ExtensionLoaded { id: ext.id.clone(), result: Err(err.message()) });
            }
        }
    }

    pub fn set_enabled(ext: &Extension, on: bool) {
        let Some(engine_id) = ext.engine_id.clone() else { return };
        installed(move |found| {
            if let Some((_, engine)) = found.iter().find(|(id, _)| *id == engine_id) {
                enable(engine, on);
            }
        });
    }

    pub fn remove(ext: &Extension) {
        remove_then(ext, || {});
    }

    /// Retire l'extension du profil, puis fait `then` (même si elle n'y était pas).
    fn remove_then(ext: &Extension, then: impl FnOnce() + 'static) {
        let Some(engine_id) = ext.engine_id.clone() else {
            then();
            return;
        };
        installed(move |found| match found.iter().find(|(id, _)| *id == engine_id) {
            Some((_, engine)) => {
                let then = std::cell::Cell::new(Some(then));
                let done = BrowserExtensionRemoveCompletedHandler::create(Box::new(move |_| {
                    if let Some(then) = then.take() {
                        then();
                    }
                    Ok(())
                }));
                // SAFETY : extension valide, rappel conservé par WebView2.
                unsafe {
                    let _ = engine.Remove(&done);
                }
            }
            None => then(),
        });
    }

    /// Nouvelle version : la nouvelle n'est ajoutée qu'une fois l'ancienne retirée
    /// (même identifiant : sinon le retrait tardif emportait la nouvelle).
    pub fn replace(old: &Extension, new: &Extension, proxy: &EventLoopProxy<UserEvent>) {
        let new = new.clone();
        let proxy = proxy.clone();
        remove_then(old, move || add(&new, &proxy));
    }

    /// WebView2 ne tient pas de liste des erreurs des extensions.
    pub fn errors() -> Vec<(String, Vec<String>)> {
        Vec::new()
    }

    /// Adresse d'une page de l'extension (`popup.html`…), une fois chargée.
    pub fn page_url(ext: &Extension, page: &str) -> Option<String> {
        Some(format!("chrome-extension://{}/{page}", ext.engine_id.as_ref()?))
    }

    pub fn options_url(ext: &Extension) -> Option<String> {
        page_url(ext, ext.options.as_ref()?)
    }

    /// Adresse de la petite fenêtre : la webview est créée par `App` (voir main.rs).
    pub fn open_popup(
        ext: &Extension,
        _button: (f64, f64, f64, f64),
        _frame: (f64, f64, f64, f64),
        _bar: &str,
        _chrome: &WebView,
    ) -> Result<Option<String>, String> {
        let page = ext.popup.as_ref().ok_or("Cette extension n'a pas de fenêtre à ouvrir.")?;
        page_url(ext, page).map(Some).ok_or_else(|| "L'extension est encore en cours de chargement.".into())
    }

    pub fn close_popup() {}

    pub fn popup_shown() -> bool {
        true
    }
}

#[cfg(target_os = "macos")]
mod engine {
    //! macOS 15.4+ : `WKWebExtensionController`. WebKit ne connaît ni les onglets
    //! ni la fenêtre de Feather : on les lui décrit (`ExtTab`, `ExtWindow`), et le
    //! délégué (`ExtDelegate`) répond à ses demandes (fenêtre de l'extension,
    //! nouvel onglet, autorisations). Sans cela, les scripts des extensions ne
    //! savaient pas dans quel onglet ils tournaient (Proton Pass ne remplissait
    //! rien) et leur fenêtre s'affichait vide, avant d'être chargée.
    //!
    //! Règle : jamais d'appel à WebKit pendant un emprunt de `STATE`, car WebKit
    //! rappelle aussitôt nos objets, qui le relisent.
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ptr::{self, NonNull};

    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{NSPopover, NSPopoverBehavior, NSView};
    use objc2_foundation::{
        NSArray, NSDate, NSError, NSNotification, NSNotificationCenter, NSPoint, NSRect, NSRectEdge, NSSet, NSSize, NSString,
        NSUInteger, NSURL, NSURLRequest,
    };
    use objc2_web_kit::{
        WKWebExtension, WKWebExtensionAction, WKWebExtensionContext, WKWebExtensionContextErrorsDidUpdateNotification,
        WKWebExtensionContextPermissionStatus, WKWebExtensionController, WKWebExtensionControllerDelegate,
        WKWebExtensionMatchPattern, WKWebExtensionTab, WKWebExtensionTabChangedProperties, WKWebExtensionTabConfiguration,
        WKWebExtensionWindow,
        WKUserContentController, WKWebExtensionWindowState, WKWebExtensionWindowType, WKWebView, WKWebViewConfiguration,
        WKWebsiteDataStore,
    };
    use tao::event_loop::EventLoopProxy;
    use wry::{WebView, WebViewBuilder, WebViewBuilderExtMacos, WebViewExtMacOS};

    use super::{Extension, TabView};
    use crate::UserEvent;

    pub const PLATFORM: &str = "macos";

    type NewTabDone = RcBlock<dyn Fn(*mut ProtocolObject<dyn WKWebExtensionTab>, *mut NSError)>;

    /// Un onglet de Feather tel que les extensions le voient (recopié par `sync_tabs`).
    struct Mirror {
        id: u32,
        tab: Retained<ExtTab>,
        title: String,
        url: String,
        loading: bool,
        pinned: bool,
        audible: bool,
        muted: bool,
        webview: Option<Retained<WKWebView>>,
    }

    #[derive(Default)]
    struct State {
        /// Une extension au moins est installée : les onglets sont décrits à WebKit.
        wanted: bool,
        controller: Option<Retained<WKWebExtensionController>>,
        delegate: Option<Retained<ExtDelegate>>,
        contexts: HashMap<String, Retained<WKWebExtensionContext>>,
        popover: Option<Retained<NSPopover>>,
        proxy: Option<EventLoopProxy<UserEvent>>,
        /// La vue de l'interface, contre laquelle la fenêtre de l'extension s'ouvre.
        chrome: Option<Retained<NSView>>,
        /// Le bouton cliqué (x, y, largeur, hauteur) et la place de la barre.
        anchor: Option<((f64, f64, f64, f64), String)>,
        window: Option<Retained<ExtWindow>>,
        tabs: Vec<Mirror>,
        active: Option<u32>,
        /// Onglets demandés par une extension, en attente de leur création.
        new_tabs: Vec<NewTabDone>,
        /// Abonnement aux erreurs notées par WebKit (voir `errors`), et une
        /// lecture déjà demandée à la boucle d'évènements.
        errors_observer: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
        errors_due: bool,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::default();
    }

    fn send(event: UserEvent) {
        if let Some(proxy) = STATE.with_borrow(|s| s.proxy.clone()) {
            let _ = proxy.send_event(event);
        }
    }

    fn tab_object(id: u32) -> Option<Retained<ProtocolObject<dyn WKWebExtensionTab>>> {
        STATE.with_borrow(|s| s.tabs.iter().find(|m| m.id == id).map(|m| ProtocolObject::from_retained(m.tab.clone())))
    }

    fn active_tab() -> Option<Retained<ProtocolObject<dyn WKWebExtensionTab>>> {
        tab_object(STATE.with_borrow(|s| s.active)?)
    }

    fn window_object() -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
        STATE.with_borrow(|s| s.window.clone()).map(ProtocolObject::from_retained)
    }

    fn with_tab<R>(id: u32, f: impl FnOnce(&Mirror) -> R) -> Option<R> {
        STATE.with_borrow(|s| s.tabs.iter().find(|m| m.id == id).map(f))
    }

    struct TabIvars {
        id: u32,
    }

    define_class!(
        // SAFETY : NSObject n'impose rien à ses sous-classes ; pas de `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "FeatherExtensionTab"]
        #[ivars = TabIvars]
        struct ExtTab;

        unsafe impl NSObjectProtocol for ExtTab {}

        unsafe impl WKWebExtensionTab for ExtTab {
            #[unsafe(method_id(windowForWebExtensionContext:))]
            fn window_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
                window_object()
            }

            #[unsafe(method(indexInWindowForWebExtensionContext:))]
            fn index_for(&self, _context: &WKWebExtensionContext) -> NSUInteger {
                let id = self.ivars().id;
                STATE.with_borrow(|s| s.tabs.iter().position(|m| m.id == id)).unwrap_or(NSUInteger::MAX)
            }

            #[unsafe(method_id(webViewForWebExtensionContext:))]
            fn webview_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<WKWebView>> {
                with_tab(self.ivars().id, |m| m.webview.clone()).flatten()
            }

            #[unsafe(method_id(titleForWebExtensionContext:))]
            fn title_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<NSString>> {
                with_tab(self.ivars().id, |m| NSString::from_str(&m.title))
            }

            #[unsafe(method_id(urlForWebExtensionContext:))]
            fn url_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<NSURL>> {
                with_tab(self.ivars().id, |m| NSURL::URLWithString(&NSString::from_str(&m.url))).flatten()
            }

            #[unsafe(method(isLoadingCompleteForWebExtensionContext:))]
            fn loaded_for(&self, _context: &WKWebExtensionContext) -> bool {
                with_tab(self.ivars().id, |m| !m.loading).unwrap_or(true)
            }

            #[unsafe(method(isPinnedForWebExtensionContext:))]
            fn pinned_for(&self, _context: &WKWebExtensionContext) -> bool {
                with_tab(self.ivars().id, |m| m.pinned).unwrap_or(false)
            }

            #[unsafe(method(isPlayingAudioForWebExtensionContext:))]
            fn audible_for(&self, _context: &WKWebExtensionContext) -> bool {
                with_tab(self.ivars().id, |m| m.audible).unwrap_or(false)
            }

            #[unsafe(method(isMutedForWebExtensionContext:))]
            fn muted_for(&self, _context: &WKWebExtensionContext) -> bool {
                with_tab(self.ivars().id, |m| m.muted).unwrap_or(false)
            }

            #[unsafe(method(isSelectedForWebExtensionContext:))]
            fn selected_for(&self, _context: &WKWebExtensionContext) -> bool {
                STATE.with_borrow(|s| s.active) == Some(self.ivars().id)
            }

            // Clic sur le bouton d'une extension : elle a accès à l'onglet affiché
            // (autorisation « activeTab »), comme dans Chrome.
            #[unsafe(method(shouldGrantPermissionsOnUserGestureForWebExtensionContext:))]
            fn grant_on_gesture(&self, _context: &WKWebExtensionContext) -> bool {
                true
            }

            #[unsafe(method(loadURL:forWebExtensionContext:completionHandler:))]
            fn load_url(&self, url: &NSURL, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
                if let Some(webview) = with_tab(self.ivars().id, |m| m.webview.clone()).flatten() {
                    // SAFETY : fil principal, vue et requête valides.
                    unsafe { webview.loadRequest(&NSURLRequest::requestWithURL(url)) };
                }
                done.call((ptr::null_mut(),));
            }

            #[unsafe(method(reloadFromOrigin:forWebExtensionContext:completionHandler:))]
            fn reload(&self, _origin: bool, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
                if let Some(webview) = with_tab(self.ivars().id, |m| m.webview.clone()).flatten() {
                    // SAFETY : fil principal, vue valide.
                    unsafe { webview.reload() };
                }
                done.call((ptr::null_mut(),));
            }

            #[unsafe(method(activateForWebExtensionContext:completionHandler:))]
            fn activate(&self, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
                send(UserEvent::Chrome(format!(r#"{{"type":"activate_tab","id":{}}}"#, self.ivars().id)));
                done.call((ptr::null_mut(),));
            }

            #[unsafe(method(closeForWebExtensionContext:completionHandler:))]
            fn close(&self, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
                send(UserEvent::Chrome(format!(r#"{{"type":"close_tab","id":{}}}"#, self.ivars().id)));
                done.call((ptr::null_mut(),));
            }
        }
    );

    impl ExtTab {
        fn new(mtm: MainThreadMarker, id: u32) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(TabIvars { id });
            // SAFETY : `init` de NSObject.
            unsafe { msg_send![super(this), init] }
        }
    }

    define_class!(
        // SAFETY : NSObject n'impose rien à ses sous-classes ; pas de `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "FeatherExtensionWindow"]
        struct ExtWindow;

        unsafe impl NSObjectProtocol for ExtWindow {}

        unsafe impl WKWebExtensionWindow for ExtWindow {
            #[unsafe(method_id(tabsForWebExtensionContext:))]
            fn tabs_for(&self, _context: &WKWebExtensionContext) -> Retained<NSArray<ProtocolObject<dyn WKWebExtensionTab>>> {
                let tabs: Vec<_> = STATE.with_borrow(|s| s.tabs.iter().map(|m| ProtocolObject::from_retained(m.tab.clone())).collect());
                NSArray::from_retained_slice(&tabs)
            }

            #[unsafe(method_id(activeTabForWebExtensionContext:))]
            fn active_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<ProtocolObject<dyn WKWebExtensionTab>>> {
                active_tab()
            }

            #[unsafe(method(windowTypeForWebExtensionContext:))]
            fn type_for(&self, _context: &WKWebExtensionContext) -> WKWebExtensionWindowType {
                WKWebExtensionWindowType::Normal
            }

            #[unsafe(method(windowStateForWebExtensionContext:))]
            fn state_for(&self, _context: &WKWebExtensionContext) -> WKWebExtensionWindowState {
                WKWebExtensionWindowState::Normal
            }

            #[unsafe(method(isPrivateForWebExtensionContext:))]
            fn private_for(&self, _context: &WKWebExtensionContext) -> bool {
                false
            }

            #[unsafe(method(focusForWebExtensionContext:completionHandler:))]
            fn focus(&self, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
                done.call((ptr::null_mut(),));
            }
        }
    );

    impl ExtWindow {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            // SAFETY : `init` de NSObject.
            unsafe { msg_send![super(this), init] }
        }
    }

    define_class!(
        // SAFETY : NSObject n'impose rien à ses sous-classes ; pas de `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "FeatherExtensionDelegate"]
        struct ExtDelegate;

        unsafe impl NSObjectProtocol for ExtDelegate {}

        unsafe impl WKWebExtensionControllerDelegate for ExtDelegate {
            #[unsafe(method_id(webExtensionController:openWindowsForExtensionContext:))]
            fn open_windows(
                &self,
                _controller: &WKWebExtensionController,
                _context: &WKWebExtensionContext,
            ) -> Retained<NSArray<ProtocolObject<dyn WKWebExtensionWindow>>> {
                let windows: Vec<_> = window_object().into_iter().collect();
                NSArray::from_retained_slice(&windows)
            }

            #[unsafe(method_id(webExtensionController:focusedWindowForExtensionContext:))]
            fn focused_window(
                &self,
                _controller: &WKWebExtensionController,
                _context: &WKWebExtensionContext,
            ) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
                window_object()
            }

            // `tabs.create` : un onglet de Feather, rendu à l'extension dès qu'il existe.
            #[unsafe(method(webExtensionController:openNewTabUsingConfiguration:forExtensionContext:completionHandler:))]
            fn open_new_tab(
                &self,
                _controller: &WKWebExtensionController,
                configuration: &WKWebExtensionTabConfiguration,
                _context: &WKWebExtensionContext,
                done: &DynBlock<dyn Fn(*mut ProtocolObject<dyn WKWebExtensionTab>, *mut NSError)>,
            ) {
                // SAFETY : fil principal, configuration valide.
                let url = unsafe { configuration.url() }.and_then(|u| u.absoluteString()).map(|s| s.to_string());
                match url.filter(|u| super::opens_in_tab(u)) {
                    Some(url) => {
                        STATE.with_borrow_mut(|s| s.new_tabs.push(done.copy()));
                        send(UserEvent::ExtensionOpen(url));
                    }
                    None => done.call((ptr::null_mut(), ptr::null_mut())),
                }
            }

            #[unsafe(method(webExtensionController:openOptionsPageForExtensionContext:completionHandler:))]
            fn open_options(
                &self,
                _controller: &WKWebExtensionController,
                context: &WKWebExtensionContext,
                done: &DynBlock<dyn Fn(*mut NSError)>,
            ) {
                // SAFETY : fil principal, contexte valide.
                if let Some(url) = unsafe { context.optionsPageURL() }.and_then(|u| u.absoluteString()) {
                    send(UserEvent::ExtensionOpen(url.to_string()));
                }
                done.call((ptr::null_mut(),));
            }

            // Autorisations demandées plus tard par l'extension : WebKit n'accepte
            // que celles que son manifeste annonce comme facultatives, et la fiche
            // les a montrées avant l'installation (« pourra aussi demander… »).
            #[unsafe(method(webExtensionController:promptForPermissions:inTab:forExtensionContext:completionHandler:))]
            fn prompt_permissions(
                &self,
                _controller: &WKWebExtensionController,
                permissions: &NSSet<NSString>,
                _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
                _context: &WKWebExtensionContext,
                done: &DynBlock<dyn Fn(NonNull<NSSet<NSString>>, *mut NSDate)>,
            ) {
                done.call((NonNull::from(permissions), ptr::null_mut()));
            }

            #[unsafe(method(webExtensionController:promptForPermissionToAccessURLs:inTab:forExtensionContext:completionHandler:))]
            fn prompt_urls(
                &self,
                _controller: &WKWebExtensionController,
                urls: &NSSet<NSURL>,
                _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
                _context: &WKWebExtensionContext,
                done: &DynBlock<dyn Fn(NonNull<NSSet<NSURL>>, *mut NSDate)>,
            ) {
                done.call((NonNull::from(urls), ptr::null_mut()));
            }

            #[unsafe(method(webExtensionController:promptForPermissionMatchPatterns:inTab:forExtensionContext:completionHandler:))]
            fn prompt_patterns(
                &self,
                _controller: &WKWebExtensionController,
                patterns: &NSSet<WKWebExtensionMatchPattern>,
                _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
                _context: &WKWebExtensionContext,
                done: &DynBlock<dyn Fn(NonNull<NSSet<WKWebExtensionMatchPattern>>, *mut NSDate)>,
            ) {
                done.call((NonNull::from(patterns), ptr::null_mut()));
            }

            // WebKit appelle ceci une fois la page de la fenêtre chargée : avant,
            // la bulle était montrée tout de suite, vide.
            #[unsafe(method(webExtensionController:presentPopupForAction:forExtensionContext:completionHandler:))]
            fn present_popup(
                &self,
                _controller: &WKWebExtensionController,
                action: &WKWebExtensionAction,
                _context: &WKWebExtensionContext,
                done: &DynBlock<dyn Fn(*mut NSError)>,
            ) {
                show_popover(action);
                done.call((ptr::null_mut(),));
            }
        }
    );

    impl ExtDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            // SAFETY : `init` de NSObject.
            unsafe { msg_send![super(this), init] }
        }
    }

    pub fn support() -> Result<(), &'static str> {
        if objc2::available!(macos = 15.4) {
            Ok(())
        } else {
            Err("Les extensions demandent macOS 15.4 ou plus récent.")
        }
    }

    fn controller(mtm: MainThreadMarker) -> Option<Retained<WKWebExtensionController>> {
        support().ok()?;
        if let Some(controller) = STATE.with_borrow(|s| s.controller.clone()) {
            return Some(controller);
        }
        // SAFETY : fil principal, classe présente depuis macOS 15.4 (vérifié ci-dessus).
        let controller = unsafe { WKWebExtensionController::new(mtm) };
        let delegate = ExtDelegate::new(mtm);
        // SAFETY : fil principal ; le contrôleur ne garde qu'une référence faible
        // au délégué, gardé dans `STATE`.
        unsafe { controller.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
        // Erreurs notées par WebKit pendant que les extensions tournent : une
        // seule lecture demandée pour une rafale de notifications.
        let block = RcBlock::new(|_: NonNull<NSNotification>| {
            let proxy = STATE.with_borrow_mut(|s| {
                let proxy = if s.errors_due { None } else { s.proxy.clone() };
                s.errors_due |= proxy.is_some();
                proxy
            });
            if let Some(proxy) = proxy {
                let _ = proxy.send_event(UserEvent::ExtensionErrors);
            }
        });
        // SAFETY : nom publié par WebKit depuis macOS 15.4 (vérifié par `support`) ;
        // sans file d'attente, le bloc est appelé sur le fil qui publie, le
        // principal pour WebKit. L'abonnement est gardé dans `STATE`.
        let observer = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(WKWebExtensionContextErrorsDidUpdateNotification),
                None,
                None,
                &block,
            )
        };
        STATE.with_borrow_mut(|s| {
            s.controller = Some(controller.clone());
            s.delegate = Some(delegate);
            s.errors_observer = Some(observer);
        });
        Some(controller)
    }

    /// Les erreurs notées par WebKit, par extension : manifeste, arrière-plan
    /// refusé, erreur de script (selon la version de macOS). Toutes à chaque
    /// fois ; WebKit ne garde qu'un exemplaire de chacune.
    pub fn errors() -> Vec<(String, Vec<String>)> {
        let contexts: Vec<_> = STATE.with_borrow_mut(|s| {
            s.errors_due = false;
            s.contexts.iter().map(|(id, c)| (id.clone(), c.clone())).collect()
        });
        contexts
            .into_iter()
            .map(|(id, context)| {
                // SAFETY : fil principal, contexte valide.
                let errors = unsafe { context.errors() };
                (id, errors.iter().map(|e| e.localizedDescription().to_string()).collect())
            })
            .collect()
    }

    /// Toute page non privée reçoit le contrôleur des extensions. Avant, seulement
    /// quand une extension était déjà installée : les onglets ouverts avant
    /// l'installation n'avaient jamais les extensions (WebKit ne permet pas de
    /// l'ajouter à une page déjà créée).
    pub fn prepare_builder<'a>(builder: WebViewBuilder<'a>, attach: bool, url: &str) -> WebViewBuilder<'a> {
        let Some(mtm) = MainThreadMarker::new() else { return builder };
        if !attach {
            return builder;
        }
        // Page d'une extension (options, page ouverte par l'extension) : WebKit ne
        // la charge que dans la configuration de son contexte. Avec celle des
        // onglets, la page restait blanche (vérifié par outils/sonde-extensions.m).
        if let Some(config) = page_configuration(url) {
            return builder.with_webview_configuration(config);
        }
        let Some(controller) = controller(mtm) else { return builder };
        // SAFETY : fil principal ; configuration neuve, magasin par défaut
        // (celui que wry aurait choisi pour une page non privée).
        let config = unsafe {
            let config = WKWebViewConfiguration::new(mtm);
            config.setWebsiteDataStore(&WKWebsiteDataStore::defaultDataStore(mtm));
            config.setWebExtensionController(Some(&controller));
            config
        };
        builder.with_webview_configuration(config)
    }

    pub fn start(chrome: &WebView, list: &[Extension], proxy: &EventLoopProxy<UserEvent>) {
        if support().is_err() {
            return;
        }
        let view: Retained<NSView> = Retained::into_super(Retained::into_super(chrome.webview()));
        STATE.with_borrow_mut(|s| {
            s.proxy = Some(proxy.clone());
            s.chrome = Some(view);
        });
        for ext in list {
            add(ext, proxy);
        }
    }

    /// Décrit à WebKit les onglets de Feather (ouverts, fermés, déplacés, affiché,
    /// titre, adresse…), après chaque changement d'état.
    pub fn sync_tabs<'a>(tabs: impl Iterator<Item = TabView<'a>>) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        if !STATE.with_borrow(|s| s.wanted) {
            return;
        }
        let Some(controller) = controller(mtm) else { return };
        // La fenêtre, une fois pour toutes.
        let new_window = STATE.with_borrow(|s| s.window.is_none());
        if new_window {
            let window = ExtWindow::new(mtm);
            STATE.with_borrow_mut(|s| s.window = Some(window.clone()));
            let window = ProtocolObject::from_retained(window);
            // SAFETY : fil principal, objets valides.
            unsafe {
                controller.didOpenWindow(&window);
                controller.didFocusWindow(Some(&window));
            }
        }
        let views: Vec<TabView> = tabs.collect();
        let active = views.iter().find(|v| v.active).map(|v| v.id);
        let mut opened = Vec::new();
        let mut moved = Vec::new();
        let mut changed = Vec::new();
        // Les différences sont calculées d'abord, WebKit prévenu ensuite (voir la règle en tête).
        let (closed, previous, done) = STATE.with_borrow_mut(|s| {
            let mut old = std::mem::take(&mut s.tabs);
            let previous = s.active.and_then(|id| old.iter().find(|m| m.id == id)).map(|m| m.tab.clone());
            let old_index: HashMap<u32, usize> = old.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
            for (index, view) in views.iter().enumerate() {
                let webview: Option<Retained<WKWebView>> = view.webview.map(|w| Retained::into_super(w.webview()));
                let mirror = match old.iter().position(|m| m.id == view.id) {
                    Some(i) => {
                        let m = old.swap_remove(i);
                        let mut props = WKWebExtensionTabChangedProperties::empty();
                        if m.title != view.title {
                            props |= WKWebExtensionTabChangedProperties::Title;
                        }
                        let same_view = match (&m.webview, &webview) {
                            (Some(a), Some(b)) => ptr::eq(&**a, &**b),
                            (None, None) => true,
                            _ => false,
                        };
                        if m.url != view.url || !same_view {
                            props |= WKWebExtensionTabChangedProperties::URL;
                        }
                        if m.loading != view.loading || !same_view {
                            props |= WKWebExtensionTabChangedProperties::Loading;
                        }
                        if m.pinned != view.pinned {
                            props |= WKWebExtensionTabChangedProperties::Pinned;
                        }
                        if m.audible != view.audible {
                            props |= WKWebExtensionTabChangedProperties::PlayingAudio;
                        }
                        if m.muted != view.muted {
                            props |= WKWebExtensionTabChangedProperties::Muted;
                        }
                        if !props.is_empty() {
                            changed.push((m.tab.clone(), props));
                        }
                        if old_index.get(&view.id).is_some_and(|&i| i != index) {
                            moved.push((m.tab.clone(), old_index[&view.id]));
                        }
                        m.tab
                    }
                    None => {
                        let tab = ExtTab::new(mtm, view.id);
                        opened.push(tab.clone());
                        tab
                    }
                };
                s.tabs.push(Mirror {
                    id: view.id,
                    tab: mirror,
                    title: view.title.to_string(),
                    url: view.url.to_string(),
                    loading: view.loading,
                    pinned: view.pinned,
                    audible: view.audible,
                    muted: view.muted,
                    webview,
                });
            }
            // L'onglet affiché avant, sauf s'il vient d'être fermé.
            let previous = previous.filter(|p| !old.iter().any(|m| ptr::eq(&*m.tab, &**p)));
            let previous = if s.active == active { None } else { Some(previous) };
            s.active = active;
            // Onglets demandés par une extension : le dernier ouvert leur est rendu.
            let done = if opened.is_empty() { Vec::new() } else { std::mem::take(&mut s.new_tabs) };
            (old, previous, done)
        });
        let active_tab = active.and_then(tab_object);
        // SAFETY : fil principal, objets valides ; aucun emprunt de `STATE` en cours.
        unsafe {
            for m in closed {
                controller.didCloseTab_windowIsClosing(&ProtocolObject::from_retained(m.tab), false);
            }
            for tab in &opened {
                controller.didOpenTab(ProtocolObject::from_ref(&**tab));
            }
            for (tab, from) in moved {
                controller.didMoveTab_fromIndex_inWindow(ProtocolObject::from_ref(&*tab), from, window_object().as_deref());
            }
            for (tab, props) in changed {
                controller.didChangeTabProperties_forTab(props, ProtocolObject::from_ref(&*tab));
            }
            if let (Some(previous), Some(tab)) = (previous, &active_tab) {
                let previous = previous.map(ProtocolObject::from_retained);
                controller.didActivateTab_previousActiveTab(tab, previous.as_deref());
            }
        }
        if let Some(tab) = opened.last() {
            let tab: &ProtocolObject<dyn WKWebExtensionTab> = ProtocolObject::from_ref(&**tab);
            let tab = tab as *const ProtocolObject<dyn WKWebExtensionTab> as *mut _;
            for block in done {
                block.call((tab, ptr::null_mut()));
            }
        }
    }

    pub fn add(ext: &Extension, proxy: &EventLoopProxy<UserEvent>) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let (Some(controller), Some(path)) = (controller(mtm), ext.path()) else { return };
        STATE.with_borrow_mut(|s| s.wanted = true);
        let id = ext.id.clone();
        let enabled = ext.enabled;
        let proxy = proxy.clone();
        let block = RcBlock::new(move |extension: *mut WKWebExtension, error: *mut NSError| {
            // SAFETY : WebKit passe une extension valide, ou nulle avec une erreur.
            let Some(extension) = (unsafe { Retained::retain(extension) }) else {
                let error = unsafe { Retained::retain(error) };
                let message = error.map(|e| e.localizedDescription().to_string()).unwrap_or_else(|| "extension refusée par WebKit".into());
                let _ = proxy.send_event(UserEvent::ExtensionLoaded { id: id.clone(), result: Err(message) });
                return;
            };
            // SAFETY : fil principal (WebKit rappelle sur la file principale), objets valides.
            let result = unsafe {
                let context = WKWebExtensionContext::contextForExtension(&extension);
                context.setUniqueIdentifier(&NSString::from_str(&id));
                context.setInspectable(cfg!(debug_assertions));
                // Ce que l'extension demande lui est accordé à l'installation,
                // comme Chrome (la liste des sites est affichée dans le panneau).
                for permission in extension.requestedPermissions().iter() {
                    context.setPermissionStatus_forPermission(WKWebExtensionContextPermissionStatus::GrantedExplicitly, &permission);
                }
                for pattern in extension.allRequestedMatchPatterns().iter() {
                    context.setPermissionStatus_forMatchPattern(WKWebExtensionContextPermissionStatus::GrantedExplicitly, &pattern);
                }
                let loaded = if enabled { controller.loadExtensionContext_error(&context) } else { Ok(()) };
                STATE.with_borrow_mut(|s| s.contexts.insert(id.clone(), context));
                loaded.map(|()| None).map_err(|e| e.localizedDescription().to_string())
            };
            let _ = proxy.send_event(UserEvent::ExtensionLoaded { id: id.clone(), result });
        });
        // Les évènements de Chrome absents de WebKit, avant l'arrière-plan.
        if let Err(err) = super::add_compat(&path) {
            eprintln!("compléments de l'extension non ajoutés : {err}");
        }
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&path.to_string_lossy()), true);
        // SAFETY : fil principal, adresse de fichier valide, rappel conservé par WebKit.
        unsafe { WKWebExtension::extensionWithResourceBaseURL_completionHandler(&url, &block, mtm) };
    }

    fn context(id: &str) -> Option<Retained<WKWebExtensionContext>> {
        STATE.with_borrow(|s| s.contexts.get(id).cloned())
    }

    /// Configuration du contexte dont l'adresse de base commence `url`
    /// (webkit-extension://<identifiant>/…).
    ///
    /// WebKit rend une copie de la configuration, mais qui garde le même
    /// `WKUserContentController` pour toutes les pages de l'extension : wry y
    /// ajoutait à chaque page son gestionnaire « ipc » (WebKit refuse un nom déjà
    /// pris) et ses scripts, jamais retirés. Chaque page reçoit donc le sien.
    fn page_configuration(url: &str) -> Option<Retained<WKWebViewConfiguration>> {
        if !url.starts_with("webkit-extension://") {
            return None;
        }
        let mtm = MainThreadMarker::new()?;
        let contexts: Vec<_> = STATE.with_borrow(|s| s.contexts.values().cloned().collect());
        contexts.into_iter().find_map(|context| {
            // SAFETY : fil principal, contexte valide ; contrôleur neuf.
            unsafe {
                let base = context.baseURL().absoluteString()?.to_string();
                if !url.starts_with(&base) {
                    return None;
                }
                let config = context.webViewConfiguration()?;
                config.setUserContentController(&WKUserContentController::new(mtm));
                Some(config)
            }
        })
    }

    pub fn set_enabled(ext: &Extension, on: bool) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let (Some(controller), Some(context)) = (controller(mtm), context(&ext.id)) else { return };
        // SAFETY : fil principal, objets valides.
        unsafe {
            if on && !context.isLoaded() {
                let _ = controller.loadExtensionContext_error(&context);
            } else if !on && context.isLoaded() {
                let _ = controller.unloadExtensionContext_error(&context);
            }
        }
    }

    pub fn remove(ext: &Extension) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let Some(context) = STATE.with_borrow_mut(|s| s.contexts.remove(&ext.id)) else { return };
        if let Some(controller) = controller(mtm) {
            // SAFETY : fil principal, objets valides.
            unsafe {
                if context.isLoaded() {
                    let _ = controller.unloadExtensionContext_error(&context);
                }
            }
        }
    }

    pub fn replace(old: &Extension, new: &Extension, proxy: &EventLoopProxy<UserEvent>) {
        remove(old);
        add(new, proxy);
    }

    /// Adresse d'une page de l'extension (`popup.html`…), une fois chargée.
    pub fn page_url(ext: &Extension, page: &str) -> Option<String> {
        let context = context(&ext.id)?;
        // SAFETY : fil principal, contexte valide.
        let base = unsafe { context.baseURL().absoluteString() }?.to_string();
        Some(format!("{}/{page}", base.trim_end_matches('/')))
    }

    pub fn options_url(ext: &Extension) -> Option<String> {
        let context = context(&ext.id)?;
        // SAFETY : fil principal, contexte valide.
        let url = unsafe { context.optionsPageURL() }?;
        url.absoluteString().map(|s| s.to_string())
    }

    /// Clic sur le bouton de l'extension (`button`, dans l'interface) : WebKit
    /// charge sa fenêtre, puis demande de l'afficher (`present_popup`).
    /// Sans fenêtre, le clic est transmis à l'extension.
    pub fn open_popup(
        ext: &Extension,
        button: (f64, f64, f64, f64),
        _frame: (f64, f64, f64, f64),
        bar: &str,
        _chrome: &WebView,
    ) -> Result<Option<String>, String> {
        let context = context(&ext.id).ok_or("L'extension est encore en cours de chargement.")?;
        let tab = active_tab();
        // SAFETY : fil principal, objets valides ; aucun emprunt de `STATE` en cours.
        unsafe {
            let action = context.actionForTab(tab.as_deref()).ok_or("Cette extension n'a pas de bouton.")?;
            if action.presentsPopup() {
                STATE.with_borrow_mut(|s| s.anchor = Some((button, bar.to_string())));
            }
            context.performActionForTab(tab.as_deref());
        }
        Ok(None)
    }

    /// La bulle de la fenêtre, la pointe sur le bouton de l'extension, du côté de la page.
    fn show_popover(action: &WKWebExtensionAction) {
        // SAFETY : fil principal, action valide.
        let Some(popover) = (unsafe { action.popupPopover() }) else { return };
        let (anchor, view) = STATE.with_borrow_mut(|s| (s.anchor.take(), s.chrome.clone()));
        let Some(view) = view else { return };
        let ((x, y, w, h), bar) = anchor.unwrap_or(((0.0, 0.0, 1.0, 1.0), String::new()));
        let flipped = view.isFlipped();
        let height = view.frame().size.height;
        let y = if flipped { y } else { height - y - h };
        let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(1.0), h.max(1.0)));
        let edge = match bar.as_str() {
            "right" => NSRectEdge::MinX,
            "top" if flipped => NSRectEdge::MaxY,
            "top" => NSRectEdge::MinY,
            _ => NSRectEdge::MaxX,
        };
        popover.setBehavior(NSPopoverBehavior::Transient);
        popover.showRelativeToRect_ofView_preferredEdge(rect, &view, edge);
        STATE.with_borrow_mut(|s| s.popover = Some(popover));
    }

    pub fn close_popup() {
        let popover = STATE.with_borrow_mut(|s| {
            s.anchor = None;
            s.popover.take()
        });
        if let Some(popover) = popover {
            // SAFETY : fil principal, bulle valide.
            unsafe { popover.performClose(None) };
        }
    }

    /// La bulle se ferme seule au clic ailleurs : est-elle encore affichée ?
    /// (Juste après le clic, la fenêtre se charge : elle compte comme affichée.)
    pub fn popup_shown() -> bool {
        STATE.with_borrow(|s| s.anchor.is_some() || s.popover.as_ref().is_some_and(|p| p.isShown()))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod engine {
    use tao::event_loop::EventLoopProxy;
    use wry::{WebView, WebViewBuilder};

    use super::Extension;
    use crate::UserEvent;

    pub const PLATFORM: &str = "linux";

    pub fn support() -> Result<(), &'static str> {
        Err("WebKitGTK, le moteur de Feather sous Linux, ne sait pas faire tourner d'extensions.")
    }

    pub fn prepare_builder<'a>(builder: WebViewBuilder<'a>, _attach: bool, _url: &str) -> WebViewBuilder<'a> {
        builder
    }

    pub fn start(_chrome: &WebView, _list: &[Extension], _proxy: &EventLoopProxy<UserEvent>) {}

    pub fn sync_tabs<'a>(_tabs: impl Iterator<Item = super::TabView<'a>>) {}


    pub fn add(_ext: &Extension, _proxy: &EventLoopProxy<UserEvent>) {}

    pub fn set_enabled(_ext: &Extension, _on: bool) {}

    pub fn remove(_ext: &Extension) {}

    pub fn replace(_old: &Extension, _new: &Extension, _proxy: &EventLoopProxy<UserEvent>) {}

    pub fn errors() -> Vec<(String, Vec<String>)> {
        Vec::new()
    }

    pub fn page_url(_ext: &Extension, _page: &str) -> Option<String> {
        None
    }

    pub fn options_url(_ext: &Extension) -> Option<String> {
        None
    }

    pub fn open_popup(
        _ext: &Extension,
        _button: (f64, f64, f64, f64),
        _frame: (f64, f64, f64, f64),
        _bar: &str,
        _chrome: &WebView,
    ) -> Result<Option<String>, String> {
        Err(support().unwrap_err().into())
    }

    pub fn close_popup() {}

    pub fn popup_shown() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "ghmbeldphafepmbegfdlkpapadhbakde";

    #[test]
    fn bloqueurs_reconnus() {
        let ext = |id: &str, name: &str| Extension {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            version: "1".into(),
            source: Source::Chrome,
            enabled: true,
            dir: id.into(),
            popup: None,
            options: None,
            icon: None,
            all_sites: true,
            firefox: false,
            engine_id: None,
            origin: None,
            permissions: Vec::new(),
            hosts: Vec::new(),
            optional: Vec::new(),
        };
        assert!(blocks_ads(&ext("ddkjiahejlhfcafbddmgiahcphecmpfh", "uBO Lite")));
        assert!(blocks_ads(&ext("autre", "AdGuard AdBlocker")));
        assert!(!blocks_ads(&ext(ID, "Proton Pass")));
    }

    #[test]
    fn fiche_d_autorisations() {
        assert_eq!(permission_label("tabs").as_deref(), Some("Voir vos onglets et leurs adresses"));
        assert_eq!(permission_label("storage"), None);
        assert_eq!(permission_label("truc").as_deref(), Some("truc"));
        assert_eq!(permission_label("https://*/*").as_deref(), Some("Lire et modifier d'autres sites"));
        assert_eq!(labels(&["webRequest".into(), "webRequestBlocking".into(), "alarms".into()]), ["Observer les requêtes des pages"]);
        let hosts = ["https://*.proton.me/*".to_string(), "*://*.proton.me/x".into(), "http://a.fr/".into()];
        assert_eq!(site_labels(&hosts), ["*.proton.me", "a.fr"]);
    }

    #[test]
    fn nouvelles_versions() {
        assert!(is_newer("1.10.2", "1.9"));
        assert!(is_newer("2", "1.99.99"));
        assert!(!is_newer("1.2", "1.2.0"));
        assert!(!is_newer("1.2.3", "1.3"));
        let xml = r#"<?xml version="1.0"?><gupdate><app appid="x" status="ok"><updatecheck codebase="https://x/y.crx" version="2025.1.2" status="ok"/></app></gupdate>"#;
        assert_eq!(chrome_latest(xml).as_deref(), Some("2025.1.2"));
        assert_eq!(chrome_latest(r#"<app><updatecheck status="noupdate"/></app>"#), None);
        assert_eq!(chrome_latest("rien"), None);
    }

    #[test]
    fn mise_a_jour_qui_demande_plus() {
        let base = Extension {
            id: ID.into(),
            name: "x".into(),
            description: String::new(),
            version: "1".into(),
            source: Source::Chrome,
            enabled: true,
            dir: ID.into(),
            popup: None,
            options: None,
            icon: None,
            all_sites: false,
            firefox: false,
            engine_id: None,
            origin: None,
            permissions: vec!["tabs".into()],
            hosts: vec!["https://a.fr/*".into()],
            optional: Vec::new(),
        };
        let mut same = base.clone();
        same.version = "2".into();
        assert!(!same.asks_more_than(&base));
        let mut more = same.clone();
        more.permissions.push("cookies".into());
        assert!(more.asks_more_than(&base));
        let mut all = same.clone();
        all.all_sites = true;
        assert!(all.asks_more_than(&base));
        // Ancienne installation sans `origin` : l'identifiant du Chrome Web Store sert.
        assert_eq!(base.update_origin(), Some(Origin::Chrome(ID.into())));
    }

    #[test]
    fn bouton_du_store() {
        assert_eq!(store_page(&format!("https://chromewebstore.google.com/detail/proton-pass/{ID}?hl=fr")), Some(ID.into()));
        assert_eq!(store_page("https://chromewebstore.google.com/"), None);
        assert_eq!(store_page(&format!("https://exemple.fr/detail/x/{ID}")), None);
        assert_eq!(store_page(&format!("http://chromewebstore.google.com/detail/x/{ID}")), None);
        assert_eq!(store_page(ID), None);
    }

    #[test]
    fn liens_compris() {
        let chrome = Origin::Chrome(ID.into());
        assert_eq!(parse_input(&format!("https://chromewebstore.google.com/detail/proton-pass/{ID}")), Ok(chrome));
        assert!(parse_input(&format!("https://chromewebstore.google.com/detail/proton-pass/{ID}?hl=fr")).is_ok());
        assert_eq!(parse_input(&format!("chrome.google.com/webstore/detail/x/{ID}")), Ok(Origin::Chrome(ID.into())));
        assert_eq!(parse_input(ID), Ok(Origin::Chrome(ID.into())));
        assert_eq!(
            parse_input("https://addons.mozilla.org/fr/firefox/addon/ublock-origin/"),
            Ok(Origin::Firefox("ublock-origin".into()))
        );
        assert!(parse_input("https://chromewebstore.google.com/").is_err());
        assert!(parse_input("https://exemple.fr/extension").is_err());
        assert!(parse_input("bonjour").is_err());
        assert!(firefox_url("a b").ends_with("/a+b/latest.xpi"));
    }

    #[test]
    fn base64_aller_retour() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(base64_decode(&base64_encode(data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert!(base64_decode("@@").is_none());
    }

    /// Un champ protobuf « longueur + octets ».
    fn field(num: u64, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut put = |mut v: u64| loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        };
        put((num << 3) | 2);
        put(value.len() as u64);
        out.extend_from_slice(value);
        out
    }

    #[test]
    fn crx3_lu() {
        let key = b"une cle publique".to_vec();
        let other = b"une autre cle".to_vec();
        let id: Vec<u8> = Sha256::digest(&key)[..16].to_vec();
        let mut header = field(2, &[field(1, &other), field(2, b"sig")].concat());
        header.extend(field(2, &[field(1, &key), field(2, b"sig")].concat()));
        header.extend(field(10000, &field(1, &id)));
        let mut crx = b"Cr24".to_vec();
        crx.extend(3u32.to_le_bytes());
        crx.extend((header.len() as u32).to_le_bytes());
        crx.extend(&header);
        crx.extend(b"PK\x03\x04reste");
        let (zip, found) = crx_zip(&crx).unwrap();
        assert!(zip.starts_with(b"PK"));
        assert_eq!(found.as_deref(), Some(&key[..]));
        assert_eq!(id_from_key(&key).len(), 32);
        assert!(is_store_id(&id_from_key(&key)));
        // Zip seul (.xpi) : pas de clé ; autre chose : refusé.
        assert_eq!(crx_zip(b"PK\x03\x04abc").unwrap().1, None);
        assert!(crx_zip(b"<html>").is_err());
        assert!(crx_zip(b"Cr24\x03\x00\x00\x00\xff\xff\x00\x00").is_err());
    }

    #[test]
    fn identifiant_chrome() {
        // Exemple connu : clé vide → empreinte e3b0c442… → « odlameecjipmbmbejkplpemijjgpljce ».
        assert_eq!(id_from_key(b""), "odlameecjipmbmbejkplpemijjgpljce");
    }

    #[test]
    fn commentaires_retires() {
        let text = "{ // nom\n \"a\": \"http://x/*y*/\", /* bloc */ \"b\": 1 }";
        let v: Value = serde_json::from_str(&strip_comments(text)).unwrap();
        assert_eq!(v["a"], "http://x/*y*/");
        assert_eq!(v["b"], 1);
    }

    #[test]
    fn manifeste_lu() {
        let dir = std::env::temp_dir().join(format!("feather-ext-{}", stamp()));
        fs::create_dir_all(dir.join("_locales/fr")).unwrap();
        fs::create_dir_all(dir.join("icons")).unwrap();
        fs::write(dir.join("popup.html"), "<p>").unwrap();
        fs::write(dir.join("icons/48.png"), "png").unwrap();
        fs::write(dir.join("icons/16.png"), "png").unwrap();
        fs::write(dir.join("_locales/fr/messages.json"), r#"{"appName":{"message":"Mon extension"}}"#).unwrap();
        fs::write(
            dir.join("manifest.json"),
            "\u{feff}{\"manifest_version\":3,\"name\":\"__MSG_appname__\",\"version\":\"1.2\",\"default_locale\":\"en\",
              \"action\":{\"default_popup\":\"popup.html\"},\"options_page\":\"../dehors.html\",
              \"icons\":{\"16\":\"icons/16.png\",\"48\":\"icons/48.png\"},
              \"host_permissions\":[\"<all_urls>\"],\"browser_specific_settings\":{\"gecko\":{\"id\":\"a@b\"}},
              \"permissions\":[\"tabs\",\"storage\",\"https://*.proton.me/*\"],\"optional_permissions\":[\"history\"],
              \"content_scripts\":[{\"matches\":[\"https://*.proton.me/*\",\"https://mail.exemple.fr/*\"]}]}",
        )
        .unwrap();
        let info = read_manifest(&dir).unwrap();
        assert_eq!(info.name, "Mon extension");
        assert_eq!(info.version, "1.2");
        assert_eq!(info.popup.as_deref(), Some("popup.html"));
        assert_eq!(info.options, None);
        assert_eq!(info.icon.as_deref(), Some("icons/48.png"));
        assert!(info.all_sites);
        assert_eq!(info.gecko_id.as_deref(), Some("a@b"));
        assert!(!info.has_key);
        // Les sites sont séparés des autorisations, sans doublons.
        assert_eq!(info.permissions, ["tabs", "storage"]);
        assert_eq!(info.hosts, ["https://*.proton.me/*", "<all_urls>", "https://mail.exemple.fr/*"]);
        assert_eq!(info.optional, ["history"]);
        add_key(&dir, b"cle").unwrap();
        assert!(read_manifest(&dir).unwrap().has_key);
        assert_eq!(manifest_root(&dir).unwrap(), dir);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn complements_avant_l_arriere_plan() {
        let dir = std::env::temp_dir().join(format!("feather-compat-{}", stamp()));
        let write = |manifest: &str| {
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("manifest.json"), manifest).unwrap();
        };
        let manifest = || read_json(&dir.join("manifest.json")).unwrap();
        let wrapper = || fs::read_to_string(dir.join(WRAPPER_FILE)).unwrap();
        // Arrière-plan classique (Proton Pass) : chargé dans un filet.
        write(r#"{"name":"a","background":{"service_worker":"background.js"}}"#);
        add_compat(&dir).unwrap();
        assert_eq!(manifest()["background"]["service_worker"], WRAPPER_FILE);
        let classic = wrapper();
        let version = format!("// Compléments de Feather {}\n", short_hash(COMPAT_JS));
        assert!(classic.starts_with(&version));
        assert!(classic.contains("importScripts('/feather-compat.js');\ntry {\n  importScripts(\"/background.js\");\n} catch (e) {"));
        assert!(classic.contains("reportError(e)"));
        assert_eq!(fs::read_to_string(dir.join(COMPAT_FILE)).unwrap(), COMPAT_JS);
        // Deuxième fois : rien ne change (l'original n'est pas perdu).
        add_compat(&dir).unwrap();
        assert_eq!(wrapper(), classic);
        // Enveloppe des versions précédentes, sans filet : réécrite au chargement suivant.
        fs::write(dir.join(WRAPPER_FILE), "importScripts('/feather-compat.js', \"/background.js\");\n").unwrap();
        add_compat(&dir).unwrap();
        assert_eq!(wrapper(), classic);
        assert_eq!(manifest()["background"]["service_worker"], WRAPPER_FILE);
        // Enveloppe perdue : l'original est introuvable, elle n'est pas réécrite.
        fs::remove_file(dir.join(WRAPPER_FILE)).unwrap();
        assert!(add_compat(&dir).is_err());
        // Module (uBlock Origin Lite).
        write(r#"{"name":"b","background":{"service_worker":"/js/background.js","type":"module"}}"#);
        add_compat(&dir).unwrap();
        let module = format!("{version}import '/feather-compat.js';\nimport \"/js/background.js\";\n");
        assert_eq!(wrapper(), module);
        assert_eq!(manifest()["background"]["type"], "module");
        add_compat(&dir).unwrap();
        assert_eq!(wrapper(), module);
        // Manifeste v2 : les compléments en tête de liste, une seule fois.
        write(r#"{"name":"c","background":{"scripts":["a.js","b.js"]}}"#);
        add_compat(&dir).unwrap();
        add_compat(&dir).unwrap();
        assert_eq!(manifest()["background"]["scripts"], serde_json::json!([COMPAT_FILE, "a.js", "b.js"]));
        // Fenêtre et page d'options : les compléments en premier script, une seule fois.
        write(r#"{"name":"e","action":{"default_popup":"popup.html"},"options_ui":{"page":"opt/options.html?x=1"}}"#);
        fs::write(dir.join("popup.html"), "<!doctype html><html><HEAD lang=\"fr\"><style></style></head><body></body></html>").unwrap();
        fs::create_dir_all(dir.join("opt")).unwrap();
        fs::write(dir.join("opt/options.html"), "<header></header><p>x</p>").unwrap();
        add_compat(&dir).unwrap();
        add_compat(&dir).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("popup.html")).unwrap(),
            format!("<!doctype html><html><HEAD lang=\"fr\">{COMPAT_TAG}<style></style></head><body></body></html>")
        );
        assert_eq!(fs::read_to_string(dir.join("opt/options.html")).unwrap(), format!("{COMPAT_TAG}<header></header><p>x</p>"));
        // Sans arrière-plan : manifeste intact.
        write(r#"{"name":"d"}"#);
        add_compat(&dir).unwrap();
        assert_eq!(manifest(), serde_json::json!({"name": "d"}));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn original_relu_dans_l_enveloppe() {
        // Toutes les versions de l'enveloppe, et un chemin à échapper.
        assert_eq!(wrapped_original("importScripts('/feather-compat.js', \"/background.js\");\n").as_deref(), Some("/background.js"));
        assert_eq!(wrapped_original("import '/feather-compat.js';\nimport \"/js/bg.js\";\n").as_deref(), Some("/js/bg.js"));
        let odd = "/arrière \"plan\"\\x.js";
        assert_eq!(wrapped_original(&wrapper_js(odd, false).unwrap()).as_deref(), Some(odd));
        assert_eq!(wrapped_original(&wrapper_js(odd, true).unwrap()).as_deref(), Some(odd));
        assert_eq!(wrapped_original("importScripts('/feather-compat.js');\n"), None);
    }

    #[test]
    fn sites_declares_par_les_extensions() {
        // Motifs de Proton Pass ; les motifs trop larges sont ignorés.
        let proton = declared("https://*.proton.me/*").unwrap();
        assert_eq!(proton, Declared { scheme: "https".into(), host: "proton.me".into(), subdomains: true });
        assert_eq!(declared("*://Mail.Exemple.fr:8443/boite/*").map(|d| (d.host, d.subdomains)), Some(("mail.exemple.fr".into(), false)));
        for broad in ["<all_urls>", "*://*/*", "https://*/*", "https://*.com/*", "https://localhost/*", "ftp://a.b/*", "https://*.*.fr/*", "pas un motif"] {
            assert_eq!(declared(broad), None, "{broad}");
        }
        let list = [proton, declared("*://mail.exemple.fr/*").unwrap()];
        for url in ["https://account.proton.me/authorize?app=proton-pass-extension", "https://proton.me/", "https://PASS.proton.me/", "http://mail.exemple.fr/", "https://mail.exemple.fr/x"] {
            assert!(site_matches(&list, url), "{url}");
        }
        for url in ["http://account.proton.me/", "https://evilproton.me/", "https://proton.me.exemple.org/", "https://a.mail.exemple.fr/", "navigateur://accueil/", "pas une adresse"] {
            assert!(!site_matches(&list, url), "{url}");
        }
    }

    #[test]
    fn place_de_la_fenetre() {
        let win = (1000.0, 700.0);
        // Barre à gauche : à droite du bouton.
        assert_eq!(popup_frame((10.0, 100.0, 30.0, 26.0), (300.0, 400.0), win, "left"), (48.0, 100.0, 300.0, 400.0));
        // Barre à droite : à gauche du bouton.
        assert_eq!(popup_frame((960.0, 100.0, 30.0, 26.0), (300.0, 400.0), win, "right").0, 652.0);
        // En haut : sous le bouton, sans sortir de la fenêtre.
        assert_eq!(popup_frame((980.0, 9.0, 26.0, 26.0), (300.0, 400.0), win, "top"), (692.0, 43.0, 300.0, 400.0));
        // Trop grande : ramenée à 800×600 au plus, et dans la fenêtre.
        let (x, y, w, h) = popup_frame((10.0, 600.0, 30.0, 26.0), (5000.0, 5000.0), win, "left");
        assert_eq!((w, h), (800.0, 600.0));
        assert!(x + w <= 992.0 && y + h <= 692.0);
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    /// Un zip sans compression, assez pour `tar` (Windows), `ditto` (macOS) et `unzip`.
    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let offset = out.len() as u32;
            let crc = crc32(data);
            let head = |sig: u32, central: bool| {
                let mut h = sig.to_le_bytes().to_vec();
                if central {
                    h.extend(20u16.to_le_bytes());
                }
                for v in [20u16, 0, 0, 0, 0x21] {
                    h.extend(v.to_le_bytes());
                }
                h.extend(crc.to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend(0u16.to_le_bytes());
                if central {
                    for v in [0u16, 0, 0] {
                        h.extend(v.to_le_bytes());
                    }
                    h.extend(0u32.to_le_bytes());
                    h.extend(offset.to_le_bytes());
                }
                h.extend(name.as_bytes());
                h
            };
            out.extend(head(0x0403_4b50, false));
            out.extend(*data);
            central.extend(head(0x0201_4b50, true));
        }
        let start = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        for v in [0u16, 0, files.len() as u16, files.len() as u16] {
            out.extend(v.to_le_bytes());
        }
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    /// De bout en bout : un `.crx` (version 3) choisi comme fichier est décompressé
    /// par l'outil du système, rangé, et reçoit sa clé.
    #[test]
    fn fichier_crx_installe() {
        let key = b"cle publique de test".to_vec();
        let crx_id: Vec<u8> = Sha256::digest(&key)[..16].to_vec();
        let mut header = field(2, &[field(1, &key), field(2, b"sig")].concat());
        header.extend(field(10000, &field(1, &crx_id)));
        let manifest = br#"{ // commentaire
            "manifest_version": 3, "name": "Essai", "version": "0.1",
            "action": { "default_popup": "popup.html", "default_icon": { "32": "icons/32.png" } } }"#;
        let archive = zip(&[
            ("essai/manifest.json", &manifest[..]),
            ("essai/popup.html", b"<p>bonjour</p>"),
            ("essai/icons/32.png", b"\x89PNG"),
        ]);
        let mut crx = b"Cr24".to_vec();
        crx.extend(3u32.to_le_bytes());
        crx.extend((header.len() as u32).to_le_bytes());
        crx.extend(&header);
        crx.extend(&archive);

        let ext = prepare(Request::File { name: "essai.crx".into(), bytes: crx }).unwrap();
        assert_eq!(ext.id, id_from_key(&key));
        assert_eq!(ext.name, "Essai");
        assert_eq!(ext.source, Source::Fichier);
        assert_eq!(ext.popup.as_deref(), Some("popup.html"));
        assert_eq!(ext.icon.as_deref(), Some("icons/32.png"));
        assert!(ext.dir.ends_with("/essai"));
        let path = ext.path().unwrap();
        assert_eq!(fs::read_to_string(path.join("popup.html")).unwrap(), "<p>bonjour</p>");
        assert!(read_manifest(&path).unwrap().has_key);
        assert!(icon_data(&ext).unwrap().starts_with("data:image/png;base64,"));
        fs::remove_dir_all(root().unwrap().join(ext.dir.split('/').next().unwrap())).unwrap();

        // Pas une extension : refusé, sans rien laisser derrière.
        assert!(prepare(Request::File { name: "x.zip".into(), bytes: zip(&[("a.txt", b"a")]) }).is_err());
        let left = fs::read_dir(root().unwrap()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".nouvelle")).count();
        assert_eq!(left, 0);
    }
}

