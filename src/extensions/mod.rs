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
//! Le téléchargement passe par `curl` (https seulement), comme la mise à
//! jour ; la décompression par `archive`, qui vérifie toute l'archive avant
//! d'écrire quoi que ce soit.

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

mod archive;
#[cfg(any(target_os = "macos", test))]
mod compat;

// Le moteur de chaque système.
#[cfg(windows)]
#[path = "windows.rs"]
mod engine;
#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod engine;
#[cfg(not(any(windows, target_os = "macos")))]
#[path = "linux.rs"]
mod engine;

#[cfg(test)]
mod tests;

/// Fichier qui liste les extensions installées.
const FILE: &str = "extensions.json";
/// Dossiers d'extensions qui ne servent plus : gardés une semaine dans
/// `extensions/.corbeille` avant d'être effacés, au cas où la liste se tromperait.
const TRASH: &str = ".corbeille";
const TRASH_KEPT: u64 = 7 * 24 * 3600;
/// Taille maximale d'une extension téléchargée ou choisie (octets).
pub const MAX_ARCHIVE: usize = 100 * 1024 * 1024;
/// Taille maximale d'une extension décompressée, et nombre de fichiers.
const MAX_UNPACKED: u64 = 300 * 1024 * 1024;
const MAX_FILES: usize = 20_000;
/// Icône envoyée à l'interface : au-delà, on garde la pastille avec l'initiale.
const MAX_ICON: u64 = 96 * 1024;
/// Demandes d'autorisation d'une même extension en attente de réponse : au-delà, refusées.
const MAX_ASKS: usize = 2;
/// Motifs qui donnent accès à tous les sites.
const ALL_SITES: [&str; 4] = ["<all_urls>", "*://*/*", "http://*/*", "https://*/*"];
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
    /// Accordé plus tard, à la demande de l'extension et avec l'accord de
    /// l'utilisateur (macOS, voir `extension_asks`) : autorisations et motifs
    /// de sites, rendus au moteur à chaque lancement.
    #[serde(default)]
    pub granted: Vec<String>,
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
        granted: Vec::new(),
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
/// Un `.xpi` ou un `.zip` est déjà un zip, sans clé. La clé d'un `.crx` n'est
/// rendue que si sa signature est vérifiée (voir `crx3_key`) : sans cela,
/// n'importe qui pourrait prendre l'identifiant d'une autre extension (la clé
/// publique de Proton Pass est publique, sa clé privée non).
pub fn crx_zip(bytes: &[u8]) -> Result<(&[u8], Option<Vec<u8>>), String> {
    const BAD: &str = "ce fichier n'est pas une extension (.crx, .xpi ou .zip)";
    if bytes.starts_with(b"PK\x03\x04") {
        return Ok((bytes, None));
    }
    if !bytes.starts_with(b"Cr24") || bytes.len() < 16 {
        return Err(BAD.into());
    }
    let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let (zip_start, header) = match u32_at(4) {
        // Ancien format, signé en SHA-1, que Chrome refuse depuis 2019 : sa clé
        // n'est pas vérifiée, donc pas gardée (l'extension s'installe comme un zip).
        Some(2) => (16 + u32_at(8).ok_or(BAD)? + u32_at(12).ok_or(BAD)?, None),
        Some(3) => {
            let hlen = u32_at(8).ok_or(BAD)?;
            (12 + hlen, Some(bytes.get(12..12 + hlen).ok_or(BAD)?))
        }
        _ => return Err(BAD.into()),
    };
    let zip = bytes.get(zip_start..).filter(|z| z.starts_with(b"PK\x03\x04")).ok_or(BAD)?;
    let key = match header {
        Some(header) => crx3_key(header, zip)?,
        None => None,
    };
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

/// Clé publique vérifiée de l'en-tête d'un `.crx` (version 3) : celle qui
/// correspond à l'identifiant signé (`CrxFileHeader` : 2 = preuves RSA,
/// 10000 = données signées). Comme dans Chrome, chaque preuve RSA (celle de
/// l'auteur, celle du Chrome Web Store) doit signer « CRX3 SignedData », les
/// données signées et l'archive : une seule fausse et le fichier est refusé.
/// `None` : rien de vérifiable (pas d'identifiant signé, ou sa clé n'est pas
/// parmi les preuves RSA).
fn crx3_key(header: &[u8], zip: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut proofs = Vec::new();
    let mut signed = None;
    for (field, value) in proto_fields(header) {
        match field {
            2 => {
                let parts = proto_fields(value);
                let part = |n: u64| parts.iter().find(|(f, _)| *f == n).map_or(&[][..], |(_, v)| *v);
                proofs.push((part(1), part(2)));
            }
            10000 => signed = Some(value),
            _ => {}
        }
    }
    let crx_id = signed.and_then(|s| proto_fields(s).into_iter().find(|(f, _)| *f == 1)).map(|(_, id)| id);
    let (Some(signed), Some(crx_id)) = (signed, crx_id.filter(|id| id.len() == 16)) else { return Ok(None) };
    let digest: [u8; 32] = Sha256::new()
        .chain_update(b"CRX3 SignedData\x00")
        .chain_update((signed.len() as u32).to_le_bytes())
        .chain_update(signed)
        .chain_update(zip)
        .finalize()
        .into();
    for (key, signature) in &proofs {
        if !crate::rsa::public_key(key).is_some_and(|key| crate::rsa::verify_sha256(&key, &digest, signature)) {
            return Err("signature invalide : le fichier a été modifié ou abîmé".into());
        }
    }
    Ok(proofs.into_iter().map(|(key, _)| key).find(|key| Sha256::digest(key).get(..16) == Some(crx_id)).map(<[u8]>::to_vec))
}

/// Décompresse l'extension dans `dest` et renvoie la clé du `.crx`.
fn unpack(bytes: &[u8], dest: &Path) -> Result<Option<Vec<u8>>, String> {
    let (zip, key) = crx_zip(bytes)?;
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    archive::extract(zip, dest, MAX_UNPACKED, MAX_FILES)?;
    Ok(key)
}

/// Refuse les liens symboliques (ils pourraient viser hors du dossier) et les
/// extensions démesurées : seconde vérification après `archive::extract`, et
/// la seule pour un dossier choisi (`copy_dir`).
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
    let all_sites = lists.filter_map(Value::as_str).any(|p| ALL_SITES.contains(&p));
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
    if is_site_pattern(permission) {
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

/// Un motif de sites (`https://*.proton.me/*`, `<all_urls>`), pas un nom d'autorisation.
fn is_site_pattern(item: &str) -> bool {
    item.contains("://") || item == "<all_urls>"
}

/// Ce qu'une extension demande en plus, en clair (voir `extension_asks`) ;
/// vide si rien ne touche aux pages ni aux données.
fn ask_labels(items: &[String]) -> Vec<String> {
    let (sites, permissions): (Vec<String>, Vec<String>) = items.iter().cloned().partition(|i| is_site_pattern(i));
    let mut out = labels(&permissions);
    if sites.iter().any(|s| ALL_SITES.contains(&s.as_str())) {
        out.push("Lire et modifier toutes les pages que vous visitez".into());
    } else if !sites.is_empty() {
        out.push(format!("Lire et modifier les pages de {}", site_labels(&sites).join(", ")));
    }
    out
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
#[cfg(target_os = "macos")]
pub fn declared_site(url: &str) -> bool {
    DECLARED.read().is_ok_and(|list| site_matches(&list, url))
}

/// Vrai si l'adresse est sur l'un des sites de `list`.
#[cfg(any(target_os = "macos", test))]
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

/// Une erreur du moteur pour le panneau : en français quand elle est connue.
fn explain_error(error: &str) -> String {
    // macOS 15 (Proton Pass) : l'arrière-plan d'une extension récente ne peut
    // pas suivre les requêtes ; le reste de l'extension marche.
    if error.contains("cannot listen to `webRequest`") {
        return "WebKit ne laisse pas cette extension suivre les requêtes réseau (webRequest) : les fonctions qui en dépendent manquent sur ce Mac.".into();
    }
    error.to_string()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// Extension téléchargée qui attend l'accord de l'utilisateur : la fiche du
/// panneau montre ce qu'elle pourra lire et faire.
pub struct Pending {
    pub ext: Extension,
    /// Mise à jour d'une extension déjà installée (qui demande plus qu'avant).
    pub update: bool,
    /// Son icône, lue une fois (avant, relue sur le disque à chaque envoi de l'état).
    pub icon: Option<String>,
}

/// Demande d'autorisation d'une extension qui attend la réponse de
/// l'utilisateur (voir `extension_asks`) : son numéro chez le moteur, et ce
/// qu'elle demande (autorisations et motifs de sites).
pub struct Ask {
    number: u32,
    id: String,
    items: Vec<String>,
}

/// Les extensions installées, et ce que montre le panneau.
#[derive(Default)]
pub struct Extensions {
    list: Vec<Extension>,
    /// Icônes en `data:` URL, envoyées à l'interface à part de l'état, et
    /// seulement quand elles changent : `icons_version` change avec elles.
    icons: HashMap<String, String>,
    icons_version: u64,
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
    /// Demandes d'autorisation en attente, posées une à une dans le bandeau du bas.
    asks: Vec<Ask>,
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
        // La liste ne fait foi que lue dans le fichier lui-même. Reprise de la
        // copie `.bak`, ou illisible, elle peut oublier des extensions
        // installées : leurs dossiers ne sont alors pas rangés (avant, un
        // fichier illisible effaçait toutes les extensions).
        let (mut saved, complete): (Saved, bool) = storage::read_json_checked(FILE).unwrap_or_default();
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
        if complete {
            me.sweep();
        }
        me.sync_declared();
        me
    }

    pub fn save(&self) {
        let saved = Saved { list: self.list.clone(), checked: self.checked };
        if let Err(err) = storage::write_json(FILE, &saved, true) {
            eprintln!("liste des extensions non enregistrée : {err}");
        }
    }

    /// Range les dossiers qui ne servent plus (anciennes versions, essais ratés).
    fn sweep(&self) {
        if let Some(root) = root() {
            sweep_in(&root, &self.list, now_secs());
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
        self.icons_version += 1;
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
        if self.icons.remove(id).is_some() {
            self.icons_version += 1;
        }
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

    /// Change à chaque installation ou retrait d'une icône (voir `icons_json`).
    pub fn icons_version(&self) -> u64 {
        self.icons_version
    }

    /// Les icônes, par identifiant d'extension, pour `navigateur.setExtensionIcons`.
    pub fn icons_json(&self) -> Value {
        json!(self.icons)
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
            // La plus ancienne demande d'autorisation, posée dans le bandeau du bas.
            "ask": self.asks.first().map(|a| json!({
                "request": a.number,
                "name": self.get(&a.id).map_or("", |e| e.name.as_str()),
                "items": ask_labels(&a.items),
            })),
            "pending": self.pending.as_ref().map(|p| {
                let e = &p.ext;
                json!({
                    "name": e.name,
                    "version": e.version,
                    "update": p.update,
                    "icon": p.icon,
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

pub use engine::{add, answer, close_popup, open_popup, options_url, page_url, popup_shown, remove, replace, set_enabled, start};
#[cfg(target_os = "macos")]
pub use engine::sync_tabs;

/// Un onglet non privé tel que les extensions le voient (macOS, voir `sync_tabs`).
#[cfg(target_os = "macos")]
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
#[cfg(windows)]
#[derive(Debug, Default, Deserialize)]
pub struct PopupMessage {
    #[serde(default)]
    pub close: bool,
    pub w: Option<f64>,
    pub h: Option<f64>,
}

/// Script de la petite fenêtre (Windows) : `window.close()` et Échap la ferment,
/// et elle donne la taille de son contenu, comme dans Chrome.
#[cfg(windows)]
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
        let icon = icon_data(&ext);
        self.extensions.pending = Some(Pending { ext, update: old.is_some(), icon });
        if automatic {
            let text = serde_json::to_string(&format!(
                "La mise à jour de {name} demande de nouvelles autorisations : ouvrez Extensions pour l'accepter."
            ))
            .unwrap_or_default();
            self.chrome_call(&format!("navigateur.showNotice({text})"));
        }
    }

    /// Une extension demande plus que ce qu'elle a reçu à l'installation
    /// (macOS : `permissions.request`, accès à d'autres sites). Avant, WebKit
    /// recevait toujours oui. Comme dans Chrome, ce qui ne touche ni aux pages
    /// ni aux données est accordé sans question ; le reste attend la réponse de
    /// l'utilisateur, dans le bandeau du bas (le banc, lui, accepte).
    pub(crate) fn extension_asks(&mut self, number: u32, id: String, items: Vec<String>) {
        let waiting = self.extensions.asks.iter().filter(|a| a.id == id).count();
        if !self.extensions.get(&id).is_some_and(|e| e.enabled) || waiting >= MAX_ASKS {
            answer(number, false);
        } else if ask_labels(&items).is_empty() || crate::banc::demande_extension(&id, &items) {
            self.grant_extension(&id, &items);
            answer(number, true);
        } else {
            self.extensions.asks.push(Ask { number, id, items });
            self.push_state();
        }
    }

    /// Réponse du bandeau : tout ce qui était demandé, ou rien.
    pub(crate) fn answer_extension(&mut self, number: u32, allow: bool) {
        let Some(at) = self.extensions.asks.iter().position(|a| a.number == number) else { return };
        let ask = self.extensions.asks.remove(at);
        if allow {
            self.grant_extension(&ask.id, &ask.items);
        }
        answer(number, allow);
        self.push_state();
    }

    /// Retenu pour les prochains lancements (voir `Extension::granted`).
    fn grant_extension(&mut self, id: &str, items: &[String]) {
        let Some(ext) = self.extensions.get_mut(id) else { return };
        for item in items {
            if !ext.granted.contains(item) {
                ext.granted.push(item.clone());
            }
        }
        self.extensions.save();
    }

    /// Extension retirée, coupée ou remplacée : ses demandes en attente reçoivent non.
    fn drop_asks(&mut self, id: &str) {
        let (dropped, kept): (Vec<Ask>, Vec<Ask>) = std::mem::take(&mut self.extensions.asks).into_iter().partition(|a| a.id == id);
        self.extensions.asks = kept;
        for ask in dropped {
            answer(ask.number, false);
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
            // Mise à jour : on garde le choix « activée » de l'ancienne version,
            // et ce que l'utilisateur lui avait accordé.
            ext.enabled = old.enabled;
            ext.granted = old.granted.clone();
            self.drop_asks(&ext.id);
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
        // Les erreurs notées par WebKit en lisant le manifeste, avant que le
        // contexte existe, ne sont jamais annoncées : relues ici.
        if cfg!(target_os = "macos") {
            self.extension_errors();
        }
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
                let shown = errors.iter().take(MAX_ERRORS).map(|e| explain_error(e).chars().take(300).collect()).collect();
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
        if !enabled {
            self.drop_asks(id);
        }
        self.extensions.save();
        self.extensions.sync_declared();
        set_enabled(&ext, enabled);
        self.sync_ad_blocker();
    }

    pub(crate) fn remove_extension(&mut self, id: &str) {
        if self.extensions.popup_id() == Some(id) {
            self.close_extension_popup();
        }
        self.drop_asks(id);
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
    #[cfg(windows)]
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
    #[cfg(target_os = "macos")]
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

/// Voir `Extensions::sweep` : les dossiers que `list` n'utilise pas vont à la
/// corbeille (une installation interrompue est effacée tout de suite), et ce
/// qui y attend depuis une semaine est effacé.
fn sweep_in(root: &Path, list: &[Extension], now: u64) {
    let remove = |path: &Path| {
        let _ = if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    };
    let trash = root.join(TRASH);
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == TRASH || list.iter().any(|e| e.dir.split('/').next() == Some(name.as_str())) {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() || name.starts_with(".nouvelle-") {
                remove(&path);
            } else if fs::create_dir_all(&trash).is_ok() {
                let _ = fs::rename(&path, trash.join(format!("{now}-{name}")));
            }
        }
    }
    let Ok(waiting) = fs::read_dir(&trash) else { return };
    for entry in waiting.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let since = name.split('-').next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        if now.saturating_sub(since) >= TRASH_KEPT {
            remove(&entry.path());
        }
    }
}

/// Efface le dossier d'une extension remplacée ou retirée, quand le moteur l'a lâché.
/// Un dossier encore pris sera rangé au prochain démarrage (voir `sweep`).
fn remove_folder_later(ext: &Extension) {
    let Some(root) = root() else { return };
    let Some(folder) = ext.dir.split('/').next().filter(|f| !f.is_empty() && *f != "." && *f != "..") else { return };
    let path = root.join(folder);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let _ = fs::remove_dir_all(path);
    });
}
