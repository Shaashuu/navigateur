//! Suggestions du moteur de recherche sous la barre d'adresse.
//!
//! Pendant la frappe, ce qui est tapé est envoyé au service de suggestions du
//! moteur choisi (le même que pour la recherche), par `curl` comme les mises à
//! jour : aucune bibliothèque réseau en plus, et rien ne reste en mémoire une
//! fois la réponse arrivée. Une seule demande part à la fois : si l'on tape
//! pendant qu'elle attend, seul le dernier texte est demandé ensuite.
//! Jamais dans un onglet privé, ni pour une adresse complète (voir `worth_asking`).

use std::sync::Mutex;

use serde_json::Value;
use tao::event_loop::EventLoopProxy;

use crate::settings::SearchEngine;
use crate::UserEvent;

/// Nombre de suggestions du moteur gardées.
const MAX: usize = 5;
/// Au-delà, le texte n'est pas envoyé (une phrase collée n'a pas besoin de suggestions).
const MAX_QUERY: usize = 80;
/// Une réponse de suggestions tient en quelques kilo-octets.
const MAX_BODY: &str = "65536";

/// Demande en cours, et la dernière tapée pendant qu'elle attendait.
struct Attente {
    busy: bool,
    pending: Option<(SearchEngine, String)>,
}

static ATTENTE: Mutex<Attente> = Mutex::new(Attente { busy: false, pending: None });

/// Adresse du service de suggestions du moteur (format OpenSearch, en français).
fn endpoint(engine: SearchEngine, query: &str) -> String {
    let q: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    match engine {
        SearchEngine::DuckDuckGo => format!("https://duckduckgo.com/ac/?q={q}&type=list&kl=fr-fr"),
        SearchEngine::Qwant => format!("https://api.qwant.com/api/suggest/?q={q}&client=opensearch&locale=fr_FR"),
        SearchEngine::Ecosia => format!("https://ac.ecosia.org/autocomplete?q={q}&type=list"),
        SearchEngine::Google => {
            format!("https://suggestqueries.google.com/complete/search?client=firefox&hl=fr&ie=utf-8&oe=utf-8&q={q}")
        }
        SearchEngine::Bing => format!("https://api.bing.com/osjson.aspx?query={q}&mkt=fr-FR"),
    }
}

/// Vrai si ce texte peut partir chez le moteur : pas vide, pas trop long, ni
/// une commande, ni une adresse complète ou locale, ni une adresse e-mail
/// (ce qui est privé ne quitte pas l'ordinateur). Un nom de site comme
/// « lemonde.fr » part : c'est ce que l'on tape pour le retrouver.
pub fn worth_asking(query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() || q.chars().count() > MAX_QUERY || q.starts_with('/') {
        return false;
    }
    let lower = q.to_lowercase();
    if lower.contains("://") || lower.contains('@') || lower.starts_with("localhost") || lower.starts_with("file:") {
        return false;
    }
    // Un chemin après un nom de site (« exemple.fr/compte/… ») reste privé.
    let first = lower.split_whitespace().next().unwrap_or("");
    if !lower.contains(' ') && first.contains('.') && first.contains('/') {
        return false;
    }
    // Adresse IP (réseau local, box…).
    let host = first.split([':', '/']).next().unwrap_or("");
    !(host.parse::<std::net::IpAddr>().is_ok())
}

/// Lit la réponse : `["texte", ["suggestion", …]]` (OpenSearch), ou la forme
/// de Qwant `{"data": {"items": [{"value": …}]}}`. Sans doublon ni le texte tapé.
pub fn parse(body: &[u8], query: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else { return Vec::new() };
    let list: Vec<&Value> = match &value {
        Value::Array(a) => a.get(1).and_then(Value::as_array).map(|l| l.iter().collect()).unwrap_or_default(),
        _ => value["data"]["items"].as_array().map(|l| l.iter().map(|i| &i["value"]).collect()).unwrap_or_default(),
    };
    let typed = query.trim().to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for item in list {
        let Some(text) = item.as_str() else { continue };
        let text: String = text.trim().chars().take(120).collect();
        let lower = text.to_lowercase();
        if text.is_empty() || lower == typed || out.iter().any(|o| o.to_lowercase() == lower) {
            continue;
        }
        out.push(text);
        if out.len() == MAX {
            break;
        }
    }
    out
}

/// Demande les suggestions de `query` ; la réponse arrive en
/// `UserEvent::EngineSuggestions`. Une demande déjà en cours garde la main :
/// ce texte attend son tour (et remplace celui qui attendait).
pub fn request(proxy: &EventLoopProxy<UserEvent>, engine: SearchEngine, query: &str) {
    {
        let mut attente = ATTENTE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if attente.busy {
            attente.pending = Some((engine, query.to_string()));
            return;
        }
        attente.busy = true;
    }
    let proxy = proxy.clone();
    let mut next = Some((engine, query.to_string()));
    std::thread::spawn(move || {
        while let Some((engine, query)) = next.take() {
            let items = fetch(engine, &query);
            if proxy.send_event(UserEvent::EngineSuggestions { query, items }).is_err() {
                break;
            }
            let mut attente = ATTENTE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            next = attente.pending.take();
        }
        ATTENTE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).busy = false;
    });
}

/// Une demande, courte : au bout de 3 secondes, tant pis pour les suggestions.
fn fetch(engine: SearchEngine, query: &str) -> Vec<String> {
    let mut cmd = crate::network::command("curl");
    cmd.args([
        "-sS",
        "--fail",
        "--max-time",
        "3",
        "--max-filesize",
        MAX_BODY,
        "--proto",
        "=https",
        "--no-location",
        "-H",
        "Accept: application/json",
    ])
    .arg(endpoint(engine, query));
    match cmd.output() {
        Ok(out) if out.status.success() => parse(&out.stdout, query),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opensearch() {
        let body = br#"["lemon",["lemonde","Lemonde.fr","le monde","lemonde","lemon"]]"#;
        assert_eq!(parse(body, "Lemon"), vec!["lemonde", "Lemonde.fr", "le monde"]);
    }

    #[test]
    fn qwant() {
        let body = br#"{"status":"success","data":{"items":[{"value":"meteo paris","suggestType":3},{"value":"meteo lyon"}]}}"#;
        assert_eq!(parse(body, "meteo"), vec!["meteo paris", "meteo lyon"]);
    }

    #[test]
    fn mauvaise_reponse() {
        assert!(parse(b"<html>", "a").is_empty());
        assert!(parse(br#"["a"]"#, "a").is_empty());
        assert!(parse(br#"["a",[1,null]]"#, "a").is_empty());
    }

    #[test]
    fn au_plus_cinq() {
        let body = br#"["a",["a1","a2","a3","a4","a5","a6","a7"]]"#;
        assert_eq!(parse(body, "a").len(), MAX);
    }

    #[test]
    fn ce_qui_part() {
        for q in ["lemonde", "lemonde.fr", "météo demain", "youtube.com chats", "rust 1.90"] {
            assert!(worth_asking(q), "{q}");
        }
        for q in ["", "   ", "/calcul 2+2", "https://exemple.fr", "moi@exemple.fr", "exemple.fr/compte/42",
            "192.168.1.1", "localhost:8080", "10.0.0.2/admin", "file:///Users/a"] {
            assert!(!worth_asking(q), "{q}");
        }
        assert!(!worth_asking(&"a".repeat(MAX_QUERY + 1)));
    }

    #[test]
    fn adresses() {
        assert_eq!(
            endpoint(SearchEngine::DuckDuckGo, "le monde"),
            "https://duckduckgo.com/ac/?q=le+monde&type=list&kl=fr-fr"
        );
        for engine in [SearchEngine::DuckDuckGo, SearchEngine::Qwant, SearchEngine::Ecosia, SearchEngine::Google, SearchEngine::Bing] {
            assert!(endpoint(engine, "été & co").starts_with("https://"));
            assert!(endpoint(engine, "été & co").contains("%C3%A9t%C3%A9+%26+co"));
        }
    }
}
