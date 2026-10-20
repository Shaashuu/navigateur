//! Mode lecture : le titre, le texte et les images d'un article, sans le reste
//! (menus, publicités, vidéos qui démarrent seules).
//!
//! L'extraction reprend Readability de Mozilla (celle de Firefox, licence
//! Apache 2.0, `assets/vendor/`). Le script n'est injecté qu'au clic, pas dans
//! chaque page. L'article revient à Rust, qui le garde pour l'onglet et l'affiche
//! dans une page Feather toute simple, `navigateur://lecture/` : la page
//! d'origine, souvent lourde, est quittée et sa mémoire rendue.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::network;
use crate::settings::{Settings, Theme};
use crate::tab_manager::TabId;
use crate::ui::{escape_html, fill_template};

const READABILITY: &str = include_str!("../assets/vendor/Readability.js");
const READER_HTML: &str = include_str!("../assets/reader.html");

/// Taille maximale d'un article gardé (texte HTML).
const MAX_CONTENT: usize = 2_000_000;

#[cfg(windows)]
const READER_URL: &str = "http://navigateur.lecture/";
#[cfg(not(windows))]
const READER_URL: &str = "navigateur://lecture/";

/// Ce que Readability rend, envoyé par la page.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Article {
    pub title: String,
    pub byline: Option<String>,
    pub site: Option<String>,
    pub content: String,
    /// Nombre de caractères du texte.
    pub length: usize,
    pub lang: String,
}

/// Le dernier article de chaque onglet, avec l'adresse de sa page d'origine.
/// Partagé avec le protocole `navigateur` de chaque page (voir `tab_manager.rs`).
pub type Store = Arc<RwLock<HashMap<TabId, (String, Article)>>>;

/// Script injecté au clic : extrait l'article d'une copie de la page (la page
/// elle-même n'est pas modifiée) et l'envoie à Feather.
pub fn extraction_script() -> String {
    // `module` local : sinon un site qui définit `window.module` recevrait Readability.
    format!(
        r#"(() => {{
  const module = undefined;
  {READABILITY}
  const post = (m) => window.ipc.postMessage(JSON.stringify(m));
  try {{
    const a = new Readability(document.cloneNode(true), {{ charThreshold: 400 }}).parse();
    if (!a || !a.content) {{ post({{ reader_failed: true }}); return; }}
    post({{ reader: {{ title: a.title || document.title, byline: a.byline, site: a.siteName,
      content: a.content, length: a.length || 0, lang: a.lang || document.documentElement.lang || '' }} }});
  }} catch (e) {{ post({{ reader_failed: true }}); }}
}})();"#
    )
}

/// Adresse de la page de lecture de l'onglet `tab`, pour l'article de `original`.
pub fn reader_url(tab: TabId, original: &str) -> String {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("t", &tab.to_string())
        .append_pair("u", original)
        .finish();
    format!("{READER_URL}?{query}")
}

/// Vrai si l'adresse est une page de lecture Feather.
pub fn is_reader_page(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    match (parsed.scheme(), parsed.host_str()) {
        ("navigateur", Some("lecture")) => true,
        ("http", Some("navigateur.lecture")) => parsed.port().is_none(),
        _ => false,
    }
}

/// L'onglet et l'adresse d'origine d'une page de lecture.
pub fn parse(url: &str) -> Option<(TabId, String)> {
    if !is_reader_page(url) {
        return None;
    }
    let parsed = Url::parse(url).ok()?;
    let mut tab = None;
    let mut original = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "t" => tab = value.parse().ok(),
            "u" => original = Some(value.into_owned()),
            _ => {}
        }
    }
    let original = original.filter(|u| network::is_web(u))?;
    Some((tab?, original))
}

/// Vérifie et range un article reçu d'une page. `None` : rien d'utilisable.
pub fn clean(mut article: Article) -> Option<Article> {
    if article.content.trim().is_empty() || article.content.len() > MAX_CONTENT {
        return None;
    }
    let short = |t: &str, n: usize| t.chars().filter(|c| !c.is_control()).take(n).collect::<String>();
    article.title = short(&article.title, 300);
    article.byline = article.byline.map(|b| short(&b, 200)).filter(|b| !b.trim().is_empty());
    article.site = article.site.map(|s| short(&s, 100)).filter(|s| !s.trim().is_empty());
    article.lang = article.lang.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(12).collect();
    Some(article)
}

/// La page de lecture. `article` : celui de l'onglet s'il correspond à l'adresse
/// demandée ; sinon une page qui renvoie vers l'original.
pub fn render(url: &str, article: Option<&Article>, settings: &Settings) -> String {
    let original = parse(url).map(|(_, u)| u).unwrap_or_default();
    let host = network::host_of(&original).unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let theme = match settings.theme {
        Theme::Auto => "auto",
        Theme::Light => "light",
        Theme::Dark => "dark",
    };
    // Nonce du script de la page : imprévisible pour le contenu du site.
    let mut hash = Sha256::new();
    hash.update(url.as_bytes());
    hash.update(format!("{:?}", std::time::SystemTime::now()).as_bytes());
    let nonce: String = hash.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect();
    let (title, byline, content, lang) = match article {
        Some(a) => {
            let minutes = (a.length / 1100).max(1);
            let mut by = Vec::new();
            if let Some(b) = &a.byline {
                by.push(b.clone());
            }
            by.push(format!("{minutes} min de lecture"));
            (a.title.clone(), by.join(" · "), a.content.clone(), a.lang.clone())
        }
        None => (
            "Article plus disponible".to_string(),
            String::new(),
            format!(
                r#"<p class="missing">Feather ne garde l'article que le temps de la lecture. <a href="{}">Ouvrir la page d'origine</a></p>"#,
                escape_html(&original)
            ),
            String::new(),
        ),
    };
    let site = article.and_then(|a| a.site.clone()).unwrap_or(host);
    fill_template(
        READER_HTML,
        &[
            ("LANG", if lang.is_empty() { "fr".into() } else { escape_html(&lang) }),
            ("THEME", theme.to_string()),
            ("NONCE", nonce),
            ("TITLE", escape_html(&title)),
            ("SITE", escape_html(&site)),
            ("URL", escape_html(&original)),
            ("BYLINE", escape_html(&byline)),
            ("CONTENT", content),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adresse_aller_retour() {
        let url = reader_url(7, "https://www.lemonde.fr/article?a=1&b=2");
        assert!(is_reader_page(&url));
        assert_eq!(parse(&url), Some((7, "https://www.lemonde.fr/article?a=1&b=2".into())));
        assert!(!network::is_web(&url));
        assert!(!network::is_web("http://navigateur.lecture/?t=1&u=https%3A%2F%2Fa.fr%2F"));
        assert_eq!(parse("navigateur://lecture/?t=1&u=javascript%3Aalert(1)"), None);
        assert!(!is_reader_page("http://navigateur.lecture.exemple.fr/"));
    }

    #[test]
    fn page_de_lecture() {
        let a = Article {
            title: "Les <navigateurs> légers".into(),
            content: "<p>Bonjour</p>".into(),
            length: 5000,
            ..Default::default()
        };
        let url = reader_url(1, "https://exemple.fr/a");
        let html = render(&url, Some(&a), &Settings::default());
        assert!(html.contains("Les &lt;navigateurs&gt; légers"));
        assert!(html.contains("<p>Bonjour</p>"));
        assert!(html.contains("4 min de lecture"));
        assert!(html.contains("script-src 'nonce-"));
        assert!(!html.contains("{{"));
        let missing = render(&url, None, &Settings::default());
        assert!(missing.contains("Article plus disponible"));
    }

    #[test]
    fn script_d_extraction() {
        let s = extraction_script();
        assert!(s.contains("function Readability"));
        assert!(s.contains("reader_failed"));
        assert!(clean(Article::default()).is_none());
    }
}
