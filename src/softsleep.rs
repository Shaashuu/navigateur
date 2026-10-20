//! « Veille douce » des onglets cachés, avant la vraie mise en veille.
//!
//! La veille de `tab_manager.rs` détruit la page : elle libère tout, mais la
//! page est rechargée au retour (position et formulaires perdus). Sous
//! Windows, WebView2 propose deux paliers intermédiaires :
//!
//! 1. après [`ALLEGER_APRES`], `MemoryUsageTargetLevel = Low` : le moteur rend
//!    au système ce qu'il peut (caches, mémoire du JavaScript) ;
//! 2. après [`GELER_APRES`], `TrySuspend` : la page est gelée (plus de
//!    minuteurs ni de scripts) et sa mémoire peut être mise de côté.
//!
//! Dans les deux cas la page reste là et revient instantanément. Ailleurs, ces
//! fonctions ne font rien : WebKit ralentit déjà seul les pages cachées.

use std::time::Duration;

use wry::WebView;

/// Un onglet caché depuis ce délai demande au moteur de libérer sa mémoire.
pub const ALLEGER_APRES: Duration = Duration::from_secs(2 * 60);
/// Un onglet caché depuis ce délai est gelé.
pub const GELER_APRES: Duration = Duration::from_secs(10 * 60);

/// Où en est la veille douce d'un onglet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Repos {
    #[default]
    Eveille,
    Allege,
    Gele,
}

/// Palier visé pour un onglet caché depuis `cache_depuis`.
pub fn palier(cache_depuis: Duration) -> Repos {
    if cache_depuis >= GELER_APRES {
        Repos::Gele
    } else if cache_depuis >= ALLEGER_APRES {
        Repos::Allege
    } else {
        Repos::Eveille
    }
}

/// Fait passer la page au palier `vise` (seulement vers le haut). Renvoie le
/// palier atteint : sans WebView2, la page reste éveillée.
pub fn reposer(webview: &WebView, actuel: Repos, vise: Repos) -> Repos {
    if vise <= actuel {
        return actuel;
    }
    imp::reposer(webview, actuel, vise)
}

/// Rend à la page sa mémoire normale et la dégèle (au retour sur l'onglet).
pub fn reveiller(webview: &WebView, actuel: Repos) {
    if actuel != Repos::Eveille {
        imp::reveiller(webview);
    }
}

#[cfg(windows)]
mod imp {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::TrySuspendCompletedHandler;
    use windows::core::Interface;
    use wry::{WebView, WebViewExtWindows};

    use super::Repos;

    pub fn reposer(webview: &WebView, actuel: Repos, vise: Repos) -> Repos {
        let core = webview.webview();
        let mut atteint = actuel;
        // SAFETY : appels COM sur le fil de la webview, objets valides.
        unsafe {
            if actuel < Repos::Allege
                && let Ok(core19) = core.cast::<ICoreWebView2_19>()
                && core19.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW).is_ok()
            {
                atteint = Repos::Allege;
            }
            if vise == Repos::Gele && let Ok(core3) = core.cast::<ICoreWebView2_3>() {
                // Le moteur peut refuser (page qui joue du son, appel en cours) :
                // on ne réessaie pas, la vraie veille prendra le relais.
                let fini = TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(())));
                if core3.TrySuspend(&fini).is_ok() {
                    atteint = Repos::Gele;
                }
            }
        }
        atteint
    }

    pub fn reveiller(webview: &WebView) {
        let core = webview.webview();
        // SAFETY : appels COM sur le fil de la webview, objets valides.
        unsafe {
            if let Ok(core3) = core.cast::<ICoreWebView2_3>() {
                let _ = core3.Resume();
            }
            if let Ok(core19) = core.cast::<ICoreWebView2_19>() {
                let _ = core19.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use wry::WebView;

    use super::Repos;

    pub fn reposer(_webview: &WebView, actuel: Repos, _vise: Repos) -> Repos {
        actuel
    }

    pub fn reveiller(_webview: &WebView) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paliers() {
        assert_eq!(palier(Duration::from_secs(30)), Repos::Eveille);
        assert_eq!(palier(ALLEGER_APRES), Repos::Allege);
        assert_eq!(palier(Duration::from_secs(9 * 60)), Repos::Allege);
        assert_eq!(palier(GELER_APRES), Repos::Gele);
        assert!(Repos::Eveille < Repos::Allege && Repos::Allege < Repos::Gele);
    }
}
