//! macOS : la barre des menus de Feather.
//!
//! Sans menu « Édition », macOS n'envoie pas ⌘C, ⌘V, ⌘X, ⌘A ni ⌘Z aux pages :
//! copier-coller ne marchait nulle part. Chaque entrée envoie l'action
//! standard (`copy:`, `paste:`…) à ce qui a le focus (page, barre d'adresse).
//! Le menu de l'application ajoute ⌘Q et ⌘H, celui de la fenêtre ⌘M et le
//! plein écran (⌃⌘F).

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

/// Une entrée : titre, action, touche (vide : pas de raccourci) et touches de
/// modification. `None` dans une liste : un séparateur.
type Entry = (&'static str, Sel, &'static str, NSEventModifierFlags);

/// À appeler depuis le fil principal, avant la boucle des événements.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let cmd = NSEventModifierFlags::Command;
    let shift = NSEventModifierFlags::Shift;
    let alt = NSEventModifierFlags::Option;
    let ctrl = NSEventModifierFlags::Control;
    let none = NSEventModifierFlags::empty();

    let app: &[Option<Entry>] = &[
        Some(("Masquer Feather", sel!(hide:), "h", cmd)),
        Some(("Masquer les autres", sel!(hideOtherApplications:), "h", cmd | alt)),
        Some(("Tout afficher", sel!(unhideAllApplications:), "", none)),
        None,
        // Passe par la fermeture de l'application : tao envoie alors
        // `LoopDestroyed`, et Feather enregistre tout (voir main.rs).
        Some(("Quitter Feather", sel!(terminate:), "q", cmd)),
    ];
    let edit: &[Option<Entry>] = &[
        Some(("Annuler", sel!(undo:), "z", cmd)),
        Some(("Rétablir", sel!(redo:), "z", cmd | shift)),
        None,
        Some(("Couper", sel!(cut:), "x", cmd)),
        Some(("Copier", sel!(copy:), "c", cmd)),
        Some(("Coller", sel!(paste:), "v", cmd)),
        Some(("Coller sans la mise en forme", sel!(pasteAsPlainText:), "v", cmd | shift | alt)),
        Some(("Supprimer", sel!(delete:), "", none)),
        Some(("Tout sélectionner", sel!(selectAll:), "a", cmd)),
    ];
    let window: &[Option<Entry>] = &[
        Some(("Réduire", sel!(performMiniaturize:), "m", cmd)),
        Some(("Agrandir", sel!(performZoom:), "", none)),
        None,
        Some(("Plein écran", sel!(toggleFullScreen:), "f", cmd | ctrl)),
    ];

    let bar = menu(mtm, "");
    // Le premier menu porte toujours le nom de l'application, quel que soit son titre.
    add_submenu(mtm, &bar, &menu_with(mtm, "Feather", app));
    add_submenu(mtm, &bar, &menu_with(mtm, "Édition", edit));
    let windows = menu_with(mtm, "Fenêtre", window);
    add_submenu(mtm, &bar, &windows);

    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setMainMenu(Some(&bar));
    // macOS y ajoute la liste des fenêtres ouvertes.
    ns_app.setWindowsMenu(Some(&windows));
}

fn menu(mtm: MainThreadMarker, title: &str) -> Retained<NSMenu> {
    NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title))
}

fn menu_with(mtm: MainThreadMarker, title: &str, entries: &[Option<Entry>]) -> Retained<NSMenu> {
    let m = menu(mtm, title);
    for entry in entries {
        let item = match entry {
            None => NSMenuItem::separatorItem(mtm),
            Some((label, action, key, mods)) => {
                // SAFETY : chaque action est une méthode standard d'AppKit, envoyée à
                // ce qui a le focus ; l'entrée est grisée si personne ne la connaît.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(label),
                        Some(*action),
                        &NSString::from_str(key),
                    )
                };
                if !key.is_empty() {
                    item.setKeyEquivalentModifierMask(*mods);
                }
                item
            }
        };
        m.addItem(&item);
    }
    m
}

fn add_submenu(mtm: MainThreadMarker, bar: &NSMenu, sub: &NSMenu) {
    let item = NSMenuItem::new(mtm);
    item.setSubmenu(Some(sub));
    bar.addItem(&item);
}
