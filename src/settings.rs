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
    /// Bloquer les traqueurs connus (mesure d'audience, régies publicitaires).
    pub block_trackers: bool,
    /// Bloquer les publicités (listes EasyList, voir `adblock.rs`).
    pub block_ads: bool,
    /// Mettre en veille un onglet resté caché ce nombre de minutes (0 : jamais).
    pub sleep_minutes: u32,
    /// Garder le texte des pages visitées pour les retrouver par leur contenu.
    pub index_pages: bool,
    /// Largeur de la barre à gauche ou à droite, en pixels (voir `SIDEBAR_MIN`).
    pub sidebar_width: u32,
    /// Mode économie : veille plus tôt, vidéos cachées en pause, lecture
    /// automatique bloquée, traqueurs bloqués (voir `sleep_after` et `assets/page.js`).
    pub economy: bool,
}

/// En mode économie, un onglet caché passe en veille au plus tard après ce délai.
pub const ECONOMY_SLEEP_MINUTES: u32 = 5;

/// Largeur la plus étroite de la barre latérale : les pastilles seules.
pub const SIDEBAR_MIN: u32 = 56;
/// Largeur la plus grande : les pastilles et le titre des pages.
pub const SIDEBAR_MAX: u32 = 300;

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
            block_trackers: true,
            block_ads: true,
            sleep_minutes: 30,
            index_pages: true,
            sidebar_width: SIDEBAR_MIN,
            economy: false,
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
        self.sleep_minutes = self.sleep_minutes.min(24 * 60);
        self.sidebar_width = self.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        self
    }

    /// Délai de mise en veille d'un onglet caché, en minutes (0 : jamais),
    /// raccourci par le mode économie.
    pub fn sleep_after(&self) -> u32 {
        match (self.economy, self.sleep_minutes) {
            (false, minutes) => minutes,
            (true, 0) => ECONOMY_SLEEP_MINUTES,
            (true, minutes) => minutes.min(ECONOMY_SLEEP_MINUTES),
        }
    }

    /// Les traqueurs sont bloqués : réglage choisi, ou mode économie.
    pub fn blocks_trackers(&self) -> bool {
        self.block_trackers || self.economy
    }

    /// Les publicités sont bloquées : réglage choisi, ou mode économie.
    pub fn blocks_ads(&self) -> bool {
        self.block_ads || self.economy
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
        let narrow = Settings { sidebar_width: 10, ..Default::default() }.cleaned();
        assert_eq!(narrow.sidebar_width, SIDEBAR_MIN);
        let wide = Settings { sidebar_width: 5000, ..Default::default() }.cleaned();
        assert_eq!(wide.sidebar_width, SIDEBAR_MAX);
    }

    #[test]
    fn mode_economie() {
        let normal = Settings { sleep_minutes: 30, block_trackers: false, ..Default::default() };
        assert_eq!(normal.sleep_after(), 30);
        assert!(!normal.blocks_trackers());
        let eco = Settings { economy: true, ..normal.clone() };
        assert_eq!(eco.sleep_after(), ECONOMY_SLEEP_MINUTES);
        assert!(eco.blocks_trackers());
        // « Jamais » devient 5 minutes ; un délai plus court est gardé.
        assert_eq!(Settings { sleep_minutes: 0, ..eco.clone() }.sleep_after(), ECONOMY_SLEEP_MINUTES);
        assert_eq!(Settings { sleep_minutes: 2, ..eco }.sleep_after(), 2);
    }
}
