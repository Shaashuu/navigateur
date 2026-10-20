//! Effet de verre : le bureau, flouté, se voit à travers la fenêtre.
//!
//! - macOS : une vue `NSVisualEffectView` sous le contenu de la fenêtre ;
//! - Windows 10 (1809+) et 11 : l'effet Acrylic du système ;
//! - Linux : pas pris en charge (dépend trop du gestionnaire de fenêtres).
//!
//! La fenêtre et les webviews sont transparentes ; ce sont les pages HTML
//! (l'interface et, si on le veut, le fond des sites) qui laissent passer le
//! flou, plus ou moins selon le curseur « Transparence ».

use tao::window::Window;

/// Active ou retire le flou derrière la fenêtre. `dark` : thème sombre.
pub fn apply(window: &Window, on: bool, dark: bool) {
    platform::apply(window, on, dark);
}

#[cfg(target_os = "macos")]
mod platform {
    use tao::window::Window;
    use window_vibrancy::{NSVisualEffectMaterial, NSVisualEffectState, apply_vibrancy, clear_vibrancy};

    pub fn apply(window: &Window, on: bool, _dark: bool) {
        // On repart d'une vue neuve : appliquer deux fois empilerait deux flous.
        let _ = clear_vibrancy(window);
        if on {
            // « UnderWindowBackground » suit le thème clair ou sombre du système.
            let result = apply_vibrancy(
                window,
                NSVisualEffectMaterial::UnderWindowBackground,
                Some(NSVisualEffectState::Active),
                None,
            );
            if let Err(err) = result {
                eprintln!("effet de verre impossible : {err}");
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use tao::window::Window;
    use window_vibrancy::{apply_acrylic, clear_acrylic};

    pub fn apply(window: &Window, on: bool, dark: bool) {
        let _ = clear_acrylic(window);
        if on {
            // Teinte légère, claire ou sombre selon le thème (ignorée par Windows 11,
            // qui suit son propre thème).
            let tint = if dark { (18, 18, 24, 90) } else { (245, 245, 250, 90) };
            if let Err(err) = apply_acrylic(window, Some(tint)) {
                eprintln!("effet de verre impossible : {err}");
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use tao::window::Window;

    pub fn apply(_window: &Window, _on: bool, _dark: bool) {}
}
