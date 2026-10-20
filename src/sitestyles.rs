//! Transparence des pages, site par site, avec les styles de Zen Internet.
//!
//! Le projet my-internet (licence MIT, https://github.com/sameerasw/my-internet)
//! écrit à la main, pour des centaines de sites, la feuille de style qui rend
//! transparents le fond de la page et ses grands blocs, sans toucher aux
//! cartes, champs et menus. C'est la base de l'extension Zen Internet du
//! navigateur Zen. Feather télécharge le fichier qui les réunit (`styles.json`),
//! le garde dans le dossier de configuration et le remet à jour chaque jour,
//! seulement quand la transparence des pages est activée.
//!
//! Pour un site absent de la liste, seul le fond de la page devient
//! transparent (comme Zen Internet).

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::storage;

/// Fichier qui réunit les styles de tous les sites.
const URL: &str = "https://raw.githubusercontent.com/sameerasw/my-internet/main/styles.json";
/// Copie locale, dans le dossier de configuration.
const FILE: &str = "styles-sites.json";
/// On redemande le fichier au plus une fois par jour.
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// Style d'un site inconnu : seul le fond de la page devient transparent.
pub const FALLBACK: &str = "html, body { background-color: transparent !important; background-image: none !important; }";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteStyles {
    /// Nom du fichier (`youtube.com.css`, `+wikipedia.org.css`…) → réglage → CSS.
    website: HashMap<String, BTreeMap<String, String>>,
    /// Nom du fichier → autres sites qui utilisent le même style.
    mapping: HashMap<String, Vec<String>>,
}

impl SiteStyles {
    /// Copie locale, si elle existe.
    pub fn load() -> Option<Self> {
        storage::read_json(FILE)
    }

    pub fn is_empty(&self) -> bool {
        self.website.is_empty()
    }

    /// Le CSS de transparence de ce site, ou `None` s'il n'est pas dans la liste.
    ///
    /// Seuls les réglages de transparence sont repris (pas ceux qui cachent des
    /// parties du site ou changent sa mise en page).
    pub fn css_for(&self, host: &str) -> Option<String> {
        let host = host.trim_start_matches("www.").to_ascii_lowercase();
        let name = self.style_name(&host)?;
        let css: Vec<&str> = self.website.get(name)?
            .iter()
            .filter(|(feature, _)| is_transparency(feature))
            .map(|(_, css)| css.as_str())
            .collect();
        (!css.is_empty()).then(|| css.join("\n"))
    }

    /// Le fichier de style qui vaut pour ce site : le site exact d'abord, puis
    /// un site qui partage son style, puis la règle de tout un domaine.
    fn style_name(&self, host: &str) -> Option<&str> {
        let exact = format!("{host}.css");
        if let Some((name, _)) = self.website.get_key_value(&exact) {
            return Some(name);
        }
        let mut best: Option<(u8, usize, &str)> = None;
        for name in self.website.keys() {
            let stem = name.trim_end_matches(".css");
            if let Some(rank) = matches(stem, host) {
                let better = best.is_none_or(|(r, l, _)| (rank, stem.len()) > (r, l));
                if better {
                    best = Some((rank, stem.len(), name));
                }
            }
        }
        for (name, sites) in &self.mapping {
            if !self.website.contains_key(name) {
                continue;
            }
            for site in sites {
                if let Some(rank) = matches(site, host) {
                    // Un site nommé dans la liste passe avant une règle de domaine.
                    let rank = rank + 1;
                    let better = best.is_none_or(|(r, l, _)| (rank, site.len()) > (r, l));
                    if better {
                        best = Some((rank, site.len(), name));
                    }
                }
            }
        }
        best.map(|(_, _, name)| name)
    }
}

/// Un réglage de transparence (« transparency », « yt-Transparent header »…).
fn is_transparency(feature: &str) -> bool {
    feature.to_ascii_lowercase().contains("transparen")
}

/// Le nom d'un style (sans `.css`) vaut-il pour ce site ? Rend son rang :
/// 4 site exact, 2 sous-domaines (`+domaine`), 1 autres extensions (`-domaine`).
fn matches(stem: &str, host: &str) -> Option<u8> {
    if let Some(domain) = stem.strip_prefix('+') {
        return (host == domain || host.ends_with(&format!(".{domain}"))).then_some(2);
    }
    if let Some(domain) = stem.strip_prefix('-') {
        // `-google.com` vaut aussi pour google.fr, google.co.uk…
        let base = domain.rsplit_once('.').map_or(domain, |(b, _)| b);
        let rest = host.strip_prefix(base)?.strip_prefix('.')?;
        let tld = !rest.is_empty() && rest.split('.').count() <= 2 && rest.split('.').all(|l| !l.is_empty() && l.len() <= 4);
        return tld.then_some(1);
    }
    (stem == host).then_some(4)
}

/// La copie locale manque ou date de plus d'un jour.
pub fn needs_refresh() -> bool {
    let Some(path) = storage::path(FILE) else { return false };
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(time) => SystemTime::now().duration_since(time).map_or(true, |age| age > MAX_AGE),
        Err(_) => true,
    }
}

/// Télécharge le fichier des styles et le garde (à appeler hors du fil principal).
pub fn download() -> Result<SiteStyles, String> {
    let bytes = crate::update::fetch(URL, "le téléchargement des styles")?;
    let styles: SiteStyles = serde_json::from_slice(&bytes).map_err(|_| "fichier de styles illisible".to_string())?;
    if styles.is_empty() {
        return Err("fichier de styles vide".into());
    }
    storage::write_json(FILE, &styles, false).map_err(|e| format!("styles non enregistrés : {e}"))?;
    Ok(styles)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles() -> SiteStyles {
        let json = r#"{
          "website": {
            "youtube.com.css": { "yt-Transparency": "A", "yt-no shorts": "CACHE", "yt-Transparent header": "B" },
            "music.youtube.com.css": { "ytm-transparency": "M" },
            "+wikipedia.org.css": { "transparency": "W" },
            "-google.com.css": { "transparency": "G" },
            "example.com.css": { "transparency": "E" }
          },
          "mapping": { "example.com.css": ["alternativeto.net"] }
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn style_du_site() {
        let s = styles();
        assert_eq!(s.css_for("www.youtube.com").as_deref(), Some("A\nB"));
        assert_eq!(s.css_for("music.youtube.com").as_deref(), Some("M"));
        assert_eq!(s.css_for("fr.wikipedia.org").as_deref(), Some("W"));
        assert_eq!(s.css_for("wikipedia.org").as_deref(), Some("W"));
        assert_eq!(s.css_for("www.google.fr").as_deref(), Some("G"));
        assert_eq!(s.css_for("google.co.uk").as_deref(), Some("G"));
        assert_eq!(s.css_for("alternativeto.net").as_deref(), Some("E"));
        assert_eq!(s.css_for("exemple.fr"), None);
        assert_eq!(s.css_for("notwikipedia.org"), None);
        assert_eq!(s.css_for("google.evil.example.com"), None);
    }
}
