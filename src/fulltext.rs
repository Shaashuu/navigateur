//! Recherche dans le texte des pages visitées, sans quitter l'ordinateur.
//!
//! À la fin du chargement, le script injecté envoie le texte lisible de la
//! page (voir `assets/page.js`). On en garde le début, rangé dans
//! `contenu.json` à côté de l'historique, et la Bibliothèque peut retrouver une
//! page par ce qu'elle contenait. Ne sont jamais gardés : les onglets privés,
//! les pages avec un champ de mot de passe (voir `assets/page.js`), les
//! messageries, banques et sites de santé (voir `excluded`), et les sites que
//! l'utilisateur a exclus dans les paramètres.

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::library::now;
use crate::network::is_web;
use crate::storage;

const FICHIER: &str = "contenu.json";
/// Nombre de pages gardées (les plus anciennes sont oubliées).
const MAX_PAGES: usize = 2000;
/// Caractères gardés par page : de quoi retrouver un article sans grossir le fichier.
const MAX_TEXTE: usize = 4000;
/// Les textes normalisés sont libérés ce délai après la dernière recherche
/// (voir `FullText::release`).
const GARDE_NORMALISES: Duration = Duration::from_secs(60);

/// Sites dont le texte n'est jamais gardé : messageries, documents en ligne,
/// administrations (le site et ses sous-domaines).
const SENSIBLES: &[&str] = &[
    "mail.google.com", "docs.google.com", "drive.google.com", "outlook.live.com", "outlook.office.com",
    "outlook.office365.com", "mail.proton.me", "proton.me", "mail.yahoo.com", "icloud.com", "web.whatsapp.com",
    "messenger.com", "web.telegram.org", "app.slack.com", "discord.com", "teams.microsoft.com", "teams.live.com",
    "onedrive.live.com", "dropbox.com", "notion.so", "1password.com", "bitwarden.com", "lastpass.com",
    "impots.gouv.fr", "ameli.fr", "doctolib.fr", "caf.fr", "service-public.fr", "franceconnect.gouv.fr",
    "laposte.net", "orange.fr", "free.fr", "sfr.fr", "paypal.com",
];
/// Mots qui, dans le nom d'un site, désignent une banque, une assurance ou la santé.
const MOTS_SENSIBLES: &[&str] = &["banque", "bank", "credit", "bourse", "assurance", "mutuelle", "sante", "health", "epargne"];

/// Le texte des pages de ce site ne doit pas être gardé (`user` : sites exclus
/// dans les paramètres).
pub fn excluded(host: &str, user: &[String]) -> bool {
    let host = host.trim_start_matches("www.");
    let under = |site: &str| host == site || host.ends_with(&format!(".{site}"));
    SENSIBLES.iter().any(|s| under(s))
        || user.iter().any(|s| under(s.trim_start_matches("www.")))
        || MOTS_SENSIBLES.iter().any(|m| host.contains(m))
}

/// Nettoie la liste des sites exclus : noms de domaine seuls, en minuscules.
pub fn clean_hosts(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in list {
        let item = item.trim().to_lowercase();
        let item = item.split_once("://").map_or(item.as_str(), |(_, rest)| rest);
        let host = item.split('/').next().unwrap_or("").trim_start_matches("www.").to_string();
        let valid = !host.is_empty() && host.len() <= 253 && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if valid && !out.contains(&host) {
            out.push(host);
        }
    }
    out.truncate(200);
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Page {
    url: String,
    title: String,
    text: String,
    time: u64,
    /// Titre et texte normalisés (minuscules, sans accents), calculés à la
    /// première recherche seulement et libérés une minute après la dernière
    /// (voir `FullText::release`) : tant qu'on ne cherche rien, ils ne
    /// prennent aucune place en mémoire. Rangés en `String` (1 octet par
    /// lettre courante) et non en `Vec<char>` (4 octets par lettre).
    #[serde(skip)]
    norm: OnceLock<String>,
}

impl Page {
    fn new(url: &str, title: &str, text: String) -> Self {
        Self { url: url.into(), title: title.into(), text, time: now(), norm: OnceLock::new() }
    }

    fn norm(&self) -> &str {
        self.norm.get_or_init(|| normaliser(&format!("{}\n{}", self.title, self.text)))
    }
}

/// Une page trouvée, avec l'extrait où apparaît le texte cherché.
#[derive(Debug, Serialize)]
pub struct Trouve {
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub time: u64,
}

#[derive(Default)]
pub struct FullText {
    pages: Vec<Page>,
    changed: bool,
    /// Heure de la dernière recherche (voir `release`).
    searched: Cell<Option<Instant>>,
}

impl FullText {
    pub fn load() -> Self {
        let pages: Vec<Page> = storage::read_json(FICHIER).unwrap_or_default();
        Self { pages, changed: false, searched: Cell::new(None) }
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        if self.changed {
            storage::write_json(FICHIER, &self.pages, false)?;
            self.changed = false;
        }
        Ok(())
    }

    /// Range (ou remplace) le texte d'une page.
    pub fn add(&mut self, url: &str, title: &str, text: &str) {
        if !is_web(url) {
            return;
        }
        // Espaces regroupés : un texte de page en contient beaucoup. On s'arrête
        // à `MAX_TEXTE` caractères sans parcourir le reste (la page peut en envoyer beaucoup).
        let mut kept = String::with_capacity(MAX_TEXTE.min(text.len()));
        let mut count = 0;
        for word in text.split_whitespace() {
            if count > 0 {
                if count == MAX_TEXTE {
                    break;
                }
                kept.push(' ');
                count += 1;
            }
            for c in word.chars() {
                if count == MAX_TEXTE {
                    break;
                }
                kept.push(c);
                count += 1;
            }
        }
        let text = kept;
        if text.is_empty() {
            return;
        }
        self.pages.retain(|p| p.url != url);
        self.pages.push(Page::new(url, title, text));
        if self.pages.len() > MAX_PAGES {
            let trop = self.pages.len() - MAX_PAGES;
            self.pages.drain(..trop);
        }
        self.changed = true;
    }

    /// Le titre arrive parfois après le texte.
    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(p) = self.pages.iter_mut().rev().find(|p| p.url == url)
            && p.title != title
            && !title.is_empty()
        {
            p.title = title.into();
            p.norm = OnceLock::new();
            self.changed = true;
        }
    }

    pub fn remove(&mut self, url: &str) {
        let avant = self.pages.len();
        self.pages.retain(|p| p.url != url);
        self.changed |= self.pages.len() != avant;
    }

    /// Oublie les pages des sites exclus (liste des paramètres changée).
    pub fn forget_excluded(&mut self, user: &[String]) {
        let avant = self.pages.len();
        self.pages.retain(|p| !url::Url::parse(&p.url).ok().and_then(|u| u.host_str().map(|h| excluded(h, user))).unwrap_or(false));
        self.changed |= self.pages.len() != avant;
    }

    pub fn clear(&mut self) {
        self.pages.clear();
        self.changed = true;
    }

    /// Oublie les pages plus vieilles que `days` jours (0 : on garde tout),
    /// comme l'historique.
    pub fn prune(&mut self, days: u32) {
        if days == 0 {
            return;
        }
        let limite = now().saturating_sub(u64::from(days) * 86_400);
        let avant = self.pages.len();
        self.pages.retain(|p| p.time >= limite);
        self.changed |= self.pages.len() != avant;
    }

    /// Libère les textes normalisés une minute après la dernière recherche :
    /// jusqu'à 2 000 pages de 4 000 caractères (8 Mo environ), qui ne servent
    /// qu'à chercher. La recherche suivante les recalcule. Appelé toutes les
    /// cinq secondes (voir `App::survey`).
    pub fn release(&mut self, now: Instant) {
        if self.searched.get().is_some_and(|t| now.saturating_duration_since(t) >= GARDE_NORMALISES) {
            self.searched.set(None);
            for p in &mut self.pages {
                p.norm = OnceLock::new();
            }
        }
    }

    /// Pages contenant tous les mots cherchés, les plus pertinentes d'abord.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Trouve> {
        self.searched.set(Some(Instant::now()));
        let mots: Vec<String> = query
            .split_whitespace()
            .map(normaliser)
            .filter(|m| m.chars().count() >= 2)
            .collect();
        if mots.is_empty() {
            return Vec::new();
        }
        let mut trouves: Vec<(usize, &Page, usize)> = self
            .pages
            .iter()
            .filter_map(|p| {
                let mut score = 0;
                let mut premier = usize::MAX;
                for mot in &mots {
                    let norm = p.norm();
                    let mut positions = norm.match_indices(mot.as_str()).map(|(i, _)| i);
                    let first = positions.next()?;
                    score += 1 + positions.count();
                    // Position en caractères (et non en octets) pour l'extrait.
                    premier = premier.min(norm[..first].chars().count());
                }
                Some((score, p, premier))
            })
            .collect();
        trouves.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.time.cmp(&a.1.time)));
        trouves
            .into_iter()
            .take(limit)
            .map(|(_, p, premier)| Trouve {
                url: p.url.clone(),
                title: p.title.clone(),
                snippet: extrait(p, premier),
                time: p.time,
            })
            .collect()
    }
}

/// Extrait d'environ 160 caractères autour de la position trouvée.
/// `norm` a le même nombre de caractères que « titre\ntexte » (voir `normaliser`).
fn extrait(p: &Page, position: usize) -> String {
    let titre = p.title.chars().count() + 1;
    let texte: Vec<char> = p.text.chars().collect();
    let pos = position.saturating_sub(titre).min(texte.len());
    let debut = pos.saturating_sub(60);
    let fin = (pos + 100).min(texte.len());
    let mut s: String = texte[debut..fin].iter().collect();
    if debut > 0 {
        s.insert(0, '…');
    }
    if fin < texte.len() {
        s.push('…');
    }
    s
}

/// Minuscules et sans accents, un caractère pour un caractère (pour que les
/// positions trouvées restent valables dans le texte d'origine).
fn normaliser(texte: &str) -> String {
    texte
        .chars()
        .map(|c| {
            let c = c.to_lowercase().next().unwrap_or(c);
            match c {
                'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
                'ç' => 'c',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'î' | 'ï' | 'í' | 'ì' => 'i',
                'ô' | 'ö' | 'ó' | 'ò' | 'õ' => 'o',
                'û' | 'ü' | 'ú' | 'ù' => 'u',
                'ÿ' => 'y',
                'œ' => 'o',
                'æ' => 'a',
                c => c,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_exclus() {
        let user = clean_hosts(&["https://www.Intranet.fr/page".into(), " ".into(), "a b".into(), "intranet.fr".into()]);
        assert_eq!(user, ["intranet.fr"]);
        assert!(excluded("mail.google.com", &[]));
        assert!(excluded("www.labanquepostale.fr", &[]));
        assert!(excluded("particuliers.credit-agricole.fr", &[]));
        assert!(excluded("docs.intranet.fr", &user));
        assert!(!excluded("fr.wikipedia.org", &user));
        assert!(!excluded("google.com", &[]));
        let mut f = FullText::default();
        f.add("https://mail.google.com/x", "Boîte", &"courrier ".repeat(10));
        f.add("https://fr.wikipedia.org/wiki/Plume", "Plume", &"oiseau ".repeat(10));
        f.forget_excluded(&[]);
        assert_eq!(f.pages.len(), 1);
    }

    #[test]
    fn retrouver_une_page_par_son_texte() {
        let mut index = FullText::default();
        index.add("https://cuisine.fr/crepes", "Recette", "Les crêpes de la Chandeleur : farine, œufs, lait.");
        index.add("https://velo.fr/", "Vélo", "Réparer une crevaison en dix minutes.");
        index.add("navigateur://accueil/", "Accueil", "crêpes");
        let r = index.search("CREPES lait", 10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "https://cuisine.fr/crepes");
        assert!(r[0].snippet.contains("crêpes de la Chandeleur"), "{}", r[0].snippet);
        assert!(index.search("crêpes vélo", 10).is_empty());
        assert!(index.search("a", 10).is_empty());
        // Une nouvelle visite remplace l'ancien texte.
        index.add("https://velo.fr/", "Vélo", "Changer une chaîne.");
        assert!(index.search("crevaison", 10).is_empty());
        index.remove("https://velo.fr/");
        assert!(index.search("chaine", 10).is_empty());
    }

    #[test]
    fn rien_en_memoire_avant_la_premiere_recherche() {
        let mut index = FullText::default();
        index.add("https://a.fr/", "Été", "Une journée à la plage.");
        assert!(index.pages[0].norm.get().is_none());
        // L'extrait reste juste après des lettres accentuées (positions en caractères).
        let r = index.search("plage", 1);
        assert!(r[0].snippet.contains("à la plage"), "{}", r[0].snippet);
        assert_eq!(index.pages[0].norm.get().map(String::as_str), Some("ete\nune journee a la plage."));
        // Un nouveau titre oublie la version normalisée, recalculée à la recherche suivante.
        index.set_title("https://a.fr/", "Vacances");
        assert!(index.pages[0].norm.get().is_none());
        assert_eq!(index.search("vacances", 1).len(), 1);
        // Libérée une minute après la dernière recherche, pas avant.
        let after = Instant::now();
        index.release(after + Duration::from_secs(30));
        assert!(index.pages[0].norm.get().is_some());
        index.release(after + Duration::from_secs(61));
        assert!(index.pages[0].norm.get().is_none());
        assert_eq!(index.search("plage", 1).len(), 1);
    }

    #[test]
    fn extrait_au_milieu_d_un_long_texte() {
        let mut index = FullText::default();
        let texte = format!("{} aiguille {}", "foin ".repeat(100), "foin ".repeat(100));
        index.add("https://a.fr/", "Titre", &texte);
        let r = index.search("aiguille", 1);
        assert!(r[0].snippet.starts_with('…') && r[0].snippet.ends_with('…'));
        assert!(r[0].snippet.contains("aiguille"));
    }
}
