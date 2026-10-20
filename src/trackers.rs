//! Blocage des traqueurs : les requêtes vers des domaines connus de mesure
//! d'audience et de publicité sont refusées avant de partir.
//!
//! Chaque système a son propre mécanisme :
//! - macOS : une liste de règles WebKit (`WKContentRuleList`), compilée une fois ;
//! - Windows : l'événement `WebResourceRequested` de WebView2, qui répond 403 ;
//! - Linux : pas encore pris en charge (Feather n'y est pas distribué).
//!
//! Sous Windows, le même événement sert au bloqueur de publicités (`adblock.rs`).

// Le filtrage requête par requête ne sert que sous Windows (WebKit applique
// ses propres règles) ; ailleurs, il n'est utilisé que par les tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::{Arc, RwLock};

use wry::WebView;

/// Domaines bloqués (et tous leurs sous-domaines). Liste courte, choisie pour
/// ne casser aucun site courant : mesure d'audience, régies publicitaires,
/// pixels de suivi.
pub const DOMAINES: &[&str] = &[
    "2mdn.net", "adform.net", "adnxs.com", "adsrvr.org", "adservice.google.com",
    "advertising.com", "amazon-adsystem.com", "amplitude.com", "app-measurement.com",
    "appsflyer.com", "bat.bing.com", "bluekai.com", "branch.io", "casalemedia.com",
    "chartbeat.com", "chartbeat.net", "clarity.ms", "criteo.com", "criteo.net",
    "crwdcntrl.net", "demdex.net", "doubleclick.net", "everesttech.net", "exelator.com",
    "facebook.net", "fullstory.com", "google-analytics.com", "googleadservices.com",
    "googlesyndication.com", "googletagmanager.com", "googletagservices.com", "hotjar.com",
    "hotjar.io", "id5-sync.com", "indexww.com", "krxd.net", "mathtag.com", "mixpanel.com",
    "moatads.com", "mouseflow.com", "openx.net", "outbrain.com", "pubmatic.com",
    "quantserve.com", "rubiconproject.com", "scorecardresearch.com", "segment.com",
    "segment.io", "sentry-cdn.com", "smartadserver.com", "taboola.com", "tapad.com",
    "teads.tv", "tiqcdn.com", "xiti.com", "yieldmo.com", "zemanta.com",
];

/// Vrai si l'hôte est un domaine de la liste ou l'un de ses sous-domaines.
pub fn is_tracker(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    DOMAINES.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Hôte d'une adresse, sans analyser toute l'URL (appelé pour chaque requête).
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host_port = &rest[..end];
    let host_port = host_port.rsplit_once('@').map_or(host_port, |(_, h)| h);
    host_port.split(':').next().unwrap_or("")
}

/// Vrai si la requête vers `url` doit être bloquée pour une page de `page_host`.
/// Un site visité directement (par exemple hotjar.com) n'est jamais bloqué.
pub fn should_block(url: &str, page_host: &str) -> bool {
    let host = host_of(url);
    is_tracker(host) && !is_tracker(page_host)
}

/// Règles WebKit (format JSON des « content blockers ») : une règle par domaine,
/// limitée aux requêtes venant d'un autre site.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn webkit_rules() -> String {
    let rules: Vec<serde_json::Value> = DOMAINES
        .iter()
        .map(|d| {
            let escaped = d.replace('.', "\\.");
            serde_json::json!({
                "trigger": {
                    "url-filter": format!("^[a-z]+://([^/:]*\\.)?{escaped}[:/]"),
                    "load-type": ["third-party"],
                },
                "action": { "type": "block" },
            })
        })
        .collect();
    serde_json::Value::Array(rules).to_string()
}

/// Installe le blocage sur une nouvelle page. `page_host` suit l'adresse de
/// l'onglet (pour ne pas bloquer un site visité directement).
pub fn install(webview: &WebView, page_host: Arc<RwLock<String>>, enabled: bool) {
    platform::install(webview, page_host);
    platform::set_enabled(webview, enabled);
}

/// Active ou coupe le blocage sur une page déjà ouverte.
pub fn set_enabled(webview: &WebView, enabled: bool) {
    platform::set_enabled(webview, enabled);
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::sync::{Arc, RwLock};

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_foundation::{NSError, NSString};
    use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore, WKUserContentController};
    use wry::{WebView, WebViewExtMacOS};

    /// La liste compilée, et les pages qui l'attendent pendant la compilation.
    #[derive(Default)]
    struct State {
        list: Option<Retained<WKContentRuleList>>,
        waiting: Vec<Retained<WKUserContentController>>,
        compiling: bool,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::default();
    }

    // Sous macOS, tout passe par `set_enabled` (WebKit ne bloque que les requêtes
    // venues d'un autre site : l'hôte de la page n'est pas nécessaire).
    pub fn install(_webview: &WebView, _page_host: Arc<RwLock<String>>) {}

    pub fn set_enabled(webview: &WebView, enabled: bool) {
        let controller = webview.manager();
        STATE.with_borrow_mut(|state| {
            state.waiting.retain(|c| !std::ptr::eq(Retained::as_ptr(c), Retained::as_ptr(&controller)));
            match (&state.list, enabled) {
                // SAFETY : appelé sur le fil principal, avec une liste valide.
                (Some(list), true) => unsafe {
                    controller.removeContentRuleList(list);
                    controller.addContentRuleList(list);
                },
                (Some(list), false) => unsafe { controller.removeContentRuleList(list) },
                (None, true) => {
                    state.waiting.push(controller);
                    if !state.compiling {
                        state.compiling = true;
                        compile();
                    }
                }
                (None, false) => {}
            }
        });
    }

    /// Compile les règles une fois (WebKit les garde ensuite en cache sur le disque).
    fn compile() {
        let Some(mtm) = MainThreadMarker::new() else { return };
        // SAFETY : `defaultStore` est appelé sur le fil principal (vérifié ci-dessus).
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
            return;
        };
        let rules = NSString::from_str(&super::webkit_rules());
        let id = NSString::from_str("feather-traqueurs");
        let done = RcBlock::new(|list: *mut WKContentRuleList, error: *mut NSError| {
            // SAFETY : WebKit passe une liste valide (ou nulle en cas d'erreur).
            let list = unsafe { Retained::retain(list) };
            STATE.with_borrow_mut(|state| {
                state.compiling = false;
                match list {
                    Some(list) => {
                        for controller in state.waiting.drain(..) {
                            // SAFETY : rappel reçu sur le fil principal.
                            unsafe { controller.addContentRuleList(&list) };
                        }
                        state.list = Some(list);
                    }
                    None => {
                        state.waiting.clear();
                        if !error.is_null() {
                            eprintln!("règles anti-traqueurs non compilées");
                        }
                    }
                }
            });
        });
        // SAFETY : arguments non nuls, rappel conservé par WebKit jusqu'à son appel.
        unsafe {
            store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                Some(&id),
                Some(&rules),
                Some(&done),
            );
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, RwLock};

    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::{WebResourceRequestedEventHandler, take_pwstr};
    use windows::core::{HSTRING, Interface, PWSTR};
    use wry::{WebView, WebViewExtWindows};

    /// Sous Windows, un seul réglage pour toutes les pages (lu à chaque requête).
    static ENABLED: AtomicBool = AtomicBool::new(true);

    pub fn install(webview: &WebView, page_host: Arc<RwLock<String>>) {
        let core = webview.webview();
        let env = webview.environment();
        let filter = HSTRING::from("*");
        // SAFETY : appels COM sur le fil de la webview, objets valides.
        unsafe {
            let added = match core.cast::<ICoreWebView2_22>() {
                // Les cadres et les « workers » ne passent par l'événement qu'avec cette version.
                Ok(core22) => core22.AddWebResourceRequestedFilterWithRequestSourceKinds(
                    &filter,
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                    COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
                ),
                Err(_) => core.AddWebResourceRequestedFilter(&filter, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL),
            };
            if added.is_err() {
                return;
            }
            let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut uri = PWSTR::null();
                args.Request()?.Uri(&mut uri)?;
                let uri = take_pwstr(uri);
                let page = page_host.read().map(|h| h.clone()).unwrap_or_default();
                // Traqueurs (liste courte de Feather), puis publicités (voir adblock.rs).
                let blocked = (ENABLED.load(Ordering::Relaxed) && super::should_block(&uri, &page))
                    || crate::adblock::should_block(super::host_of(&uri), &page);
                if blocked {
                    let response = env.CreateWebResourceResponse(
                        None,
                        403,
                        &HSTRING::from("Blocked"),
                        &HSTRING::new(),
                    )?;
                    args.SetResponse(&response)?;
                }
                Ok(())
            }));
            let mut token = Default::default();
            let _ = core.add_WebResourceRequested(&handler, &mut token);
        }
    }

    pub fn set_enabled(_webview: &WebView, enabled: bool) {
        ENABLED.store(enabled, Ordering::Relaxed);
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use std::sync::{Arc, RwLock};

    use wry::WebView;

    pub fn install(_webview: &WebView, _page_host: Arc<RwLock<String>>) {}

    pub fn set_enabled(_webview: &WebView, _enabled: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domaines_bloques() {
        assert!(is_tracker("www.google-analytics.com"));
        assert!(is_tracker("doubleclick.net"));
        assert!(is_tracker("stats.g.doubleclick.net."));
        assert!(!is_tracker("google.com"));
        assert!(!is_tracker("notdoubleclick.net"));
        assert!(should_block("https://www.googletagmanager.com/gtm.js?id=1", "lemonde.fr"));
        assert!(should_block("https://user@ad.doubleclick.net:443/x", "a.fr"));
        assert!(!should_block("https://www.lemonde.fr/", "lemonde.fr"));
        // Le site du traqueur visité directement n'est pas bloqué.
        assert!(!should_block("https://www.hotjar.com/app.js", "www.hotjar.com"));
    }

    #[test]
    fn regles_webkit_valides() {
        let rules: serde_json::Value = serde_json::from_str(&webkit_rules()).unwrap();
        assert_eq!(rules.as_array().unwrap().len(), DOMAINES.len());
        assert_eq!(rules[0]["action"]["type"], "block");
    }
}
