//! Recherche dans le texte des pages visitées, sans quitter l'ordinateur.
//!
//! À la fin du chargement, le script injecté envoie le texte lisible de la
//! page (voir `assets/page.js`). On en garde le début, rangé dans
//! `contenu.json` à côté de l'historique, et la Bibliothèque peut retrouver une
//! page par ce qu'elle contenait. Les onglets privés ne sont jamais indexés.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::library::now;
use crate::network::is_web;
use crate::storage;

const FICHIER: &str = "contenu.json";
/// Nombre de pages gardées (les plus anciennes sont oubliées).
const MAX_PAGES: usize = 2000;
/// Caractères gardés par page : de quoi retrouver un article sans grossir le fichier.
const MAX_TEXTE: usize = 4000;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Page {
    url: String,
    title: String,
    text: String,
    time: u64,
    /// Titre et texte normalisés (minuscules, sans accents), calculés à la
    /// première recherche seulement : tant qu'on ne cherche rien, ils ne
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
}

impl FullText {
    pub fn load() -> Self {
        let pages: Vec<Page> = storage::read_json(FICHIER).unwrap_or_default();
        Self { pages, changed: false }
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
        // Espaces regroupés : un texte de page en contient beaucoup.
        let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TEXTE).collect();
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

    /// Pages contenant tous les mots cherchés, les plus pertinentes d'abord.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Trouve> {
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
