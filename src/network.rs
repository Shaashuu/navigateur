//! Transformation de ce que l'utilisateur tape en une adresse à charger.
//!
//! La webview gère elle-même tout le trafic réseau des pages : ce module ne
//! fait aucune requête, il décide seulement « adresse ou recherche ? ».

use std::net::IpAddr;
use std::path::PathBuf;

use url::Url;

use crate::errorpage;
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

/// L'hôte d'une adresse (« fr.wikipedia.org »), pour savoir si on change de site.
pub fn host_of(url: &str) -> Option<String> {
    Url::parse(url).ok()?.host_str().map(str::to_string)
}

/// Vrai si cette navigation remplacerait la page de l'interface par un site,
/// un fichier déposé, une page intégrée ou une page d'extension. La page de
/// l'interface elle-même (chargée par `with_html` : `about:blank` ou `data:`
/// selon le système) n'est jamais concernée.
pub fn leaves_chrome(url: &str) -> bool {
    let scheme = url.split_once(':').map_or("", |(s, _)| s).to_ascii_lowercase();
    matches!(
        scheme.as_str(),
        "http" | "https" | "file" | "ftp" | "navigateur" | "chrome-extension" | "webkit-extension"
    )
}

/// Vrai si `frame` (l'adresse du cadre qui envoie un message) a la même origine
/// que `page` (la page affichée par l'onglet) : schéma, hôte et port. Un cadre
/// d'un autre site (publicité, lecteur intégré) n'a pas les droits de la page.
pub fn same_origin(frame: &str, page: &str) -> bool {
    // Comparé à la main : pour `url`, une adresse `navigateur://` (accueil,
    // lecture, erreur sous macOS) n'a pas d'origine comparable.
    let key = |u: &str| {
        let u = Url::parse(u).ok()?;
        let host = u.host_str().filter(|h| !h.is_empty())?.to_ascii_lowercase();
        Some((u.scheme().to_string(), host, u.port_or_known_default()))
    };
    key(frame).is_some_and(|a| key(page) == Some(a))
}

/// Vrai pour une vraie page web (http ou https), seule admise dans l'historique,
/// les favoris et les onglets ouverts par une page. L'accueil intégré et les
/// pages d'erreur ou de lecture n'en sont pas, même sous Windows où leur adresse commence par `http://`.
pub fn is_web(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://")) && !is_home(url) && !errorpage::is_error_page(url) && !crate::reader::is_reader_page(url)
}

/// Adresses données au lancement (`feather https://exemple.fr …`). Seules les
/// vraies pages web sont gardées : macOS ajoute parfois ses propres arguments
/// (`-psn_…`), et une adresse locale (`file://`) ne doit pas s'ouvrir ainsi.
pub fn launch_urls(args: impl IntoIterator<Item = String>) -> Vec<String> {
    args.into_iter()
        .map(|a| a.trim().to_string())
        .filter(|a| is_web(a) && Url::parse(a).is_ok())
        .collect()
}

/// Chemin complet d'un outil du système (`curl`, `explorer`, `tar`…).
///
/// Sous Windows, un nom seul est cherché d'abord dans le dossier de
/// `feather.exe`, avant System32 : un faux `curl.exe` posé à côté de Feather
/// (par un téléchargement, si Feather est lui-même dans Téléchargements) serait
/// lancé à la place du vrai. On donne donc toujours le chemin complet. Sous
/// macOS, ces outils sont tous dans /usr/bin. Sous Linux, leur place change
/// d'une distribution à l'autre : le nom seul suit `PATH`.
pub fn system_program(name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        // L'Explorateur est dans le dossier de Windows, les autres outils dans System32.
        if name == "explorer" {
            windows_dir(false).join("explorer.exe")
        } else {
            windows_dir(true).join(format!("{name}.exe"))
        }
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/usr/bin").join(name)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        PathBuf::from(name)
    }
}

/// Dossier de Windows (`C:\Windows`) ou System32, demandé au système plutôt
/// qu'à une variable d'environnement.
#[cfg(windows)]
fn windows_dir(system32: bool) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetSystemWindowsDirectoryW};
    let mut buffer = [0u16; 260];
    // SAFETY : tampon valide ; la longueur rendue est vérifiée avant usage.
    let len = unsafe {
        if system32 { GetSystemDirectoryW(Some(&mut buffer)) } else { GetSystemWindowsDirectoryW(Some(&mut buffer)) }
    } as usize;
    if len > 0 && len < buffer.len() {
        return PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len]));
    }
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    if system32 { root.join("System32") } else { root }
}

/// Un outil du système lancé en arrière-plan (`curl`…), par son chemin complet
/// (voir `system_program`). Sous Windows, sans la fenêtre de console qui
/// clignoterait à chaque fois.
pub fn command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(system_program(program));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
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

    /// Les outils du système sont toujours lancés par leur chemin complet, et
    /// ce chemin existe (sur les machines de GitHub, comme chez l'utilisateur).
    #[test]
    fn outils_par_chemin_complet() {
        #[cfg(windows)]
        {
            let curl = system_program("curl");
            assert!(curl.is_absolute(), "{curl:?}");
            assert_eq!(curl.file_name().and_then(|n| n.to_str()), Some("curl.exe"));
            let dir = curl.parent().and_then(|d| d.file_name()).and_then(|d| d.to_str()).unwrap_or("");
            assert!(dir.eq_ignore_ascii_case("system32"), "{curl:?}");
            assert!(curl.exists(), "{curl:?}");
            let explorer = system_program("explorer");
            assert!(explorer.is_absolute() && explorer.ends_with("explorer.exe") && explorer.exists(), "{explorer:?}");
        }
        #[cfg(target_os = "macos")]
        for name in ["curl", "ditto", "hdiutil", "xattr", "open"] {
            let path = system_program(name);
            assert_eq!(path, PathBuf::from("/usr/bin").join(name));
            assert!(path.exists(), "{path:?}");
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        assert_eq!(system_program("curl"), PathBuf::from("curl"));
        // Toujours par `command`, qui ne lance jamais un nom seul (sauf Linux).
        let cmd = command("curl");
        assert_eq!(std::path::Path::new(cmd.get_program()), system_program("curl"));
    }

    #[test]
    fn interface_jamais_remplacee() {
        assert!(!leaves_chrome("about:blank"));
        assert!(!leaves_chrome("about:blank#panneau"));
        assert!(!leaves_chrome("data:text/html,<p>x</p>"));
        for url in ["https://exemple.fr/", "HTTP://exemple.fr/", "file:///Users/a/b.html", "navigateur://accueil/", "webkit-extension://abc/options.html"] {
            assert!(leaves_chrome(url), "{url}");
        }
    }

    #[test]
    fn meme_origine() {
        assert!(same_origin("https://www.lemonde.fr/a?b", "https://www.lemonde.fr/"));
        assert!(same_origin("https://WWW.lemonde.fr:443/", "https://www.lemonde.fr/x"));
        assert!(same_origin("navigateur://accueil/", "navigateur://accueil/?q=1"));
        assert!(same_origin("http://navigateur.accueil/", "http://navigateur.accueil/?q=1"));
        // Un cadre d'un autre site, ou du même site sur un autre schéma ou port.
        assert!(!same_origin("https://pub.regie.com/cadre", "https://www.lemonde.fr/"));
        assert!(!same_origin("https://lemonde.fr/", "https://www.lemonde.fr/"));
        assert!(!same_origin("http://www.lemonde.fr/", "https://www.lemonde.fr/"));
        assert!(!same_origin("https://www.lemonde.fr:8443/", "https://www.lemonde.fr/"));
        assert!(!same_origin("navigateur://lecture/", "navigateur://accueil/"));
        // Sans hôte (cadre vide, adresse illisible) : jamais la page.
        assert!(!same_origin("about:blank", "about:blank"));
        assert!(!same_origin("", "https://www.lemonde.fr/"));
    }

    #[test]
    fn adresses_du_lancement() {
        let args = ["-psn_0_12345", "https://fr.wikipedia.org/", "file:///etc/passwd", " http://a.fr ", "mots"];
        assert_eq!(
            launch_urls(args.map(String::from)),
            vec!["https://fr.wikipedia.org/".to_string(), "http://a.fr".to_string()]
        );
    }

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
