//! Historique, favoris et dernière session : des fichiers JSON rangés à côté
//! des paramètres. Tout reste sur l'ordinateur de l'utilisateur.

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::storage::{read_json, write_json};

/// Au-delà, les visites les plus anciennes sont oubliées.
const MAX_VISITS: usize = 5000;

/// Secondes écoulées depuis le 1er janvier 1970.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

use crate::network::is_web;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub time: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub added: u64,
}

/// Les onglets ouverts, pour les rouvrir au prochain démarrage.
/// Une suggestion de la barre d'adresse.
#[derive(Debug, Serialize)]
pub struct Suggestion<'a> {
    pub url: &'a str,
    pub title: &'a str,
    pub bookmark: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub urls: Vec<String>,
    pub active: usize,
}

#[derive(Default)]
pub struct Library {
    history: Vec<Visit>,
    bookmarks: Vec<Bookmark>,
    history_changed: bool,
    bookmarks_changed: bool,
}

impl Library {
    pub fn load() -> Self {
        Self {
            history: read_json("historique.json").unwrap_or_default(),
            bookmarks: read_json("favoris.json").unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Enregistre ce qui a changé depuis la dernière fois.
    pub fn save(&mut self) -> io::Result<()> {
        if self.history_changed {
            write_json("historique.json", &self.history, false)?;
            self.history_changed = false;
        }
        if self.bookmarks_changed {
            write_json("favoris.json", &self.bookmarks, false)?;
            self.bookmarks_changed = false;
        }
        Ok(())
    }

    // --- Historique ---

    /// Une page vient de finir de charger. Un rechargement de la même page ne crée
    /// pas de nouvelle ligne : on met juste l'heure à jour.
    pub fn record_visit(&mut self, url: &str, title: &str) {
        if !is_web(url) {
            return;
        }
        match self.history.last_mut() {
            Some(last) if last.url == url => {
                if !title.is_empty() {
                    last.title = title.to_string();
                }
                last.time = now();
            }
            _ => self.history.push(Visit {
                url: url.to_string(),
                title: title.to_string(),
                time: now(),
            }),
        }
        if self.history.len() > MAX_VISITS {
            let extra = self.history.len() - MAX_VISITS;
            self.history.drain(..extra);
        }
        self.history_changed = true;
    }

    /// Un chargement a échoué (page d'erreur) : la visite que la fin du
    /// chargement vient d'ajouter pour cette adresse est retirée.
    pub fn forget_failed_visit(&mut self, url: &str) {
        if self.history.last().is_some_and(|v| v.url == url && now().saturating_sub(v.time) <= 10) {
            self.history.pop();
            self.history_changed = true;
        }
    }

    /// Le titre arrive souvent après la fin du chargement : on complète la visite.
    pub fn set_title(&mut self, url: &str, title: &str) {
        if title.is_empty() {
            return;
        }
        if let Some(visit) = self.history.iter_mut().rev().take(20).find(|v| v.url == url)
            && visit.title != title
        {
            visit.title = title.to_string();
            self.history_changed = true;
        }
    }

    /// Oublie les visites plus vieilles que `days` jours (0 : on garde tout).
    pub fn prune(&mut self, days: u32) {
        if days == 0 {
            return;
        }
        let limit = now().saturating_sub(u64::from(days) * 86_400);
        let before = self.history.len();
        self.history.retain(|v| v.time >= limit);
        if self.history.len() != before {
            self.history_changed = true;
        }
    }

    /// Les visites les plus récentes d'abord, filtrées par le texte cherché.
    pub fn search_history(&self, query: &str, limit: usize) -> Vec<&Visit> {
        let query = query.trim().to_lowercase();
        self.history
            .iter()
            .rev()
            .filter(|v| matches(&query, &v.title, &v.url))
            .take(limit)
            .collect()
    }

    pub fn remove_visits(&mut self, url: &str) {
        self.history.retain(|v| v.url != url);
        self.history_changed = true;
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.history_changed = true;
    }

    // --- Favoris ---

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.bookmarks.iter().any(|b| b.url == url)
    }

    /// Ajoute la page aux favoris, ou l'en retire si elle y est déjà.
    pub fn toggle_bookmark(&mut self, url: &str, title: &str, icon: Option<&str>) {
        if !is_web(url) {
            return;
        }
        if self.is_bookmarked(url) {
            self.remove_bookmark(url);
            return;
        }
        self.bookmarks.push(Bookmark {
            url: url.to_string(),
            title: if title.is_empty() { url.to_string() } else { title.to_string() },
            icon: icon.map(str::to_string),
            added: now(),
        });
        self.bookmarks_changed = true;
    }

    pub fn remove_bookmark(&mut self, url: &str) {
        self.bookmarks.retain(|b| b.url != url);
        self.bookmarks_changed = true;
    }

    /// Les favoris, le plus récent en premier, filtrés par le texte cherché.
    pub fn search_bookmarks(&self, query: &str) -> Vec<&Bookmark> {
        let query = query.trim().to_lowercase();
        self.bookmarks
            .iter()
            .rev()
            .filter(|b| matches(&query, &b.title, &b.url))
            .collect()
    }

    /// Suggestions de la barre d'adresse : favoris d'abord, puis pages de
    /// l'historique (la plus récente de chaque adresse), sans doublon.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<Suggestion<'_>> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut seen = std::collections::HashSet::new();
        let bookmarks = self
            .bookmarks
            .iter()
            .rev()
            .filter(|b| matches(&query, &b.title, &b.url))
            .map(|b| Suggestion { url: &b.url, title: &b.title, bookmark: true });
        let visits = self
            .history
            .iter()
            .rev()
            .filter(|v| matches(&query, &v.title, &v.url))
            .map(|v| Suggestion { url: &v.url, title: &v.title, bookmark: false });
        bookmarks
            .chain(visits)
            .filter(|s| seen.insert(s.url))
            .take(limit)
            .collect()
    }

    /// Dans l'ordre d'ajout, pour la page d'accueil.
    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.bookmarks
    }
}

fn matches(query: &str, title: &str, url: &str) -> bool {
    query.is_empty() || title.to_lowercase().contains(query) || url.to_lowercase().contains(query)
}

pub fn load_session() -> Session {
    read_json("session.json").unwrap_or_default()
}

pub fn save_session(session: &Session) -> io::Result<()> {
    write_json("session.json", session, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechargement_sans_doublon() {
        let mut lib = Library::default();
        lib.record_visit("https://a.fr/", "");
        lib.record_visit("https://a.fr/", "A");
        lib.record_visit("https://b.fr/", "B");
        lib.record_visit("navigateur://accueil/", "Accueil");
        assert_eq!(lib.history.len(), 2);
        assert_eq!(lib.history[0].title, "A");
    }

    #[test]
    fn recherche_et_titre_tardif() {
        let mut lib = Library::default();
        lib.record_visit("https://wikipedia.org/", "");
        lib.record_visit("https://youtube.com/", "YouTube");
        lib.set_title("https://wikipedia.org/", "Wikipédia");
        let found = lib.search_history("wiki", 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Wikipédia");
        assert_eq!(lib.search_history("", 10)[0].url, "https://youtube.com/");
    }

    #[test]
    fn favori_ajoute_puis_retire() {
        let mut lib = Library::default();
        lib.toggle_bookmark("https://a.fr/", "A", None);
        assert!(lib.is_bookmarked("https://a.fr/"));
        lib.toggle_bookmark("https://a.fr/", "A", None);
        assert!(!lib.is_bookmarked("https://a.fr/"));
        lib.toggle_bookmark("navigateur://accueil/", "Accueil", None);
        // Forme Windows de l'accueil : ce n'est pas une page web non plus.
        lib.toggle_bookmark("http://navigateur.accueil/", "Accueil", None);
        assert!(lib.bookmarks().is_empty());
    }

    #[test]
    fn historique_trop_vieux() {
        let mut lib = Library::default();
        lib.history.push(Visit { url: "https://old.fr/".into(), title: String::new(), time: 0 });
        lib.record_visit("https://new.fr/", "");
        lib.prune(30);
        assert_eq!(lib.history.len(), 1);
        lib.prune(0);
        assert_eq!(lib.history.len(), 1);
    }
}
