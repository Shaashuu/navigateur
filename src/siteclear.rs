//! « Effacer les données de ce site » (fiche du site) : cookies, stockage
//! local, caches et bases de données d'un seul site, puis la page est rechargée.
//!
//! - Windows : l'appel `Storage.clearDataForOrigin` du protocole DevTools ;
//! - macOS : les enregistrements de `WKWebsiteDataStore` dont le nom (le
//!   domaine du site) correspond, retirés un par un ;
//! - Linux : pas encore pris en charge (Feather n'y est pas distribué).

use wry::WebView;

/// Vrai si un enregistrement WebKit nommé `name` (« exemple.fr ») concerne `host`
/// (« www.exemple.fr »). Un autre site qui finit pareil (« monexemple.fr ») non.
/// Ne sert que sous macOS ; ailleurs, aux tests.
#[cfg(any(target_os = "macos", test))]
pub fn record_matches(host: &str, name: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    !name.is_empty() && (host == name || host.ends_with(&format!(".{name}")))
}

/// Efface les données du site de la page, puis la recharge. `origin` :
/// « https://www.exemple.fr » ; `host` : « www.exemple.fr ».
pub fn clear(webview: &WebView, origin: &str, host: &str) {
    platform::clear(webview, origin, host);
}

#[cfg(windows)]
mod platform {
    use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
    use windows::core::HSTRING;
    use wry::{WebView, WebViewExtWindows};

    pub fn clear(webview: &WebView, origin: &str, _host: &str) {
        let core = webview.webview();
        let reload = core.clone();
        let params = serde_json::json!({ "origin": origin, "storageTypes": "all" }).to_string();
        // SAFETY : appels COM sur le fil de la webview, objets valides.
        unsafe {
            let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |_, _| {
                let _ = reload.Reload();
                Ok(())
            }));
            let _ = core.CallDevToolsProtocolMethod(
                &HSTRING::from("Storage.clearDataForOrigin"),
                &HSTRING::from(params),
                &done,
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_foundation::NSArray;
    use objc2_web_kit::{WKWebsiteDataRecord, WKWebsiteDataStore};
    use wry::{WebView, WebViewExtMacOS};

    pub fn clear(webview: &WebView, _origin: &str, host: &str) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let wv = webview.webview();
        let host = host.to_string();
        // SAFETY : appelé sur le fil principal (vérifié ci-dessus), objets valides ;
        // WebKit rappelle les blocs sur le fil principal.
        unsafe {
            // Le magasin de la page : celui de tous les onglets, ou celui d'un onglet privé.
            let store = wv.configuration().websiteDataStore();
            let types = WKWebsiteDataStore::allWebsiteDataTypes(mtm);
            let (remove_from, remove_types) = (store.clone(), types.clone());
            let found = RcBlock::new(move |records: NonNull<NSArray<WKWebsiteDataRecord>>| {
                let records = records.as_ref();
                let matching: Vec<Retained<WKWebsiteDataRecord>> = records
                    .iter()
                    .filter(|r| super::record_matches(&host, &r.displayName().to_string()))
                    .collect();
                let page = wv.clone();
                let done = RcBlock::new(move || {
                    let _ = page.reload();
                });
                if matching.is_empty() {
                    done.call(());
                    return;
                }
                let list = NSArray::from_retained_slice(&matching);
                remove_from.removeDataOfTypes_forDataRecords_completionHandler(&remove_types, &list, &done);
            });
            store.fetchDataRecordsOfTypes_completionHandler(&types, &found);
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use wry::WebView;

    pub fn clear(webview: &WebView, _origin: &str, _host: &str) {
        let _ = webview.reload();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domaines() {
        assert!(record_matches("www.exemple.fr", "exemple.fr"));
        assert!(record_matches("exemple.fr", "exemple.fr"));
        assert!(record_matches("Exemple.FR.", "exemple.fr"));
        assert!(!record_matches("monexemple.fr", "exemple.fr"));
        assert!(!record_matches("exemple.fr", ""));
    }
}
