//! Paramètres de l'utilisateur, enregistrés en JSON dans le dossier de
//! configuration du système (par exemple `%APPDATA%\Navigateur\config` sur Windows).

use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::network;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Prénom affiché sur la page d'accueil (peut rester vide).
    pub name: String,
    pub search_engine: SearchEngine,
    /// Page ouverte au démarrage et dans les nouveaux onglets.
    /// Vide = page d'accueil intégrée (heure et bienvenue).
    pub home_page: String,
    pub theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: String::new(),
            search_engine: SearchEngine::DuckDuckGo,
            home_page: String::new(),
            theme: Theme::Auto,
        }
    }
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
        Self::path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
            .map(Settings::cleaned)
            .unwrap_or_default()
    }

    /// Écrit d'abord un fichier temporaire puis le renomme, pour ne jamais
    /// laisser un fichier à moitié écrit si le programme s'arrête.
    pub fn save(&self) -> io::Result<()> {
        let path =
            Self::path().ok_or_else(|| io::Error::other("dossier de configuration introuvable"))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(&tmp, json)?;
        fs::rename(tmp, path)
    }

    /// Corrige les valeurs reçues de l'interface ou du fichier.
    pub fn cleaned(mut self) -> Self {
        self.name = self.name.trim().chars().take(40).collect();
        self.home_page = network::normalize_home_page(&self.home_page).unwrap_or_default();
        self
    }

    /// Adresse à ouvrir au démarrage et dans un nouvel onglet.
    pub fn start_url(&self) -> String {
        if self.home_page.is_empty() {
            network::HOME_URL.to_string()
        } else {
            self.home_page.clone()
        }
    }

    fn path() -> Option<PathBuf> {
        ProjectDirs::from("fr", "Navigateur", "Navigateur")
            .map(|d| d.config_dir().join("parametres.json"))
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
