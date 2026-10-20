//! Pages d'erreur Feather : une vraie page quand un site ne répond pas (pas de
//! connexion, adresse introuvable, certificat invalide, site trop lent), au lieu
//! d'une page blanche ou de la page brute du moteur, en anglais.
//!
//! La page est servie par le protocole `navigateur`, comme l'accueil :
//! `navigateur://erreur/?k=<cause>&u=<adresse en échec>`. Pour savoir qu'un
//! chargement a échoué :
//! - Windows : l'événement `NavigationCompleted` de WebView2 (`IsSuccess` faux) ;
//! - macOS : wry ne le transmet pas ; on ajoute au délégué de navigation de
//!   WebKit la méthode `webView:didFailProvisionalNavigation:withError:` ;
//! - Linux : rien pour l'instant (Feather n'y est pas distribué).

use url::Url;

use crate::network;
use crate::settings::{Settings, Theme};
use crate::ui::{escape_html, fill_template};

const ERROR_HTML: &str = include_str!("../assets/error.html");

/// Début de l'adresse des pages d'erreur. Sous Windows, wry traduit les
/// protocoles personnalisés en `http://<protocole>.<hôte>` (voir `HOME_URL`).
#[cfg(windows)]
const ERROR_URL: &str = "http://navigateur.erreur/";
#[cfg(not(windows))]
const ERROR_URL: &str = "navigateur://erreur/";

/// Pourquoi la page n'a pas pu s'afficher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// L'ordinateur n'a pas de connexion.
    Offline,
    /// Le nom du site n'existe pas (ou est mal écrit).
    NotFound,
    /// Le serveur refuse la connexion ou ne répond pas.
    Connection,
    /// Le certificat de sécurité du site n'est pas valide.
    Certificate,
    /// Le site met trop de temps à répondre.
    Timeout,
    Other,
}

impl ErrorKind {
    const ALL: [ErrorKind; 6] = [
        ErrorKind::Offline,
        ErrorKind::NotFound,
        ErrorKind::Connection,
        ErrorKind::Certificate,
        ErrorKind::Timeout,
        ErrorKind::Other,
    ];

    /// Nom court, dans l'adresse de la page et dans la classe CSS.
    fn code(self) -> &'static str {
        match self {
            ErrorKind::Offline => "offline",
            ErrorKind::NotFound => "notfound",
            ErrorKind::Connection => "connection",
            ErrorKind::Certificate => "certificate",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Other => "other",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.code() == code)
    }

    /// Titre, explication (avec `{}` pour le site) et conseil.
    fn texts(self) -> (&'static str, &'static str, &'static str) {
        match self {
            ErrorKind::Offline => (
                "Pas de connexion à Internet",
                "Feather n'a pas pu joindre {}.",
                "Vérifiez le Wi-Fi ou le câble. La page se rechargera dès que la connexion revient.",
            ),
            ErrorKind::NotFound => (
                "Adresse introuvable",
                "Aucun site ne répond au nom {}.",
                "L'adresse est peut-être mal écrite. Vous pouvez aussi la chercher sur le web.",
            ),
            ErrorKind::Connection => (
                "Le site ne répond pas",
                "{} refuse la connexion ou est arrêté pour le moment.",
                "Réessayez dans quelques instants.",
            ),
            ErrorKind::Certificate => (
                "Connexion non sûre",
                "Le certificat de sécurité de {} n'est pas valide.",
                "Quelqu'un pourrait lire ou modifier ce que vous envoyez à ce site. Feather a arrêté le chargement.",
            ),
            ErrorKind::Timeout => (
                "Le site met trop de temps à répondre",
                "{} n'a pas répondu à temps.",
                "Le site est peut-être surchargé, ou la connexion très lente.",
            ),
            ErrorKind::Other => (
                "Impossible d'afficher cette page",
                "Le chargement de {} a échoué.",
                "Réessayez ; si le problème continue, le site a peut-être une panne.",
            ),
        }
    }
}

/// Adresse de la page d'erreur pour cette adresse en échec.
pub fn error_url(kind: ErrorKind, failing: &str) -> String {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("k", kind.code())
        .append_pair("u", failing)
        .finish();
    format!("{ERROR_URL}?{query}")
}

/// Vrai si l'adresse est une page d'erreur Feather.
pub fn is_error_page(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    match (parsed.scheme(), parsed.host_str()) {
        ("navigateur", Some("erreur")) => true,
        // Forme traduite par wry sous Windows (voir ERROR_URL).
        ("http", Some("navigateur.erreur")) => parsed.port().is_none(),
        _ => false,
    }
}

/// La cause et l'adresse en échec d'une page d'erreur. L'adresse doit être une
/// vraie page web : une page ne peut pas faire afficher `javascript:` ou `file:`.
pub fn parse(url: &str) -> Option<(ErrorKind, String)> {
    if !is_error_page(url) {
        return None;
    }
    let parsed = Url::parse(url).ok()?;
    let mut kind = None;
    let mut failing = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "k" => kind = ErrorKind::from_code(&value),
            "u" => failing = Some(value.into_owned()),
            _ => {}
        }
    }
    let failing = failing.filter(|u| network::is_web(u) && Url::parse(u).is_ok())?;
    Some((kind.unwrap_or(ErrorKind::Other), failing))
}

/// Le HTML de la page d'erreur (adresse non reconnue : une erreur générale).
pub fn render(url: &str, settings: &Settings) -> String {
    let (kind, failing) = parse(url).unwrap_or((ErrorKind::Other, String::new()));
    let host = network::host_of(&failing).unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let shown = if host.is_empty() { "ce site".to_string() } else { host.clone() };
    let (title, message, advice) = kind.texts();
    let message = escape_html(message).replace("{}", &format!("<b>{}</b>", escape_html(&shown)));
    let search = network::search_url(settings.search_engine, &host);
    let theme = match settings.theme {
        Theme::Auto => "auto",
        Theme::Light => "light",
        Theme::Dark => "dark",
    };
    fill_template(
        ERROR_HTML,
        &[
            ("THEME", theme.to_string()),
            ("KIND", kind.code().to_string()),
            ("URL", escape_html(&failing)),
            ("ICON", icon(kind).to_string()),
            ("TITLE", escape_html(title)),
            ("MESSAGE", message),
            ("ADVICE", escape_html(advice)),
            ("HOST", escape_html(&shown)),
            ("SEARCH", escape_html(&search)),
            (
                "SEARCH_HIDDEN",
                if kind == ErrorKind::NotFound && !host.is_empty() { "" } else { "hidden" }.to_string(),
            ),
            ("CODE", format!("Erreur : {}", kind.code())),
        ],
    )
}

fn icon(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Offline => {
            r#"<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M2 8.5a15 15 0 0 1 20 0M5 12a10 10 0 0 1 14 0M8.5 15.5a5 5 0 0 1 7 0"/><circle cx="12" cy="19" r="1"/><path d="M3 3l18 18"/></svg>"#
        }
        ErrorKind::Certificate => {
            r#"<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 4 2.5 20h19z"/><path d="M12 10v4M12 17v.5"/></svg>"#
        }
        _ => {
            r#"<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="9"/><path d="M12 7.5V13M12 16.5v.5"/></svg>"#
        }
    }
}

pub use platform::{forget, install};

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
    use std::rc::Rc;

    use tao::event_loop::EventLoopProxy;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::{NavigationCompletedEventHandler, NavigationStartingEventHandler, take_pwstr};
    use windows::core::{BOOL, PWSTR};
    use wry::{WebView, WebViewExtWindows};

    use super::ErrorKind;
    use crate::UserEvent;
    use crate::tab_manager::TabId;

    /// Cause d'un échec selon WebView2 ; `None` : pas une erreur à montrer
    /// (chargement annulé, devenu un téléchargement, identifiants demandés).
    fn kind_of(status: COREWEBVIEW2_WEB_ERROR_STATUS) -> Option<ErrorKind> {
        let certificate = [
            COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT,
            COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED,
            COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS,
            COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED,
            COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID,
        ];
        let ignored = [
            COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED,
            COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED,
            COREWEBVIEW2_WEB_ERROR_STATUS_VALID_AUTHENTICATION_CREDENTIALS_REQUIRED,
            COREWEBVIEW2_WEB_ERROR_STATUS_VALID_PROXY_AUTHENTICATION_REQUIRED,
        ];
        Some(if certificate.contains(&status) {
            ErrorKind::Certificate
        } else if ignored.contains(&status) {
            return None;
        } else if status == COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED {
            ErrorKind::Offline
        } else if status == COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED {
            ErrorKind::NotFound
        } else if status == COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT {
            ErrorKind::Timeout
        } else if [
            COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE,
            COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET,
            COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT,
        ]
        .contains(&status)
        {
            ErrorKind::Connection
        } else {
            ErrorKind::Other
        })
    }

    /// Surveille les chargements de la page ; un échec devient `UserEvent::LoadFailed`.
    pub fn install(webview: &WebView, id: TabId, proxy: EventLoopProxy<UserEvent>) {
        let core = webview.webview();
        // Adresse du chargement en cours (celle qui a échoué, même après une redirection).
        let started = Rc::new(RefCell::new(String::new()));
        let on_start = Rc::clone(&started);
        // SAFETY : appels COM sur le fil de la webview, objets valides.
        unsafe {
            // Plus de page d'erreur du moteur (en anglais) : Feather affiche la sienne.
            if let Ok(settings) = core.Settings() {
                let _ = settings.SetIsBuiltInErrorPageEnabled(false);
            }
            let starting = NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    *on_start.borrow_mut() = take_pwstr(uri);
                }
                Ok(())
            }));
            let mut token = Default::default();
            let _ = core.add_NavigationStarting(&starting, &mut token);
            let completed = NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
                let (Some(sender), Some(args)) = (sender, args) else { return Ok(()) };
                let mut success = BOOL::default();
                args.IsSuccess(&mut success)?;
                if success.as_bool() {
                    return Ok(());
                }
                let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                args.WebErrorStatus(&mut status)?;
                let Some(kind) = kind_of(status) else { return Ok(()) };
                let mut url = started.borrow().clone();
                if url.is_empty() {
                    let mut uri = PWSTR::null();
                    sender.Source(&mut uri)?;
                    url = take_pwstr(uri);
                }
                let _ = proxy.send_event(UserEvent::LoadFailed { id, url, kind });
                Ok(())
            }));
            let _ = core.add_NavigationCompleted(&completed, &mut token);
        }
    }

    pub fn forget(_webview: &WebView) {}
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::CStr;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{msg_send, sel};
    use objc2_foundation::NSString;
    use tao::event_loop::EventLoopProxy;
    use wry::{WebView, WebViewExtMacOS};

    use super::ErrorKind;
    use crate::UserEvent;
    use crate::tab_manager::TabId;

    thread_local! {
        /// Onglet de chaque page WebKit (adresse de l'objet → onglet).
        static TABS: RefCell<HashMap<usize, (TabId, EventLoopProxy<UserEvent>)>> = RefCell::default();
    }

    /// Cause d'un échec selon `NSURLErrorDomain` ; `None` : pas une erreur à
    /// montrer (chargement annulé, par exemple en cliquant ailleurs).
    fn kind_of(code: isize) -> Option<ErrorKind> {
        Some(match code {
            -999 => return None,
            -1009 => ErrorKind::Offline,
            -1003 => ErrorKind::NotFound,
            -1004 | -1005 => ErrorKind::Connection,
            -1001 => ErrorKind::Timeout,
            -1206..=-1200 => ErrorKind::Certificate,
            _ => ErrorKind::Other,
        })
    }

    /// `webView:didFailProvisionalNavigation:withError:` ajoutée au délégué de
    /// navigation de wry : appelée quand une page n'a pas pu commencer à charger.
    unsafe extern "C-unwind" fn did_fail(
        _this: *mut AnyObject,
        _cmd: Sel,
        webview: *mut AnyObject,
        _navigation: *mut AnyObject,
        error: *mut AnyObject,
    ) {
        if webview.is_null() || error.is_null() {
            return;
        }
        // SAFETY : WebKit passe une NSError valide pendant l'appel.
        let (code, domain, url) = unsafe {
            let error = &*error;
            let code: isize = msg_send![error, code];
            let domain: Option<Retained<NSString>> = msg_send![error, domain];
            let info: Option<Retained<AnyObject>> = msg_send![error, userInfo];
            let key = NSString::from_str("NSErrorFailingURLStringKey");
            let url = info.and_then(|info| {
                let value: Option<Retained<AnyObject>> = msg_send![&*info, objectForKey: &*key];
                value.and_then(|v| v.downcast::<NSString>().ok())
            });
            (code, domain, url)
        };
        if domain.is_none_or(|d| d.to_string() != "NSURLErrorDomain") {
            return;
        }
        let (Some(kind), Some(url)) = (kind_of(code), url) else { return };
        TABS.with_borrow(|tabs| {
            if let Some((id, proxy)) = tabs.get(&(webview as usize)) {
                let _ = proxy.send_event(UserEvent::LoadFailed { id: *id, url: url.to_string(), kind });
            }
        });
    }

    /// Ajoute la méthode à la classe du délégué (une fois) et retient l'onglet de la page.
    pub fn install(webview: &WebView, id: TabId, proxy: EventLoopProxy<UserEvent>) {
        let wv = webview.webview();
        // SAFETY : `navigationDelegate` existe sur toute WKWebView et rend un objet (ou nil).
        let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![&*wv, navigationDelegate] };
        let Some(delegate) = delegate else { return };
        let class: &AnyClass = delegate.class();
        let name = sel!(webView:didFailProvisionalNavigation:withError:);
        if !class.responds_to(name) {
            // SAFETY : la signature correspond aux types annoncés (« v@:@@@ » : rien
            // rendu ; self, _cmd, puis trois objets) ; la méthode n'existait pas.
            unsafe {
                let imp: Imp = std::mem::transmute(
                    did_fail as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, *mut AnyObject),
                );
                let types: &CStr = c"v@:@@@";
                objc2::ffi::class_addMethod(std::ptr::from_ref(class).cast_mut(), name, imp, types.as_ptr());
            }
        }
        let key = Retained::as_ptr(&wv) as usize;
        TABS.with_borrow_mut(|tabs| tabs.insert(key, (id, proxy)));
    }

    /// La page va être détruite : on oublie son onglet.
    pub fn forget(webview: &WebView) {
        let key = Retained::as_ptr(&webview.webview()) as usize;
        TABS.with_borrow_mut(|tabs| tabs.remove(&key));
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use tao::event_loop::EventLoopProxy;
    use wry::WebView;

    use crate::UserEvent;
    use crate::tab_manager::TabId;

    pub fn install(_webview: &WebView, _id: TabId, _proxy: EventLoopProxy<UserEvent>) {}

    pub fn forget(_webview: &WebView) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adresse_aller_retour() {
        let url = error_url(ErrorKind::NotFound, "https://exemple.fr/a?b=1&c=é");
        assert!(is_error_page(&url));
        assert_eq!(parse(&url), Some((ErrorKind::NotFound, "https://exemple.fr/a?b=1&c=é".into())));
        // Les deux formes de l'adresse sont reconnues.
        assert!(is_error_page("navigateur://erreur/?k=offline&u=https%3A%2F%2Fa.fr%2F"));
        assert!(is_error_page("http://navigateur.erreur/?k=offline"));
        assert!(!is_error_page("http://navigateur.erreur.exemple.fr/"));
        assert!(!is_error_page("http://navigateur.erreur:8080/"));
        // Une page d'erreur n'est pas une page web (ni historique, ni favoris).
        assert!(!network::is_web(&url));
    }

    #[test]
    fn adresse_en_echec_verifiee() {
        let bad = "navigateur://erreur/?k=offline&u=javascript%3Aalert(1)";
        assert_eq!(parse(bad), None);
        assert_eq!(parse("navigateur://erreur/?k=offline&u=file%3A%2F%2F%2Fetc%2Fpasswd"), None);
        // Cause inconnue : erreur générale.
        assert_eq!(
            parse("navigateur://erreur/?k=zzz&u=https%3A%2F%2Fa.fr%2F").map(|p| p.0),
            Some(ErrorKind::Other)
        );
    }

    #[test]
    fn page_echappee() {
        let url = error_url(ErrorKind::NotFound, "https://a.fr/?q=\"><x>");
        let html = render(&url, &Settings::default());
        assert!(!html.contains("\"><x>"));
        assert!(!html.contains("{{"));
        assert!(html.contains("Adresse introuvable"));
        // La recherche n'est proposée que pour une adresse introuvable.
        let html = render(&error_url(ErrorKind::Offline, "https://a.fr/"), &Settings::default());
        assert!(html.contains("Pas de connexion"));
        assert!(html.contains(r#"id="search" href"#) && html.contains(" hidden>"));
    }
}
