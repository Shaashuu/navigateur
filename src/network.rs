//! Transformation de ce que l'utilisateur tape en une adresse à charger.
//!
//! La webview gère elle-même tout le trafic réseau des pages : ce module ne
//! fait aucune requête, il décide seulement « adresse ou recherche ? ».

use std::net::IpAddr;

use url::Url;

use crate::settings::SearchEngine;

/// Adresse de la page d'accueil intégrée (servie par le protocole `navigateur`).
///
/// Sur Windows, WebView2 ne connaît pas les schémas personnalisés : wry les
/// traduit en `http://<schéma>.<chemin>`.
#[cfg(windows)]
pub const HOME_URL: &str = "http://navigateur.accueil/";
#[cfg(not(windows))]
pub const HOME_URL: &str = "navigateur://accueil/";

/// Vrai si l'adresse est la page d'accueil intégrée.
///
/// On compare l'hôte exact : un simple « commence par » acceptait aussi un vrai
/// site comme `http://navigateur.accueil.exemple.fr/`, qui recevait alors les
/// droits de la page d'accueil (barre de recherche, ouverture de la barre).
pub fn is_home(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    match (parsed.scheme(), parsed.host_str()) {
        ("navigateur", Some("accueil")) => true,
        // Forme traduite par wry sous Windows (voir HOME_URL).
        ("http", Some("navigateur.accueil")) => parsed.port().is_none(),
        _ => false,
    }
}

/// Vrai pour une vraie page web (http ou https), seule admise dans l'historique,
/// les favoris et les onglets ouverts par une page. L'accueil intégré n'en est
/// pas une, même sous Windows où son adresse commence par `http://`.
pub fn is_web(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://")) && !is_home(url)
}

/// Transforme la saisie de la barre en adresse : soit l'adresse elle-même,
/// soit une recherche avec le moteur choisi. `None` si la saisie est vide.
pub fn resolve_input(input: &str, engine: SearchEngine) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    if input.eq_ignore_ascii_case("navigateur://accueil") {
        return Some(HOME_URL.to_string());
    }

    // Adresse complète avec un schéma connu : on la garde telle quelle.
    if let Ok(url) = Url::parse(input)
        && matches!(url.scheme(), "http" | "https" | "file")
    {
        return Some(url.to_string());
    }

    if let Some(url) = guess_address(input) {
        return Some(url);
    }

    Some(search_url(engine, input))
}

/// Construit l'adresse d'une recherche.
pub fn search_url(engine: SearchEngine, query: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    format!("{}{}", engine.query_prefix(), encoded)
}

/// Vérifie qu'une adresse de page d'accueil personnalisée est utilisable.
pub fn normalize_home_page(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Ok(url) = Url::parse(input)
        && matches!(url.scheme(), "http" | "https")
    {
        return Some(url.to_string());
    }
    guess_address(input)
}

/// « exemple.fr », « localhost:3000/test », « 192.168.1.1 » ressemblent à des
/// adresses ; « recette crêpes » non.
fn guess_address(input: &str) -> Option<String> {
    if input.chars().any(char::is_whitespace) {
        return None;
    }

    let host_end = input.find(['/', '?', '#']).unwrap_or(input.len());
    let host_and_port = &input[..host_end];
    let host = match host_and_port.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
        Some((_, _)) if !host_and_port.starts_with('[') => return None,
        _ => host_and_port,
    };

    let local = host.eq_ignore_ascii_case("localhost")
        || host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok();
    let domain = !local && looks_like_domain(host);
    if !local && !domain {
        return None;
    }

    let scheme = if local { "http" } else { "https" };
    Url::parse(&format!("{scheme}://{input}"))
        .ok()
        .map(|u| u.to_string())
}

fn looks_like_domain(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    let tld = labels[labels.len() - 1];
    // Extension en lettres (« fr », « рф »), ou sa forme punycode (« xn--p1ai »).
    let punycode = tld.len() > 4
        && tld[..4].eq_ignore_ascii_case("xn--")
        && tld[4..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    punycode || (tld.chars().count() >= 2 && tld.chars().all(char::is_alphabetic))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDG: SearchEngine = SearchEngine::DuckDuckGo;

    #[test]
    fn adresses_completes() {
        assert_eq!(
            resolve_input("https://rust-lang.org", DDG).unwrap(),
            "https://rust-lang.org/"
        );
        assert_eq!(
            resolve_input("  http://exemple.fr/a?b=1 ", DDG).unwrap(),
            "http://exemple.fr/a?b=1"
        );
    }

    #[test]
    fn adresses_devinees() {
        assert_eq!(
            resolve_input("wikipedia.org", DDG).unwrap(),
            "https://wikipedia.org/"
        );
        assert_eq!(
            resolve_input("fr.wikipedia.org/wiki/Rust", DDG).unwrap(),
            "https://fr.wikipedia.org/wiki/Rust"
        );
        assert_eq!(
            resolve_input("localhost:3000", DDG).unwrap(),
            "http://localhost:3000/"
        );
        assert_eq!(
            resolve_input("192.168.1.1", DDG).unwrap(),
            "http://192.168.1.1/"
        );
        // Extension internationale en punycode.
        assert_eq!(
            resolve_input("exemple.xn--p1ai", DDG).unwrap(),
            "https://exemple.xn--p1ai/"
        );
    }

    #[test]
    fn recherches() {
        assert_eq!(
            resolve_input("recette crêpes", DDG).unwrap(),
            "https://duckduckgo.com/?q=recette+cr%C3%AApes"
        );
        assert!(
            resolve_input("rust", DDG)
                .unwrap()
                .starts_with("https://duckduckgo.com/?q=rust")
        );
        assert!(resolve_input("v1.2", DDG).unwrap().contains("?q="));
        assert!(
            resolve_input("javascript:alert(1)", DDG)
                .unwrap()
                .contains("?q=")
        );
        assert_eq!(resolve_input("   ", DDG), None);
    }

    #[test]
    fn accueil() {
        assert!(is_home(HOME_URL));
        assert!(is_home("navigateur://accueil/"));
        assert!(is_home("http://navigateur.accueil/"));
        // Un vrai site dont le nom commence pareil n'est pas l'accueil.
        assert!(!is_home("http://navigateur.accueil.exemple.fr/"));
        assert!(!is_home("http://navigateur.accueil-pirate.fr/"));
        assert!(!is_home("https://exemple.fr/?navigateur://accueil"));
        assert!(!is_web(HOME_URL));
        assert!(is_web("https://exemple.fr/"));
        assert!(!is_web("file:///etc/passwd"));
        assert_eq!(
            resolve_input("navigateur://accueil", DDG).unwrap(),
            HOME_URL
        );
    }

    #[test]
    fn page_accueil_personnalisee() {
        assert_eq!(
            normalize_home_page("exemple.fr").unwrap(),
            "https://exemple.fr/"
        );
        assert_eq!(normalize_home_page("file:///etc/passwd"), None);
        assert_eq!(normalize_home_page("pas une adresse"), None);
    }
}
