//! Bloqueur de publicités : les listes publiques EasyList, EasyPrivacy et
//! Liste FR, téléchargées au premier lancement puis chaque semaine (elles ne
//! sont pas incluses dans Feather : leurs auteurs les mettent à jour sans cesse).
//!
//! Deux sortes de règles sont gardées :
//! - les domaines de publicité (`||regie.com^`) : leurs requêtes sont refusées
//!   (sous Windows par l'événement `WebResourceRequested`, voir `trackers.rs` ;
//!   sous macOS par des listes de règles WebKit compilées une fois) ;
//! - les encarts à masquer (`##.pub`) : la page envoie les `id` et les classes
//!   qu'elle contient (voir `assets/page.js`), Feather répond par les seuls
//!   sélecteurs utiles. La page ne reçoit pas les 15 000 règles à chaque visite.
//!
//! Les règles plus fines (chemins, expressions, options par site) sont ignorées :
//! le but est de bloquer l'essentiel sans casser de site ni peser en mémoire.

// Le filtrage requête par requête ne sert que sous Windows (WebKit applique ses
// propres règles) ; ailleurs, il n'est utilisé que par les tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tao::event_loop::EventLoopProxy;
use wry::WebView;

use crate::UserEvent;
use crate::storage;

/// Les listes utilisées : nom du fichier, nom affiché, adresse.
/// (Copies GitHub des listes officielles d'easylist.to, mises à jour en même temps.)
pub const LISTES: &[(&str, &str, &str)] = &[
    ("easylist.txt", "EasyList", "https://raw.githubusercontent.com/easylist/easylist/gh-pages/easylist.txt"),
    ("easyprivacy.txt", "EasyPrivacy", "https://raw.githubusercontent.com/easylist/easylist/gh-pages/easyprivacy.txt"),
    ("liste_fr.txt", "Liste FR", "https://raw.githubusercontent.com/easylist/listefr/master/liste_fr.txt"),
];

/// Une liste plus vieille est téléchargée de nouveau.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Au-delà, un fichier n'est pas lu (une liste fait quelques Mo).
const MAX_FILE: u64 = 30 * 1024 * 1024;

// --- Ensemble de domaines ----------------------------------------------------

/// Domaines triés, rangés bout à bout dans un seul texte (100 000 domaines
/// tiennent ainsi en 2 Mo, au lieu du double dans un `HashSet<String>`).
#[derive(Default)]
pub struct DomainSet {
    text: String,
    ends: Vec<u32>,
}

impl DomainSet {
    fn from(mut list: Vec<String>) -> Self {
        list.sort_unstable();
        list.dedup();
        let mut set = DomainSet { text: String::with_capacity(list.iter().map(String::len).sum()), ends: Vec::with_capacity(list.len()) };
        for d in &list {
            set.text.push_str(d);
            set.ends.push(set.text.len() as u32);
        }
        set
    }

    fn get(&self, i: usize) -> &str {
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.text[start..self.ends[i] as usize]
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        (0..self.len()).map(|i| self.get(i))
    }

    fn contains(&self, domain: &str) -> bool {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.get(mid).cmp(domain) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }
}

/// L'hôte puis ses domaines parents : « a.b.fr », « b.fr », « fr ».
fn suffixes(host: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(host), |h| h.split_once('.').map(|(_, rest)| rest)).filter(|h| !h.is_empty())
}

// --- Règles ------------------------------------------------------------------

/// Une exception (`@@||cdn.regie.com/lecteur/$domain=site.fr`) : les requêtes vers
/// ce domaine, sous ce chemin, depuis ces sites, passent malgré le blocage.
#[derive(Debug, PartialEq)]
pub struct Exception {
    /// Début du chemin (vide : tout le domaine).
    path: Box<str>,
    /// Sites des pages concernées (vide : tous).
    sites: Vec<Box<str>>,
}

/// Domaines dont les requêtes sont refusées.
#[derive(Default)]
pub struct NetworkRules {
    pub blocked: DomainSet,
    /// Exceptions, par domaine.
    exceptions: HashMap<Box<str>, Vec<Exception>>,
    /// Sites sur lesquels rien n'est bloqué (`@@||site.fr^$document`).
    pages_allowed: DomainSet,
}

/// Hôte et chemin d'une adresse, sans analyser toute l'URL (appelé pour chaque requête).
fn split_url(url: &str) -> (&str, &str) {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host_port = &rest[..end];
    let host_port = host_port.rsplit_once('@').map_or(host_port, |(_, h)| h);
    (host_port.split(':').next().unwrap_or(""), &rest[end..])
}

impl NetworkRules {
    /// Vrai si la requête vers `url` doit être refusée pour une page de `page_host`
    /// (hôtes en minuscules). Comme pour les traqueurs, un site de la liste visité
    /// directement n'est pas bloqué.
    pub fn blocks(&self, url: &str, page_host: &str) -> bool {
        let (host, path) = split_url(url);
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let listed = |h: &str| suffixes(h).any(|d| self.blocked.contains(d));
        if !listed(&host) || listed(page_host) || suffixes(page_host).any(|d| self.pages_allowed.contains(d)) {
            return false;
        }
        let excepted = suffixes(&host).filter_map(|d| self.exceptions.get(d)).flatten().any(|e| {
            path.starts_with(&*e.path) && (e.sites.is_empty() || e.sites.iter().any(|s| suffixes(page_host).any(|p| p == &**s)))
        });
        !excepted
    }
}

/// Encarts à masquer.
#[derive(Default)]
pub struct Cosmetic {
    /// Règles générales, rangées par leur premier `#id` ou `.classe`.
    generic: HashMap<Box<str>, Vec<Box<str>>>,
    /// Règles générales sans `id` ni classe en tête (peu nombreuses) : toujours envoyées.
    always: Vec<Box<str>>,
    /// Règles propres à un site (`lemonde.fr##.pub`).
    by_host: HashMap<Box<str>, Vec<Box<str>>>,
    /// Exceptions propres à un site (`lemonde.fr#@#.pub`).
    except: HashMap<Box<str>, Vec<Box<str>>>,
    /// Sites où les règles générales ne s'appliquent pas (`$generichide`).
    no_generic: HashSet<Box<str>>,
}

impl Cosmetic {
    pub fn len(&self) -> usize {
        self.generic.values().map(Vec::len).sum::<usize>() + self.always.len() + self.by_host.values().map(Vec::len).sum::<usize>()
    }

    /// Sélecteurs à masquer sur `host` pour ces `#id` et `.classes` ; `first` : premier
    /// envoi de la page, qui reçoit aussi les règles du site et les règles « toujours ».
    pub fn selectors<'a>(&'a self, host: &str, tokens: &[String], first: bool) -> Vec<&'a str> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let excepted: Vec<&str> = suffixes(&host)
            .filter_map(|h| self.except.get(h))
            .flatten()
            .map(|s| &**s)
            .collect();
        let generic = !suffixes(&host).any(|h| self.no_generic.contains(h));
        let mut out: Vec<&str> = Vec::new();
        if first {
            out.extend(suffixes(&host).filter_map(|h| self.by_host.get(h)).flatten().map(|s| &**s));
            if generic {
                out.extend(self.always.iter().map(|s| &**s));
            }
        }
        if generic {
            out.extend(tokens.iter().filter_map(|t| self.generic.get(t.as_str())).flatten().map(|s| &**s));
        }
        out.retain(|s| !excepted.contains(s));
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Résultat de la lecture des listes.
#[derive(Default)]
pub struct Filters {
    pub network: NetworkRules,
    pub cosmetic: Cosmetic,
}

impl std::fmt::Debug for Filters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Filters({} domaines, {} encarts)", self.network.blocked.len(), self.cosmetic.len())
    }
}

/// Options acceptées sur une règle de domaine : elles ne font que restreindre le
/// type de requête (bloquer tout le domaine reste sans risque pour une régie).
const OPTIONS_OK: &[&str] = &[
    "third-party", "3p", "script", "image", "subdocument", "xmlhttprequest", "xhr", "media", "object",
    "ping", "websocket", "other", "font", "stylesheet", "important", "all",
];

/// Options d'exception qui ne concernent que le masquage des encarts.
const OPTIONS_COSMETIQUES: &[&str] = &["generichide", "ghide", "elemhide", "ehide", "specifichide", "shide"];

/// Pseudo-classes propres aux bloqueurs, inconnues des moteurs : règle ignorée.
const SELECTEURS_REFUSES: &[&str] = &[
    ":-abp-", ":has-text(", ":contains(", ":xpath(", ":style(", ":remove(", ":upward(", ":matches-",
    ":min-text-length(", ":watch-attr(", ":others(", ":matches-path(", ":remove-attr(", ":remove-class(",
];

fn valid_domain(d: &str) -> bool {
    d.contains('.')
        && !d.starts_with(['.', '-'])
        && !d.ends_with(['.', '-'])
        && d.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// Domaine d'une règle `||domaine^…` ; `rest` : ce qui suit le domaine.
fn rule_domain(rule: &str) -> Option<(String, &str)> {
    let rest = rule.strip_prefix("||")?;
    let end = rest.find(['^', '/', '$', '*', '|', ':']).unwrap_or(rest.len());
    let domain = rest[..end].to_ascii_lowercase();
    valid_domain(&domain).then(|| (domain, &rest[end..]))
}

/// Premier `#id` ou `.classe` d'un sélecteur, s'il le commence (« .pub > a » → « .pub »).
fn first_token(selector: &str) -> Option<&str> {
    let first = selector.chars().next()?;
    if first != '#' && first != '.' {
        return None;
    }
    let end = selector[1..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .map_or(selector.len(), |i| i + 1);
    // Un caractère échappé (« .a\:b ») ne se compare pas simplement : règle « toujours ».
    (end > 1 && !selector[end..].starts_with('\\')).then(|| &selector[..end])
}

/// Hôtes d'une règle cosmétique (« a.fr,b.com ») ; `None` : règle à ignorer.
fn cosmetic_hosts(prefix: &str) -> Option<Vec<&str>> {
    if !prefix.bytes().all(|b| b.is_ascii_alphanumeric() || b".,~*-_".contains(&b)) {
        return None;
    }
    if prefix.is_empty() {
        return Some(Vec::new());
    }
    // Les sites exclus (« ~a.fr ») et les jokers (« google.* ») ne sont pas gérés :
    // on garde les autres sites de la règle.
    let hosts: Vec<&str> = prefix.split(',').filter(|h| !h.starts_with('~') && !h.contains('*') && !h.is_empty()).collect();
    (!hosts.is_empty()).then_some(hosts)
}

impl Filters {
    /// Lit des listes au format Adblock Plus.
    pub fn parse<'a>(lists: impl IntoIterator<Item = &'a str>) -> Self {
        let mut blocked = Vec::new();
        let mut exceptions: HashMap<Box<str>, Vec<Exception>> = HashMap::new();
        let mut pages_allowed = Vec::new();
        let mut generic_except: HashSet<String> = HashSet::new();
        let mut cosmetic = Cosmetic::default();
        let mut generic_seen: HashSet<Box<str>> = HashSet::new();

        for line in lists.into_iter().flat_map(str::lines) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
                continue;
            }
            // --- Règles cosmétiques : « sites##sélecteur », « sites#@#sélecteur » ---
            if let Some(at) = line.find('#')
                && let Some(hosts) = cosmetic_hosts(&line[..at])
            {
                let rest = &line[at..];
                let (exception, selector) = if let Some(s) = rest.strip_prefix("##") {
                    (false, s)
                } else if let Some(s) = rest.strip_prefix("#@#") {
                    (true, s)
                } else {
                    // « #?# », « #$# », « #%# » : extensions propres aux bloqueurs.
                    continue;
                };
                let selector = selector.trim();
                if selector.is_empty()
                    || selector.starts_with(['+', '^'])
                    || SELECTEURS_REFUSES.iter().any(|p| selector.contains(p))
                {
                    continue;
                }
                let selector: Box<str> = selector.into();
                match (exception, hosts.is_empty()) {
                    (true, true) => {
                        generic_except.insert(selector.into());
                    }
                    (true, false) => {
                        for h in hosts {
                            cosmetic.except.entry(h.to_ascii_lowercase().into()).or_default().push(selector.clone());
                        }
                    }
                    (false, false) => {
                        for h in hosts {
                            cosmetic.by_host.entry(h.to_ascii_lowercase().into()).or_default().push(selector.clone());
                        }
                    }
                    (false, true) => {
                        if !generic_seen.insert(selector.clone()) {
                            continue;
                        }
                        // Un sélecteur à plusieurs parties (« .a, .b ») est envoyé à chaque page.
                        match first_token(&selector).filter(|_| !selector.contains(',')) {
                            Some(token) => cosmetic.generic.entry(token.into()).or_default().push(selector),
                            None => cosmetic.always.push(selector),
                        }
                    }
                }
                continue;
            }
            // --- Règles réseau : seuls les domaines entiers sont gardés ---
            let (exception, rule) = match line.strip_prefix("@@") {
                Some(r) => (true, r),
                None => (false, line),
            };
            let Some((domain, rest)) = rule_domain(rule) else { continue };
            let options: Vec<&str> = rest.split_once('$').map(|(_, o)| o.split(',').collect()).unwrap_or_default();
            let path = rest.split_once('$').map_or(rest, |(p, _)| p);
            if exception {
                // Exception limitée aux encarts : le site garde ses requêtes bloquées.
                if options.iter().any(|o| OPTIONS_COSMETIQUES.contains(o)) {
                    if options.iter().any(|o| ["generichide", "ghide", "elemhide", "ehide"].contains(o)) {
                        cosmetic.no_generic.insert(domain.into());
                    }
                } else if options.contains(&"document") {
                    pages_allowed.push(domain);
                } else {
                    // Les sites exclus (« ~a.fr ») sont ignorés : l'exception vaut
                    // alors un peu plus largement, ce qui ne casse rien.
                    let listed: Vec<&str> = options
                        .iter()
                        .find_map(|o| o.strip_prefix("domain="))
                        .map(|list| list.split('|').filter(|d| !d.starts_with('~') && !d.is_empty()).collect())
                        .unwrap_or_default();
                    // Les sites écrits autrement (jokers, accents) sont écartés : WebKit
                    // refuserait la liste entière. S'il n'en reste aucun, l'exception
                    // ne vaut nulle part (et non partout).
                    let sites: Vec<Box<str>> =
                        listed.iter().map(|d| d.to_ascii_lowercase()).filter(|d| valid_domain(d)).map(Into::into).collect();
                    if sites.is_empty() && !listed.is_empty() {
                        continue;
                    }
                    // Le chemin s'arrête au premier joker ou séparateur.
                    let path = path.split(['*', '^', '|']).next().unwrap_or("");
                    exceptions.entry(domain.into()).or_default().push(Exception { path: path.into(), sites });
                }
                continue;
            }
            if !matches!(path, "^" | "^|" | "") || options.iter().any(|o| !OPTIONS_OK.contains(o)) {
                continue;
            }
            blocked.push(domain);
        }

        for selectors in cosmetic.generic.values_mut() {
            selectors.retain(|s| !generic_except.contains(&**s));
        }
        cosmetic.generic.retain(|_, v| !v.is_empty());
        cosmetic.always.retain(|s| !generic_except.contains(&**s));
        let network = NetworkRules { blocked: DomainSet::from(blocked), exceptions, pages_allowed: DomainSet::from(pages_allowed) };
        Filters { network, cosmetic }
    }
}

// --- État partagé -------------------------------------------------------------

/// Réglage général (paramètres).
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Une extension (uBlock Origin Lite…) bloque déjà les publicités : le bloqueur
/// intégré se met en pause, sauf dans les onglets privés, où les extensions
/// n'agissent pas. Bloquer deux fois chargeait deux jeux de règles dans
/// chaque page.
static REPLACED: AtomicBool = AtomicBool::new(false);
/// Sites où l'utilisateur a coupé le bloqueur (fiche du site).
static SITES_OFF: RwLock<Vec<String>> = RwLock::new(Vec::new());
/// Règles des encarts.
static COSMETIC: RwLock<Option<Arc<Cosmetic>>> = RwLock::new(None);
/// Domaines bloqués (Windows : lus à chaque requête ; macOS : compilés par WebKit).
static NETWORK: RwLock<Option<Arc<NetworkRules>>> = RwLock::new(None);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn set_sites_off(hosts: Vec<String>) {
    if let Ok(mut s) = SITES_OFF.write() {
        *s = hosts;
    }
}

/// Une extension prend le relais du bloqueur (voir `REPLACED`). Vrai si
/// l'état a changé : les pages ouvertes sont alors à reprendre.
pub fn set_replaced(on: bool) -> bool {
    REPLACED.swap(on, Ordering::Relaxed) != on
}

/// Le bloqueur agit sur une page de ce site (`private` : onglet privé).
pub fn active_on(page_host: &str, private: bool) -> bool {
    ENABLED.load(Ordering::Relaxed)
        && (private || !REPLACED.load(Ordering::Relaxed))
        && !page_host.is_empty()
        && SITES_OFF.read().is_ok_and(|s| !s.iter().any(|h| h == page_host))
}

/// Vrai si la requête vers `url` doit être refusée pour une page de `page_host`.
pub fn should_block(url: &str, page_host: &str, private: bool) -> bool {
    if !active_on(page_host, private) {
        return false;
    }
    let Ok(guard) = NETWORK.read() else { return false };
    let Some(rules) = guard.as_ref() else { return false };
    rules.blocks(url, &page_host.to_ascii_lowercase())
}

/// Sélecteurs à masquer sur une page (voir `Cosmetic::selectors`) ; `None` : le
/// bloqueur n'agit pas sur ce site (la page cesse alors d'envoyer ses classes).
pub fn hide(page_host: &str, private: bool, tokens: &[String], first: bool) -> Option<Vec<String>> {
    if !active_on(page_host, private) {
        return None;
    }
    let cosmetic = COSMETIC.read().ok()?.clone()?;
    Some(cosmetic.selectors(page_host, tokens, first).into_iter().map(str::to_string).collect())
}

// --- Fichiers et téléchargement ---------------------------------------------

/// Ce que les paramètres affichent. Envoyé sans règles (`UserEvent::AdFilters`
/// avec `None`), seuls `updating` et `error` comptent : le reste ne change pas.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    /// Domaines bloqués et encarts masqués.
    pub domains: usize,
    pub hidden: usize,
    /// Date de la plus ancienne liste (secondes depuis 1970), `None` : pas encore téléchargées.
    pub updated: Option<u64>,
    pub updating: bool,
    pub error: Option<String>,
}

fn dir() -> Option<PathBuf> {
    storage::path("filtres")
}

fn age(path: &Path) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).unwrap_or_default())
}

/// Télécharge une liste (fichier temporaire, puis renommé s'il ressemble à une liste).
fn download(url: &str, path: &Path) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let mut cmd = crate::network::command("curl");
    // Les redirections aussi restent en https (`--proto` seul ne vaut que pour la première adresse).
    cmd.args(["-sSL", "--fail", "--max-time", "60", "--proto", "=https", "--proto-redir", "=https"])
        // Une liste plus grosse ne serait de toute façon pas lue (voir `read_lists`).
        .args(["--max-filesize", &MAX_FILE.to_string(), "-o"])
        .arg(&tmp)
        .arg(url);
    let out = cmd.output().map_err(|e| format!("curl introuvable : {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let head = std::fs::read(&tmp).map(|b| b.iter().take(200).copied().collect::<Vec<u8>>()).unwrap_or_default();
    if !String::from_utf8_lossy(&head).contains("[Adblock") {
        let _ = std::fs::remove_file(&tmp);
        return Err("fichier reçu inattendu".into());
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn read_lists(dir: &Path) -> Option<(Filters, Option<u64>)> {
    let mut texts = Vec::new();
    let mut oldest: Option<u64> = None;
    for (file, _, _) in LISTES {
        let path = dir.join(file);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.len() > MAX_FILE {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            texts.push(text);
            let secs = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
            oldest = match (oldest, secs) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
    }
    if texts.is_empty() {
        return None;
    }
    Some((Filters::parse(texts.iter().map(String::as_str)), oldest))
}

/// Lit les listes déjà téléchargées, puis télécharge celles qui manquent ou
/// datent de plus d'une semaine (`force` : toutes). En arrière-plan.
pub fn load(proxy: EventLoopProxy<UserEvent>, force: bool) {
    std::thread::spawn(move || {
        let Some(dir) = dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        let send = |filters: Option<Filters>, status: Status| {
            let _ = proxy.send_event(UserEvent::AdFilters(filters.map(Box::new), status));
        };
        // Envoi des règles lues, avec leur décompte et leur date.
        let send_filters = |(filters, updated): (Filters, Option<u64>), updating: bool, error: Option<String>| {
            let status = Status {
                domains: filters.network.blocked.len(),
                hidden: filters.cosmetic.len(),
                updated,
                updating,
                error,
            };
            send(Some(filters), status);
        };
        let stale: Vec<_> = LISTES
            .iter()
            .filter(|(file, _, _)| force || age(&dir.join(file)).is_none_or(|a| a > MAX_AGE))
            .collect();
        let mut had_lists = false;
        if !force && let Some(read) = read_lists(&dir) {
            had_lists = true;
            send_filters(read, !stale.is_empty(), None);
        }
        if stale.is_empty() {
            return;
        }
        if force {
            send(None, Status { updating: true, ..Status::default() });
        }
        let errors: Vec<String> = stale
            .iter()
            .filter_map(|(file, name, url)| download(url, &dir.join(file)).err().map(|e| format!("{name} : {e}")))
            .collect();
        let error = (!errors.is_empty()).then(|| errors.join(" ; "));
        // Rien de neuf : les règles déjà en place restent.
        if errors.len() == stale.len() && (had_lists || force) {
            send(None, Status { error, ..Status::default() });
            return;
        }
        match read_lists(&dir) {
            Some(read) => send_filters(read, false, error),
            None => send(None, Status { error, ..Status::default() }),
        }
    });
}

/// Met en place de nouvelles règles (sur le fil principal). Sous macOS, les
/// domaines sont confiés à WebKit puis libérés ; `UserEvent::AdRulesReady`
/// arrive quand les listes compilées peuvent être posées sur les pages.
pub fn install(filters: Filters, proxy: EventLoopProxy<UserEvent>) {
    if let Ok(mut c) = COSMETIC.write() {
        *c = Some(Arc::new(filters.cosmetic));
    }
    platform::install_network(filters.network, proxy);
}

/// Pose ou retire le blocage réseau d'une page selon son site (macOS ; sous
/// Windows, chaque requête est vérifiée au passage).
pub fn apply(webview: &WebView, page_host: &str, private: bool) {
    platform::apply(webview, active_on(page_host, private));
}

/// Après `apply` sur toutes les pages : les anciennes listes compilées sont oubliées.
pub fn applied_everywhere() {
    platform::forget_retired();
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::sync::Arc;

    use tao::event_loop::EventLoopProxy;
    use wry::WebView;

    use super::{NETWORK, NetworkRules};
    use crate::UserEvent;

    pub fn install_network(rules: NetworkRules, _proxy: EventLoopProxy<UserEvent>) {
        if let Ok(mut n) = NETWORK.write() {
            *n = Some(Arc::new(rules));
        }
    }

    pub fn apply(_webview: &WebView, _on: bool) {}

    pub fn forget_retired() {}
}

/// Nombre de règles par liste WebKit (le moteur en accepte 150 000 par liste,
/// mais compile bien plus vite des listes moyennes).
const WEBKIT_CHUNK: usize = 40_000;

/// Échappe un texte pour les expressions de WebKit.
fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.*+?()[]{}|^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Règles WebKit (format JSON des « content blockers »), en morceaux de
/// `WEBKIT_CHUNK` domaines : un blocage par domaine (requêtes vers un autre site
/// seulement), puis dans chaque morceau les exceptions (`ignore-previous-rules`
/// ne vaut que pour les règles de sa propre liste).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn webkit_chunks(rules: &NetworkRules) -> Vec<String> {
    use serde_json::{Value, json};
    let host_filter = |d: &str| format!("^[a-z]+://([^/:]*\\.)?{}", regex_escape(d));
    let sites = |list: &[Box<str>]| list.iter().map(|s| format!("*{s}")).collect::<Vec<_>>();
    let mut exceptions: Vec<Value> = Vec::new();
    for (domain, list) in &rules.exceptions {
        for e in list.iter().filter(|e| e.path.is_ascii()) {
            let filter = if e.path.is_empty() {
                format!("{}[:/]", host_filter(domain))
            } else {
                format!("{}(:[0-9]+)?{}", host_filter(domain), regex_escape(&e.path))
            };
            let mut trigger = json!({ "url-filter": filter });
            if !e.sites.is_empty() {
                trigger["if-domain"] = json!(sites(&e.sites));
            }
            exceptions.push(json!({ "trigger": trigger, "action": { "type": "ignore-previous-rules" } }));
        }
    }
    if rules.pages_allowed.len() > 0 {
        let pages: Vec<String> = rules.pages_allowed.iter().map(|s| format!("*{s}")).collect();
        exceptions.push(json!({
            "trigger": { "url-filter": ".*", "if-domain": pages },
            "action": { "type": "ignore-previous-rules" },
        }));
    }
    // Ordre stable : la même liste donne le même texte (et la même empreinte).
    exceptions.sort_by_cached_key(Value::to_string);
    let domains: Vec<&str> = rules.blocked.iter().collect();
    domains
        .chunks(WEBKIT_CHUNK)
        .map(|part| {
            // Écrit à la main : 100 000 règles passées par `serde_json::Value`
            // demanderaient des dizaines de Mo le temps de la conversion. Les domaines
            // ne contiennent que [a-z0-9.-] : rien à échapper en JSON.
            let mut out = String::with_capacity(part.len() * 110);
            out.push('[');
            for d in part {
                let filter = format!("{}[:/]", host_filter(d)).replace('\\', "\\\\");
                out.push_str(&format!(
                    r#"{{"trigger":{{"url-filter":"{filter}","load-type":["third-party"]}},"action":{{"type":"block"}}}},"#
                ));
            }
            for e in &exceptions {
                out.push_str(&e.to_string());
                out.push(',');
            }
            out.pop();
            out.push(']');
            out
        })
        .collect()
}

/// Empreinte d'une liste (FNV-1a) : nomme les listes compilées par WebKit, qui
/// les garde sur le disque ; les mêmes listes ne sont pas recompilées au lancement suivant.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn fingerprint(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::rc::Rc;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_foundation::{NSArray, NSError, NSString};
    use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore};
    use tao::event_loop::EventLoopProxy;
    use wry::{WebView, WebViewExtMacOS};

    use super::{NetworkRules, fingerprint, webkit_chunks};
    use crate::UserEvent;

    const PREFIX: &str = "feather-pub-";

    #[derive(Default)]
    struct State {
        /// Listes en service.
        lists: Vec<Retained<WKContentRuleList>>,
        /// Listes remplacées, encore posées sur des pages.
        retired: Vec<Retained<WKContentRuleList>>,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::default();
    }

    pub fn apply(webview: &WebView, on: bool) {
        let controller = webview.manager();
        STATE.with_borrow(|state| {
            // SAFETY : appelé sur le fil principal, avec des listes valides.
            unsafe {
                for list in state.retired.iter().chain(&state.lists) {
                    controller.removeContentRuleList(list);
                }
                if on {
                    for list in &state.lists {
                        controller.addContentRuleList(list);
                    }
                }
            }
        });
    }

    pub fn forget_retired() {
        STATE.with_borrow_mut(|state| state.retired.clear());
    }

    /// Cherche les listes déjà compilées (même empreinte), compile les autres,
    /// puis efface du disque celles d'anciennes versions.
    pub fn install_network(rules: NetworkRules, proxy: EventLoopProxy<UserEvent>) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        // SAFETY : `defaultStore` est appelé sur le fil principal (vérifié ci-dessus).
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else { return };
        // Les domaines sont libérés dès que le texte des règles est prêt.
        let chunks: Vec<(String, String)> = webkit_chunks(&rules)
            .into_iter()
            .map(|json| (format!("{PREFIX}{:016x}", fingerprint(&json)), json))
            .collect();
        drop(rules);
        let ids: Vec<String> = chunks.iter().map(|(id, _)| id.clone()).collect();
        let total = chunks.len();
        let done: Rc<RefCell<Vec<Option<Retained<WKContentRuleList>>>>> = Rc::new(RefCell::new((0..total).map(|_| None).collect()));
        let left = Rc::new(std::cell::Cell::new(total));

        let finish = {
            let done = Rc::clone(&done);
            let store = store.clone();
            let ids = ids.clone();
            move || {
                let lists: Vec<_> = done.borrow_mut().drain(..).flatten().collect();
                STATE.with_borrow_mut(|state| {
                    let old = std::mem::replace(&mut state.lists, lists);
                    state.retired.extend(old);
                });
                // Anciennes listes gardées par WebKit sur le disque.
                let store_rm = store.clone();
                let ids = ids.clone();
                let clean = RcBlock::new(move |found: *mut NSArray<NSString>| {
                    // SAFETY : WebKit passe un tableau valide (ou nul).
                    let Some(found) = (unsafe { Retained::retain(found) }) else { return };
                    for id in found.iter() {
                        let id = id.to_string();
                        if id.starts_with(PREFIX) && !ids.contains(&id) {
                            let ns = NSString::from_str(&id);
                            let ignore = RcBlock::new(|_: *mut NSError| {});
                            // SAFETY : arguments non nuls.
                            unsafe { store_rm.removeContentRuleListForIdentifier_completionHandler(Some(&ns), Some(&ignore)) };
                        }
                    }
                });
                // SAFETY : rappel conservé par WebKit jusqu'à son appel.
                unsafe { store.getAvailableContentRuleListIdentifiers(Some(&clean)) };
                let _ = proxy.send_event(UserEvent::AdRulesReady);
            }
        };
        let finish = Rc::new(finish);

        for (i, (id, json)) in chunks.into_iter().enumerate() {
            let ns_id = NSString::from_str(&id);
            let done = Rc::clone(&done);
            let left = Rc::clone(&left);
            let finish = Rc::clone(&finish);
            let store_c = store.clone();
            // Rappel commun : la liste trouvée ou compilée prend sa place.
            let place = Rc::new(move |list: Option<Retained<WKContentRuleList>>| {
                done.borrow_mut()[i] = list;
                left.set(left.get() - 1);
                if left.get() == 0 {
                    finish();
                }
            });
            let json = RefCell::new(Some(json));
            let looked = RcBlock::new(move |list: *mut WKContentRuleList, _error: *mut NSError| {
                // SAFETY : WebKit passe une liste valide (ou nulle si elle n'existe pas).
                if let Some(list) = unsafe { Retained::retain(list) } {
                    place(Some(list));
                    return;
                }
                let Some(json) = json.borrow_mut().take() else { return };
                let place = Rc::clone(&place);
                let compiled = RcBlock::new(move |list: *mut WKContentRuleList, error: *mut NSError| {
                    // SAFETY : comme ci-dessus.
                    let list = unsafe { Retained::retain(list) };
                    if list.is_none() {
                        // SAFETY : erreur valide quand la liste est nulle.
                        let error = unsafe { Retained::retain(error) };
                        eprintln!("liste anti-publicités non compilée : {error:?}");
                    }
                    place(list);
                });
                let ns_id = NSString::from_str(&id);
                let rules = NSString::from_str(&json);
                // SAFETY : arguments non nuls, rappel conservé par WebKit jusqu'à son appel.
                unsafe {
                    store_c.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                        Some(&ns_id),
                        Some(&rules),
                        Some(&compiled),
                    );
                }
            });
            // SAFETY : arguments non nuls, rappel conservé par WebKit jusqu'à son appel.
            unsafe { store.lookUpContentRuleListForIdentifier_completionHandler(Some(&ns_id), Some(&looked)) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_qui_prend_le_relais() {
        assert!(active_on("site.fr", false));
        assert!(set_replaced(true));
        assert!(!set_replaced(true));
        assert!(!active_on("site.fr", false));
        // Les extensions n'agissent pas dans les onglets privés : le bloqueur y reste.
        assert!(active_on("site.fr", true));
        assert!(set_replaced(false));
        assert!(active_on("site.fr", false));
    }

    const LISTE: &str = "[Adblock Plus 2.0]
! Title: exemple
||regie.com^
||pub.exemple.fr^$third-party
||cdn.regie.com^$script,image
||popup.com^$popup
||limite.com^$domain=site.fr
||chemin.com/pub/
||Majuscules.NET^
@@||libre.regie.com^$script
@@||ad.regie.com/lecteur/*.js
||regie.net^
@@||cdn.regie.net^$domain=tele.fr|~radio.tele.fr
@@||cdn.regie.net^$domain=google.*
@@||repare.fr^$document
@@||partenaire.com^$generichide
||partenaire.com^
##.pub-banniere
###encart > div
##.a, .b
##[id^=\"google_ads\"]
##div:has-text(Pub)
lemonde.fr,~abonnes.lemonde.fr##.colonne-pub
lemonde.fr#@#.pub-banniere
google.*##.sponsor
#@#.annule
##.annule
example.com#?#div:-abp-has(.ad)
##+js(nowebrtc)
";

    #[test]
    fn domaines() {
        let f = Filters::parse([LISTE]);
        let n = &f.network;
        let b = |url: &str| n.blocks(url, "site.fr");
        assert!(b("https://regie.com/x.js"));
        assert!(b("https://img.regie.com/"));
        assert!(b("https://pub.exemple.fr/a"));
        assert!(b("https://majuscules.net/"));
        assert!(b("https://PARTENAIRE.com/"), "exception cosmétique seulement");
        // Exception sur un sous-domaine : elle l'emporte sur le parent bloqué.
        assert!(!b("https://libre.regie.com/a.js"));
        assert!(!b("https://a.libre.regie.com/"));
        // Exception limitée à un chemin, puis à des sites.
        assert!(!b("https://ad.regie.com/lecteur/v.js"));
        assert!(b("https://ad.regie.com/autre.js"));
        assert!(!n.blocks("https://cdn.regie.net/x", "www.tele.fr"));
        assert!(b("https://cdn.regie.net/x"));
        // Rien n'est bloqué sur ce site, ni sur le site de la régie lui-même.
        assert!(!n.blocks("https://regie.com/x.js", "www.repare.fr"));
        assert!(!n.blocks("https://regie.com/x.js", "regie.com"));
        // Règles ignorées : fenêtres, limitées à un site, chemins.
        assert!(!b("https://popup.com/"));
        assert!(!b("https://limite.com/"));
        assert!(!b("https://chemin.com/pub/"));
        assert!(!b("https://exemple.fr/"));
        assert!(!b("https://notregie.com/"));
    }

    #[test]
    fn encarts() {
        let f = Filters::parse([LISTE]);
        let c = &f.cosmetic;
        let tokens = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let first = c.selectors("www.lemonde.fr", &tokens(&[".pub-banniere", "#encart", ".autre"]), true);
        assert_eq!(first, ["#encart > div", ".a, .b", ".colonne-pub", "[id^=\"google_ads\"]"]);
        // Ailleurs, .pub-banniere est masquée ; les règles du site ne sont pas envoyées.
        let later = c.selectors("site.fr", &tokens(&[".pub-banniere", ".annule"]), false);
        assert_eq!(later, [".pub-banniere"]);
        // `$generichide` : pas de règles générales sur ce site.
        assert!(c.selectors("www.partenaire.com", &tokens(&[".pub-banniere"]), true).is_empty());
        // Règles inconnues des moteurs, jokers et scripts : ignorées.
        assert!(!c.by_host.contains_key("google.*"));
        assert!(!c.always.iter().any(|s| s.contains("has-text") || s.starts_with('+')));
        assert!(!c.by_host.contains_key("example.com"));
    }

    #[test]
    fn premier_jeton() {
        assert_eq!(first_token(".pub > a"), Some(".pub"));
        assert_eq!(first_token("#ad_1[style]"), Some("#ad_1"));
        assert_eq!(first_token("div.pub"), None);
        assert_eq!(first_token(".a\\:b"), None);
        assert_eq!(first_token("."), None);
    }

    #[test]
    fn ensemble_trie() {
        let set = DomainSet::from(vec!["b.fr".into(), "a.com".into(), "b.fr".into(), "zz.net".into()]);
        assert_eq!(set.len(), 3);
        assert_eq!(set.iter().collect::<Vec<_>>(), ["a.com", "b.fr", "zz.net"]);
        assert!(set.contains("zz.net") && set.contains("a.com") && !set.contains("c.fr"));
        assert_eq!(suffixes("a.b.fr").collect::<Vec<_>>(), ["a.b.fr", "b.fr", "fr"]);
    }

    #[test]
    fn regles_webkit() {
        let f = Filters::parse([LISTE]);
        let chunks = webkit_chunks(&f.network);
        assert_eq!(chunks.len(), 1);
        let rules: serde_json::Value = serde_json::from_str(&chunks[0]).unwrap();
        let rules = rules.as_array().unwrap();
        assert_eq!(rules[0]["trigger"]["url-filter"], "^[a-z]+://([^/:]*\\.)?cdn\\.regie\\.com[:/]");
        assert_eq!(rules[0]["action"]["type"], "block");
        let filters: Vec<&str> = rules.iter().filter_map(|r| r["trigger"]["url-filter"].as_str()).collect();
        assert!(filters.contains(&"^[a-z]+://([^/:]*\\.)?ad\\.regie\\.com(:[0-9]+)?/lecteur/"));
        let site = rules.iter().find(|r| r["trigger"]["if-domain"] == serde_json::json!(["*tele.fr"])).unwrap();
        assert_eq!(site["action"]["type"], "ignore-previous-rules");
        assert_eq!(chunks, webkit_chunks(&Filters::parse([LISTE]).network));
        assert_ne!(fingerprint(&chunks[0]), fingerprint("[]"));
    }

    /// Les vraies listes, si elles sont là (`FEATHER_LISTES=dossier cargo test`).
    #[test]
    fn vraies_listes() {
        let Some(dir) = std::env::var_os("FEATHER_LISTES") else { return };
        let dir = PathBuf::from(dir);
        let texts: Vec<String> = LISTES.iter().filter_map(|(f, _, _)| std::fs::read_to_string(dir.join(f)).ok()).collect();
        let start = std::time::Instant::now();
        let f = Filters::parse(texts.iter().map(String::as_str));
        eprintln!(
            "{} domaines, {} exceptions, {} encarts ({} toujours), lu en {:?}",
            f.network.blocked.len(),
            f.network.exceptions.values().map(Vec::len).sum::<usize>(),
            f.cosmetic.len(),
            f.cosmetic.always.len(),
            start.elapsed()
        );
        assert!(f.network.blocks("https://securepubads.g.doubleclick.net/tag/js/gpt.js", "lemonde.fr"));
        assert!(!f.network.blocks("https://fr.wikipedia.org/", "lemonde.fr"));
        let chunks = webkit_chunks(&f.network);
        eprintln!("{} listes WebKit, {} Mo", chunks.len(), chunks.iter().map(String::len).sum::<usize>() / 1_000_000);
    }
}
