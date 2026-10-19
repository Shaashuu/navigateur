//! Paramètres de l'utilisateur, enregistrés en JSON dans le dossier de
//! configuration du système (par exemple `%APPDATA%\Feather\Feather\config` sur Windows).

use std::io;

use serde::{Deserialize, Serialize};

use crate::network;
use crate::storage;

/// Nom du fichier des paramètres dans le dossier de configuration.
const FILE: &str = "parametres.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Prénom affiché sur la page d'accueil (peut rester vide).
    pub name: String,
    pub search_engine: SearchEngine,
    /// Page du bouton maison, ouverte aussi au démarrage.
    /// Vide = page d'accueil intégrée (heure et recherche).
    pub home_page: String,
    pub theme: Theme,
    /// Chercher une nouvelle version à chaque démarrage.
    pub auto_update_check: bool,
    /// Où se trouve la barre des onglets : à gauche, à droite ou en haut.
    pub bar_position: BarPosition,
    /// Au démarrage : la page d'accueil, ou les onglets de la dernière session.
    pub startup: Startup,
    /// Nombre de jours d'historique gardés (0 : tout garder).
    pub history_days: u32,
    /// Les favoris s'affichent sous la recherche de la page d'accueil.
    pub bookmarks_on_home: bool,
    /// Dossier des téléchargements (vide : le dossier Téléchargements du système).
    pub download_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: String::new(),
            search_engine: SearchEngine::DuckDuckGo,
            home_page: String::new(),
            theme: Theme::Auto,
            auto_update_check: true,
            bar_position: BarPosition::Left,
            startup: Startup::Home,
            history_days: 90,
            bookmarks_on_home: true,
            download_dir: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Startup {
    #[default]
    Home,
    Restore,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BarPosition {
    #[default]
    Left,
    Right,
    Top,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
    #[default]
    DuckDuckGo,
    Qwant,
    Ecosia,
    Google,
    Bing,
}

impl SearchEngine {
    pub fn query_prefix(self) -> &'static str {
        match self {
            SearchEngine::DuckDuckGo => "https://duckduckgo.com/?q=",
            SearchEngine::Qwant => "https://www.qwant.com/?q=",
            SearchEngine::Ecosia => "https://www.ecosia.org/search?q=",
            SearchEngine::Google => "https://www.google.com/search?q=",
            SearchEngine::Bing => "https://www.bing.com/search?q=",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

impl Settings {
    /// Lit les paramètres ; en cas d'absence ou d'erreur, on repart des valeurs par défaut.
    pub fn load() -> Self {
        storage::read_json::<Settings>(FILE)
            .map(Settings::cleaned)
            .unwrap_or_default()
    }

    /// Enregistre les paramètres (écriture sûre, voir `storage::write_json`).
    pub fn save(&self) -> io::Result<()> {
        storage::write_json(FILE, self, true)
    }

    /// Corrige les valeurs reçues de l'interface ou du fichier.
    pub fn cleaned(mut self) -> Self {
        self.name = self.name.trim().chars().take(40).collect();
        self.home_page = network::normalize_home_page(&self.home_page).unwrap_or_default();
        self.download_dir = self.download_dir.trim().to_string();
        self
    }

    /// Adresse du bouton maison et du démarrage.
    pub fn start_url(&self) -> String {
        if self.home_page.is_empty() {
            network::HOME_URL.to_string()
        } else {
            self.home_page.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fichier_partiel_complete_par_defaut() {
        let s: Settings =
            serde_json::from_str(r#"{"name":"Shashuu","search_engine":"qwant"}"#).unwrap();
        assert_eq!(s.name, "Shashuu");
        assert_eq!(s.search_engine, SearchEngine::Qwant);
        assert_eq!(s.theme, Theme::Auto);
        assert_eq!(s.start_url(), network::HOME_URL);
    }

    #[test]
    fn nettoyage() {
        let s = Settings {
            name: "  Léa  ".into(),
            home_page: "exemple.fr".into(),
            ..Default::default()
        }
        .cleaned();
        assert_eq!(s.name, "Léa");
        assert_eq!(s.start_url(), "https://exemple.fr/");
    }
}
