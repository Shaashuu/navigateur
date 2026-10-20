//! Importer les favoris d'un autre navigateur.
//!
//! - Chrome, Edge et Brave rangent leurs favoris dans un fichier JSON
//!   (`Bookmarks`) que Feather lit directement (serde_json, déjà là) ;
//! - Firefox, Safari et les autres savent tous exporter un fichier HTML
//!   (format « Netscape ») : l'utilisateur le choisit, l'interface le lit et
//!   l'envoie ici. Pas de bibliothèque SQLite ni plist à ajouter.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// Au-delà, un fichier n'est pas lu (un fichier de favoris fait quelques Mo au plus).
const MAX_FILE: u64 = 20 * 1024 * 1024;

/// Un navigateur trouvé sur l'ordinateur, pour la liste de l'interface.
#[derive(Debug, Serialize)]
pub struct Source {
    /// Identifiant renvoyé par l'interface (« chrome », « edge », « brave »).
    pub id: &'static str,
    pub name: &'static str,
    /// Nombre de favoris trouvés.
    pub count: usize,
}

/// Navigateurs de la famille Chrome : identifiant, nom, dossier du profil par
/// défaut (relatif au dossier des données de l'utilisateur).
fn chromium_browsers() -> Vec<(&'static str, &'static str, PathBuf)> {
    let Some(dirs) = directories::BaseDirs::new() else {
        return Vec::new();
    };
    // Windows : %LOCALAPPDATA% ; macOS : ~/Library/Application Support ; Linux : ~/.config.
    let base = if cfg!(windows) { dirs.data_local_dir() } else { dirs.config_dir() };
    let list: [(&'static str, &'static str, &str); 4] = if cfg!(windows) {
        [
            ("chrome", "Google Chrome", "Google/Chrome/User Data/Default"),
            ("edge", "Microsoft Edge", "Microsoft/Edge/User Data/Default"),
            ("brave", "Brave", "BraveSoftware/Brave-Browser/User Data/Default"),
            ("chromium", "Chromium", "Chromium/User Data/Default"),
        ]
    } else if cfg!(target_os = "macos") {
        [
            ("chrome", "Google Chrome", "Google/Chrome/Default"),
            ("edge", "Microsoft Edge", "Microsoft Edge/Default"),
            ("brave", "Brave", "BraveSoftware/Brave-Browser/Default"),
            ("chromium", "Chromium", "Chromium/Default"),
        ]
    } else {
        [
            ("chrome", "Google Chrome", "google-chrome/Default"),
            ("edge", "Microsoft Edge", "microsoft-edge/Default"),
            ("brave", "Brave", "BraveSoftware/Brave-Browser/Default"),
            ("chromium", "Chromium", "chromium/Default"),
        ]
    };
    list.into_iter().map(|(id, name, dir)| (id, name, base.join(dir).join("Bookmarks"))).collect()
}

fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Les navigateurs dont on trouve des favoris.
pub fn sources() -> Vec<Source> {
    chromium_browsers()
        .into_iter()
        .filter_map(|(id, name, path)| {
            let count = parse_chromium(&read_small(&path)?).len();
            (count > 0).then_some(Source { id, name, count })
        })
        .collect()
}

/// Les favoris (adresse, titre) du navigateur `id`.
pub fn from_source(id: &str) -> Vec<(String, String)> {
    chromium_browsers()
        .into_iter()
        .find(|(i, _, _)| *i == id)
        .and_then(|(_, _, path)| read_small(&path))
        .map(|text| parse_chromium(&text))
        .unwrap_or_default()
}

/// Fichier `Bookmarks` de Chrome : des dossiers (`children`) et des liens (`type: url`).
pub fn parse_chromium(text: &str) -> Vec<(String, String)> {
    fn walk(node: &Value, out: &mut Vec<(String, String)>) {
        match node.get("type").and_then(Value::as_str) {
            Some("url") => {
                if let Some(url) = node.get("url").and_then(Value::as_str) {
                    let title = node.get("name").and_then(Value::as_str).unwrap_or("");
                    out.push((url.to_string(), title.to_string()));
                }
            }
            _ => {
                if let Some(children) = node.get("children").and_then(Value::as_array) {
                    for child in children {
                        walk(child, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Ok(root) = serde_json::from_str::<Value>(text)
        && let Some(roots) = root.get("roots").and_then(Value::as_object)
    {
        for node in roots.values() {
            walk(node, &mut out);
        }
    }
    out
}

/// Fichier d'export HTML (Firefox, Safari, Chrome…) : chaque favori est une
/// balise `<A HREF="…">titre</A>`. On ne lit que ces balises.
pub fn parse_html(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<a ").map(|i| at + i) {
        let Some(tag_end) = lower[start..].find('>').map(|i| start + i) else { break };
        let Some(close) = lower[tag_end..].find("</a>").map(|i| tag_end + i) else { break };
        let tag = &text[start..tag_end];
        if let Some(href) = attribute(tag, "href") {
            let title = strip_tags(&text[tag_end + 1..close]);
            out.push((decode_entities(&href), decode_entities(title.trim())));
        }
        at = close + 4;
    }
    out
}

/// Valeur d'un attribut (`href="…"`) dans une balise, sans tenir compte de la casse du nom.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| from + i) {
        let before_ok = i == 0 || lower.as_bytes()[i - 1].is_ascii_whitespace();
        let rest = lower[i + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let offset = tag.len() - rest.len() + 1;
            let value = tag[offset..].trim_start();
            let quote = value.chars().next()?;
            return if quote == '"' || quote == '\'' {
                value[1..].find(quote).map(|end| value[1..1 + end].to_string())
            } else {
                Some(value.split(|c: char| c.is_whitespace() || c == '>').next()?.to_string())
            };
        }
        from = i + name.len();
    }
    None
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favoris_chrome() {
        let json = r#"{"roots":{"bookmark_bar":{"type":"folder","children":[
            {"type":"url","name":"Wikipédia","url":"https://fr.wikipedia.org/"},
            {"type":"folder","name":"Dev","children":[{"type":"url","name":"Rust","url":"https://www.rust-lang.org/"}]}
        ]},"other":{"type":"folder","children":[]}},"version":1}"#;
        let items = parse_chromium(json);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1], ("https://www.rust-lang.org/".into(), "Rust".into()));
        assert!(parse_chromium("pas du json").is_empty());
    }

    #[test]
    fn export_html() {
        let html = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<DL><p>
  <DT><H3>Barre</H3>
  <DL><p>
    <DT><A HREF="https://fr.wikipedia.org/" ADD_DATE="1" ICON="data:x">Wikip&eacute;dia &amp; co</A>
    <DT><a href='https://exemple.fr/?a=1&amp;b=2'>Exemple <b>gras</b></a>
    <DT><A ADD_DATE="2" HREF=https://sans-guillemets.fr/>Sans</A>
  </DL><p>
</DL>"#;
        let items = parse_html(html);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].0, "https://fr.wikipedia.org/");
        assert_eq!(items[0].1, "Wikip&eacute;dia & co");
        assert_eq!(items[1], ("https://exemple.fr/?a=1&b=2".into(), "Exemple gras".into()));
        assert_eq!(items[2].0, "https://sans-guillemets.fr/");
    }
}
