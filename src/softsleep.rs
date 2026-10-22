//! « Veille douce » des onglets cachés, avant la vraie mise en veille.
//!
//! La veille de `tab_manager.rs` détruit la page : elle libère tout, et la
//! page est rechargée au retour (avec son historique et sa position, voir
//! `tab_manager::Souvenir`). Avant elle, deux paliers gardent la page :
//!
//! 1. après [`ALLEGER_APRES`], la page est allégée :
//!    - Windows : `MemoryUsageTargetLevel = Low`, le moteur rend au système ce
//!      qu'il peut (caches, mémoire du JavaScript) ;
//!    - macOS 12 ou plus : ses médias sont suspendus
//!      (`setAllMediaPlaybackSuspended`), une vidéo muette ne tourne plus ;
//! 2. après [`GELER_APRES`], la page est gelée :
//!    - Windows : `TrySuspend`, plus de minuteurs ni de scripts, et sa mémoire
//!      peut être mise de côté ;
//!    - macOS 12 ou plus : la vue est retirée de la fenêtre au lieu d'être
//!      seulement masquée. WebKit la freine alors davantage et rend la mémoire
//!      de son affichage ; elle est remise à sa place au retour.
//!
//! Dans les deux cas la page reste là et revient aussitôt. Sous Linux, ces
//! fonctions ne font rien : WebKitGTK ralentit déjà seul les pages cachées.

use std::time::Duration;

use serde::Serialize;
use wry::WebView;

/// Un onglet caché depuis ce délai demande au moteur de libérer sa mémoire.
pub const ALLEGER_APRES: Duration = Duration::from_secs(2 * 60);
/// Un onglet caché depuis ce délai est gelé.
pub const GELER_APRES: Duration = Duration::from_secs(10 * 60);

/// Palier le plus haut pour une page qui travaille (son, caméra, formulaire,
/// site « toujours actif », voir `features::keep_awake`). Sous Windows, elle
/// peut rendre de la mémoire sans rien perdre ; sous macOS, l'allègement
/// suspend les médias (un appel en cours se couperait) : on n'y touche pas.
pub const PLAFOND_OCCUPE: Repos = if cfg!(windows) { Repos::Allege } else { Repos::Eveille };

/// Où en est la veille douce d'un onglet (montré dans le gestionnaire des onglets).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
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
        imp::reveiller(webview, actuel);
    }
}

/// La page va être détruite (veille, fermeture) : on oublie où la remettre.
pub fn oublier(webview: &WebView) {
    imp::oublier(webview);
}

/// Vrai (une fois) si une page gelée vient d'être remise dans la fenêtre :
/// elle est alors au-dessus de tout, et l'interface doit repasser devant si un
/// panneau est ouvert (voir `App::wake_active`).
pub fn remise() -> bool {
    imp::remise()
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

    pub fn oublier(_webview: &WebView) {}

    pub fn remise() -> bool {
        false
    }

    pub fn reveiller(webview: &WebView, _actuel: Repos) {
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

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::{Cell, RefCell};

    use objc2::rc::Retained;
    use objc2_app_kit::NSView;
    use wry::{WebView, WebViewExtMacOS};

    use super::Repos;

    thread_local! {
        /// Pages gelées retirées de la fenêtre, avec la vue où les remettre.
        static RETIREES: RefCell<Vec<(usize, Retained<NSView>)>> = const { RefCell::new(Vec::new()) };
        /// Une page vient d'être remise dans la fenêtre (voir `super::remise`).
        static REMISE: Cell<bool> = const { Cell::new(false) };
    }

    /// Clé d'une page : l'adresse de sa vue WebKit.
    fn cle(webview: &WebView) -> usize {
        Retained::as_ptr(&webview.webview()) as usize
    }

    fn retirer_de_la_liste(webview: &WebView) -> Option<Retained<NSView>> {
        let cle = cle(webview);
        RETIREES.with_borrow_mut(|r| r.iter().position(|(k, _)| *k == cle).map(|i| r.swap_remove(i).1))
    }

    pub fn reposer(webview: &WebView, actuel: Repos, vise: Repos) -> Repos {
        if !objc2::available!(macos = 12.0) {
            return actuel;
        }
        let wk = webview.webview();
        let mut atteint = actuel;
        if actuel < Repos::Allege {
            // SAFETY : fil principal, vue vivante ; méthode de macOS 12. Rendu
            // par paire au réveil, comme WebKit le demande.
            unsafe { wk.setAllMediaPlaybackSuspended_completionHandler(true, None) };
            atteint = Repos::Allege;
        }
        if vise == Repos::Gele {
            let view: &NSView = &wk;
            // SAFETY : fil principal, vue vivante.
            if let Some(parent) = unsafe { view.superview() } {
                RETIREES.with_borrow_mut(|r| r.push((cle(webview), parent)));
                // La vue reste gardée par wry : elle n'est pas libérée.
                view.removeFromSuperview();
                atteint = Repos::Gele;
            }
        }
        atteint
    }

    pub fn reveiller(webview: &WebView, actuel: Repos) {
        let wk = webview.webview();
        if let Some(parent) = retirer_de_la_liste(webview) {
            // Remise au-dessus des autres vues ; l'interface repasse devant si
            // un panneau est ouvert (voir `App::wake_active`).
            parent.addSubview(&wk);
            REMISE.set(true);
        }
        if actuel >= Repos::Allege && objc2::available!(macos = 12.0) {
            // SAFETY : fil principal, vue vivante ; méthode de macOS 12.
            unsafe { wk.setAllMediaPlaybackSuspended_completionHandler(false, None) };
        }
    }

    pub fn oublier(webview: &WebView) {
        retirer_de_la_liste(webview);
    }

    pub fn remise() -> bool {
        REMISE.replace(false)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use wry::WebView;

    use super::Repos;

    pub fn reposer(_webview: &WebView, actuel: Repos, _vise: Repos) -> Repos {
        actuel
    }

    pub fn reveiller(_webview: &WebView, _actuel: Repos) {}

    pub fn oublier(_webview: &WebView) {}

    pub fn remise() -> bool {
        false
    }
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
