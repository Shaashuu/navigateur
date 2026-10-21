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
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;
use wry::WebViewBuilder;

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
}

impl Extension {
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

/// Les extensions installées, et ce que montre le panneau.
#[derive(Default)]
pub struct Extensions {
    list: Vec<Extension>,
    icons: HashMap<String, String>,
    /// Ce que le moteur a refusé, par extension.
    problems: HashMap<String, String>,
    /// Installation en cours (texte affiché), dernière erreur.
    pub busy: Option<String>,
    pub error: Option<String>,
    /// Petite fenêtre ouverte : l'extension et (Windows) sa webview.
    pub popup: Option<Popup>,
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
}

impl Extensions {
    pub fn load() -> Self {
        let mut saved: Saved = storage::read_json(FILE).unwrap_or_default();
        saved.list.retain(|e| e.path().is_some_and(|p| p.join("manifest.json").is_file()));
        let mut me = Self { list: saved.list, ..Self::default() };
        for ext in &me.list {
            if let Some(icon) = icon_data(ext) {
                me.icons.insert(ext.id.clone(), icon);
            }
        }
        me.sweep();
        me
    }

    pub fn save(&self) {
        let saved = Saved { list: self.list.clone() };
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
        old
    }

    pub fn remove(&mut self, id: &str) -> Option<Extension> {
        let i = self.list.iter().position(|e| e.id == id)?;
        self.icons.remove(id);
        self.problems.remove(id);
        let ext = self.list.remove(i);
        self.save();
        Some(ext)
    }

    pub fn set_problem(&mut self, id: &str, problem: Option<String>) {
        match problem {
            Some(p) => self.problems.insert(id.to_string(), p),
            None => self.problems.remove(id),
        };
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
        })
    }
}

/// Prépare une webview pour les extensions. Sous Windows, toutes les webviews
/// doivent avoir le même réglage (sinon WebView2 refuse de les créer) ; sous
/// macOS, seules les pages non privées (`attach`) reçoivent les extensions.
pub fn prepare_builder(builder: WebViewBuilder<'_>, attach: bool) -> WebViewBuilder<'_> {
    engine::prepare_builder(builder, attach)
}

pub use engine::{add, close_popup, open_popup, options_url, popup_shown, remove, replace, set_enabled, start};

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

    fn start_prepare(&mut self, request: Request, busy: &str) {
        if let Err(reason) = support() {
            self.extensions.error = Some(reason.into());
            return;
        }
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

    /// L'extension est rangée sur le disque : on la confie au moteur.
    pub(crate) fn extension_prepared(&mut self, result: Result<Box<Extension>, String>) {
        self.extensions.busy = None;
        let ext = match result {
            Ok(ext) => *ext,
            Err(err) => {
                self.extensions.error = Some(format!("Installation impossible : {err}."));
                return;
            }
        };
        if self.extensions.popup_id() == Some(ext.id.as_str()) {
            self.close_extension_popup();
        }
        let mut ext = ext;
        if let Some(old) = self.extensions.get(&ext.id) {
            // Mise à jour : on garde le choix « activée » de l'ancienne version.
            ext.enabled = old.enabled;
        }
        match self.extensions.insert(ext.clone()) {
            Some(old) => {
                replace(&old, &ext, &self.proxy);
                remove_folder_later(&old);
            }
            None => add(&ext, &self.proxy),
        }
    }

    /// Réponse du moteur : l'identifiant WebView2, ou ce qui n'a pas marché.
    pub(crate) fn extension_loaded(&mut self, id: &str, result: Result<Option<String>, String>) {
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
    }

    pub(crate) fn set_extension(&mut self, id: &str, enabled: bool) {
        if self.extensions.popup_id() == Some(id) {
            self.close_extension_popup();
        }
        let Some(ext) = self.extensions.get_mut(id) else { return };
        ext.enabled = enabled;
        let ext = ext.clone();
        self.extensions.save();
        set_enabled(&ext, enabled);
    }

    pub(crate) fn remove_extension(&mut self, id: &str) {
        if self.extensions.popup_id() == Some(id) {
            self.close_extension_popup();
        }
        if let Some(ext) = self.extensions.remove(id) {
            remove(&ext);
            remove_folder_later(&ext);
        }
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
        match open_popup(&ext, frame, &self.chrome) {
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
        match self.surface.attach(prepare_builder(builder, true), &self.window) {
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
        if crate::network::is_web(url) || url.starts_with("chrome-extension://") {
            self.close_extension_popup();
            self.open_tab(url, false);
        }
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

    pub fn prepare_builder(builder: WebViewBuilder<'_>, _attach: bool) -> WebViewBuilder<'_> {
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

    fn page_url(ext: &Extension, page: &str) -> Option<String> {
        Some(format!("chrome-extension://{}/{page}", ext.engine_id.as_ref()?))
    }

    pub fn options_url(ext: &Extension) -> Option<String> {
        page_url(ext, ext.options.as_ref()?)
    }

    /// Adresse de la petite fenêtre : la webview est créée par `App` (voir main.rs).
    pub fn open_popup(ext: &Extension, _frame: (f64, f64, f64, f64), _chrome: &WebView) -> Result<Option<String>, String> {
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
    use std::cell::RefCell;
    use std::collections::HashMap;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSPopover, NSPopoverBehavior, NSView};
    use objc2_foundation::{NSError, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL};
    use objc2_web_kit::{
        WKWebExtension, WKWebExtensionContext, WKWebExtensionContextPermissionStatus, WKWebExtensionController,
        WKWebViewConfiguration, WKWebsiteDataStore,
    };
    use tao::event_loop::EventLoopProxy;
    use wry::{WebView, WebViewBuilder, WebViewBuilderExtMacos, WebViewExtMacOS};

    use super::Extension;
    use crate::UserEvent;

    pub const PLATFORM: &str = "macos";

    #[derive(Default)]
    struct State {
        /// Une extension au moins est installée : les nouvelles pages reçoivent le
        /// contrôleur, même avant que WebKit ait fini de la charger (démarrage).
        wanted: bool,
        controller: Option<Retained<WKWebExtensionController>>,
        contexts: HashMap<String, Retained<WKWebExtensionContext>>,
        popover: Option<Retained<NSPopover>>,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::default();
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
        STATE.with_borrow_mut(|s| {
            // SAFETY : fil principal, classe présente depuis macOS 15.4 (vérifié ci-dessus).
            Some(s.controller.get_or_insert_with(|| unsafe { WKWebExtensionController::new(mtm) }).clone())
        })
    }

    /// Une page non privée reçoit le contrôleur des extensions, dès qu'il y en a une.
    pub fn prepare_builder(builder: WebViewBuilder<'_>, attach: bool) -> WebViewBuilder<'_> {
        let Some(mtm) = MainThreadMarker::new() else { return builder };
        if !attach || !STATE.with_borrow(|s| s.wanted) {
            return builder;
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

    pub fn start(_chrome: &WebView, list: &[Extension], proxy: &EventLoopProxy<UserEvent>) {
        if support().is_err() {
            return;
        }
        for ext in list {
            add(ext, proxy);
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
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&path.to_string_lossy()), true);
        // SAFETY : fil principal, adresse de fichier valide, rappel conservé par WebKit.
        unsafe { WKWebExtension::extensionWithResourceBaseURL_completionHandler(&url, &block, mtm) };
    }

    fn context(id: &str) -> Option<Retained<WKWebExtensionContext>> {
        STATE.with_borrow(|s| s.contexts.get(id).cloned())
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

    pub fn options_url(ext: &Extension) -> Option<String> {
        let context = context(&ext.id)?;
        // SAFETY : fil principal, contexte valide.
        let url = unsafe { context.optionsPageURL() }?;
        url.absoluteString().map(|s| s.to_string())
    }

    /// Ouvre la fenêtre de l'extension dans une bulle (celle que WebKit prépare),
    /// contre son bouton. Sans fenêtre, le clic est transmis à l'extension.
    pub fn open_popup(ext: &Extension, frame: (f64, f64, f64, f64), chrome: &WebView) -> Result<Option<String>, String> {
        let context = context(&ext.id).ok_or("L'extension est encore en cours de chargement.")?;
        // SAFETY : fil principal, objets valides ; la bulle s'attache à la vue de l'interface.
        unsafe {
            let action = context.actionForTab(None).ok_or("Cette extension n'a pas de bouton.")?;
            if !action.presentsPopup() {
                context.performActionForTab(None);
                return Ok(None);
            }
            let popover = action.popupPopover().ok_or("La fenêtre de l'extension n'a pas pu s'ouvrir.")?;
            let webview = chrome.webview();
            let view: &NSView = &webview;
            let (x, y, w, h) = frame;
            let height = view.frame().size.height;
            let y = if view.isFlipped() { y } else { height - y - h };
            let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
            popover.setBehavior(NSPopoverBehavior::Transient);
            popover.showRelativeToRect_ofView_preferredEdge(rect, view, NSRectEdge::MaxX);
            STATE.with_borrow_mut(|s| s.popover = Some(popover));
        }
        Ok(None)
    }

    pub fn close_popup() {
        if let Some(popover) = STATE.with_borrow_mut(|s| s.popover.take()) {
            // SAFETY : fil principal, bulle valide.
            unsafe { popover.performClose(None) };
        }
    }

    /// La bulle se ferme seule au clic ailleurs : est-elle encore affichée ?
    pub fn popup_shown() -> bool {
        STATE.with_borrow(|s| s.popover.as_ref().is_some_and(|p| p.isShown()))
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

    pub fn prepare_builder(builder: WebViewBuilder<'_>, _attach: bool) -> WebViewBuilder<'_> {
        builder
    }

    pub fn start(_chrome: &WebView, _list: &[Extension], _proxy: &EventLoopProxy<UserEvent>) {}

    pub fn add(_ext: &Extension, _proxy: &EventLoopProxy<UserEvent>) {}

    pub fn set_enabled(_ext: &Extension, _on: bool) {}

    pub fn remove(_ext: &Extension) {}

    pub fn replace(_old: &Extension, _new: &Extension, _proxy: &EventLoopProxy<UserEvent>) {}

    pub fn options_url(_ext: &Extension) -> Option<String> {
        None
    }

    pub fn open_popup(_ext: &Extension, _frame: (f64, f64, f64, f64), _chrome: &WebView) -> Result<Option<String>, String> {
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
              \"host_permissions\":[\"<all_urls>\"],\"browser_specific_settings\":{\"gecko\":{\"id\":\"a@b\"}}}",
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
        add_key(&dir, b"cle").unwrap();
        assert!(read_manifest(&dir).unwrap().has_key);
        assert_eq!(manifest_root(&dir).unwrap(), dir);
        fs::remove_dir_all(&dir).unwrap();
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

