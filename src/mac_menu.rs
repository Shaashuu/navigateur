//! macOS : la barre des menus de Feather.
//!
//! Sans menu « Édition », macOS n'envoie pas ⌘C, ⌘V, ⌘X, ⌘A ni ⌘Z aux pages :
//! copier-coller ne marchait nulle part. Ces entrées envoient l'action
//! standard (`copy:`, `paste:`…) à ce qui a le focus (page, barre d'adresse).
//!
//! Les menus Fichier, Présentation, Historique, Favoris, Onglet, Fenêtre et
//! Aide lancent les actions de Feather : chaque entrée passe par la même
//! file que les raccourcis des pages (`Shortcut`), donc fait exactement la même
//! chose. Les touches affichées sont celles que page.js et l'interface lisent
//! déjà : quand une page a le focus, page.js traite la touche (et l'arrête),
//! le menu ne la reçoit pas en double. Le menu prend le relais là où page.js
//! ne tourne pas (fenêtre d'une extension, page qui n'a pas encore chargé).

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;
use tao::event_loop::EventLoopProxy;

use crate::UserEvent;

/// Ce que fait une entrée.
#[derive(Clone, Copy)]
enum Action {
    /// Action standard d'AppKit, envoyée à ce qui a le focus.
    Std(Sel),
    /// Action de Feather : le nom du raccourci (`Shortcut`, en snake_case).
    App(&'static str),
}

/// Une entrée : titre, action, touche (vide : pas de raccourci) et touches de
/// modification. `None` dans une liste : un séparateur.
type Entry = (&'static str, Action, &'static str, NSEventModifierFlags);

/// Pas un raccourci : l'historique s'efface après une confirmation (voir `confirm`).
const CLEAR_HISTORY: &str = "clear_history";

thread_local! {
    /// Où envoyer les actions de Feather (fil principal seulement).
    static PROXY: RefCell<Option<EventLoopProxy<UserEvent>>> = const { RefCell::new(None) };
    /// Noms des raccourcis, rangés selon le `tag` de leur entrée.
    static ACTIONS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

define_class!(
    // SAFETY : NSObject n'impose rien à ses sous-classes ; pas de `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FeatherMenuTarget"]
    struct Target;

    impl Target {
        /// Une entrée de Feather a été choisie : son `tag` dit laquelle.
        #[unsafe(method(featherMenu:))]
        fn feather_menu(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            let Some(name) = ACTIONS.with_borrow(|a| usize::try_from(tag).ok().and_then(|i| a.get(i).copied())) else {
                return;
            };
            let message = if name == CLEAR_HISTORY {
                if !confirm(self.mtm(), "Effacer tout l'historique ?", "Les pages visitées et leur texte gardé pour la recherche seront effacés. Les favoris restent.", "Effacer") {
                    return;
                }
                r#"{"type":"clear_history"}"#.to_string()
            } else {
                format!(r#"{{"type":"shortcut","shortcut":"{name}"}}"#)
            };
            PROXY.with_borrow(|p| {
                if let Some(proxy) = p {
                    let _ = proxy.send_event(UserEvent::Chrome(message));
                }
            });
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY : `init` de NSObject.
        unsafe { msg_send![super(this), init] }
    }
}

/// Fenêtre de confirmation de macOS : `true` si l'utilisateur choisit `action`.
fn confirm(mtm: MainThreadMarker, title: &str, text: &str, action: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(text));
    alert.addButtonWithTitle(&NSString::from_str(action));
    alert.addButtonWithTitle(&NSString::from_str("Annuler"));
    alert.runModal() == NSAlertFirstButtonReturn
}

/// Les menus, dans l'ordre de la barre ; « Fenêtre » reçoit la liste des
/// fenêtres, « Aide » la recherche dans les menus.
fn menus() -> Vec<(&'static str, Vec<Option<Entry>>)> {
    let cmd = NSEventModifierFlags::Command;
    let shift = NSEventModifierFlags::Shift;
    let alt = NSEventModifierFlags::Option;
    let ctrl = NSEventModifierFlags::Control;
    let none = NSEventModifierFlags::empty();
    use Action::{App, Std};

    let app = vec![
        Some(("À propos de Feather", Std(sel!(orderFrontStandardAboutPanel:)), "", none)),
        None,
        Some(("Paramètres…", App("settings"), ",", cmd)),
        Some(("Extensions…", App("extensions"), "", none)),
        None,
        Some(("Effacer l'historique…", App(CLEAR_HISTORY), "", none)),
        None,
        Some(("Masquer Feather", Std(sel!(hide:)), "h", cmd)),
        Some(("Masquer les autres", Std(sel!(hideOtherApplications:)), "h", cmd | alt)),
        Some(("Tout afficher", Std(sel!(unhideAllApplications:)), "", none)),
        None,
        // Passe par la fermeture de l'application : tao envoie alors
        // `LoopDestroyed`, et Feather enregistre tout (voir main.rs).
        Some(("Quitter Feather", Std(sel!(terminate:)), "q", cmd)),
    ];
    let file = vec![
        Some(("Nouvel onglet", App("new_tab"), "t", cmd)),
        Some(("Nouvel onglet privé", App("new_private_tab"), "n", cmd | shift)),
        Some(("Ouvrir une adresse…", App("focus_bar"), "l", cmd)),
        None,
        Some(("Fermer l'onglet", App("close_tab"), "w", cmd)),
        None,
        Some(("Importer des favoris…", App("import"), "", none)),
        None,
        Some(("Capturer la page", App("capture"), "s", cmd | shift)),
        Some(("Imprimer…", App("print"), "p", cmd)),
    ];
    let edit = vec![
        Some(("Annuler", Std(sel!(undo:)), "z", cmd)),
        Some(("Rétablir", Std(sel!(redo:)), "z", cmd | shift)),
        None,
        Some(("Couper", Std(sel!(cut:)), "x", cmd)),
        Some(("Copier", Std(sel!(copy:)), "c", cmd)),
        Some(("Coller", Std(sel!(paste:)), "v", cmd)),
        Some(("Coller sans la mise en forme", Std(sel!(pasteAsPlainText:)), "v", cmd | shift | alt)),
        Some(("Supprimer", Std(sel!(delete:)), "", none)),
        Some(("Tout sélectionner", Std(sel!(selectAll:)), "a", cmd)),
        None,
        Some(("Rechercher dans la page…", App("find"), "f", cmd)),
        Some(("Occurrence suivante", App("find_next"), "g", cmd)),
        Some(("Occurrence précédente", App("find_previous"), "g", cmd | shift)),
    ];
    let view = vec![
        Some(("Actualiser la page", App("reload"), "r", cmd)),
        Some(("Mode lecture", App("reader"), "r", cmd | shift)),
        None,
        Some(("Zoom avant", App("zoom_in"), "+", cmd)),
        Some(("Zoom arrière", App("zoom_out"), "-", cmd)),
        Some(("Taille réelle", App("zoom_reset"), "0", cmd)),
        None,
        Some(("Mode concentration", App("focus"), "f", cmd | shift)),
        Some(("Plein écran", Std(sel!(toggleFullScreen:)), "f", cmd | ctrl)),
        None,
        // Comme Safari : pas de menu Téléchargements, la liste est ici.
        Some(("Afficher les téléchargements", App("downloads"), "l", cmd | alt)),
    ];
    let history = vec![
        Some(("Accueil", App("home"), "h", cmd | shift)),
        Some(("Précédent", App("back"), "[", cmd)),
        Some(("Suivant", App("forward"), "]", cmd)),
        None,
        Some(("Rouvrir le dernier onglet fermé", App("reopen_tab"), "t", cmd | shift)),
        None,
        Some(("Afficher l'historique", App("history"), "y", cmd)),
    ];
    let bookmarks = vec![
        Some(("Ajouter aux favoris", App("toggle_bookmark"), "d", cmd)),
        Some(("Ajouter tous les onglets aux favoris", App("bookmark_all"), "d", cmd | shift)),
        None,
        Some(("Afficher les favoris", App("bookmarks"), "b", cmd | alt)),
        Some(("Importer des favoris…", App("import"), "", none)),
    ];
    // Comme le menu Onglet de Chrome.
    let tab = vec![
        Some(("Épingler ou détacher l'onglet", App("pin_tab"), "", none)),
        Some(("Dupliquer l'onglet", App("duplicate_tab"), "", none)),
        Some(("Ranger dans un groupe…", App("group_tab"), "", none)),
        None,
        Some(("Couper le son de l'onglet", App("toggle_mute"), "m", ctrl)),
        None,
        Some(("Fermer les autres onglets", App("close_other_tabs"), "", none)),
    ];
    let window = vec![
        Some(("Réduire", Std(sel!(performMiniaturize:)), "m", cmd)),
        Some(("Agrandir", Std(sel!(performZoom:)), "", none)),
        None,
        // ⇧⌘] et ⇧⌘[ : AppKit les écrit avec l'accolade.
        Some(("Onglet suivant", App("next_tab"), "}", cmd)),
        Some(("Onglet précédent", App("previous_tab"), "{", cmd)),
        None,
        Some(("Utilisation de la mémoire", App("task_manager"), "\u{1b}", shift)),
    ];
    // macOS ajoute seul, en tête, la recherche dans tous les menus.
    let help = vec![
        Some(("Signaler un problème…", App("report_issue"), "", none)),
    ];

    vec![
        // Le premier menu porte toujours le nom de l'application, quel que soit son titre.
        ("Feather", app),
        ("Fichier", file),
        ("Édition", edit),
        ("Présentation", view),
        ("Historique", history),
        ("Favoris", bookmarks),
        ("Onglet", tab),
        ("Fenêtre", window),
        ("Aide", help),
    ]
}

/// À appeler depuis le fil principal, avant la boucle des événements.
pub fn install(proxy: EventLoopProxy<UserEvent>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    PROXY.with_borrow_mut(|p| *p = Some(proxy));
    // Gardée en vie jusqu'à la fin : les entrées ne retiennent pas leur cible.
    let target = Target::new(mtm);
    let bar = menu(mtm, "");
    let ns_app = NSApplication::sharedApplication(mtm);
    for (title, entries) in menus() {
        let sub = menu_with(mtm, title, &entries, &target);
        add_submenu(mtm, &bar, &sub);
        match title {
            "Fenêtre" => ns_app.setWindowsMenu(Some(&sub)),
            "Aide" => ns_app.setHelpMenu(Some(&sub)),
            _ => {}
        }
    }
    ns_app.setMainMenu(Some(&bar));
    std::mem::forget(target);
}

fn menu(mtm: MainThreadMarker, title: &str) -> Retained<NSMenu> {
    NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title))
}

fn menu_with(mtm: MainThreadMarker, title: &str, entries: &[Option<Entry>], target: &Target) -> Retained<NSMenu> {
    let m = menu(mtm, title);
    for entry in entries {
        let item = match entry {
            None => NSMenuItem::separatorItem(mtm),
            Some((label, action, key, mods)) => {
                let selector = match action {
                    Action::Std(sel) => *sel,
                    Action::App(_) => sel!(featherMenu:),
                };
                // SAFETY : une action standard d'AppKit, envoyée à ce qui a le
                // focus (l'entrée est grisée si personne ne la connaît), ou
                // `featherMenu:` de `Target`, qui attend une entrée de menu.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(label),
                        Some(selector),
                        &NSString::from_str(key),
                    )
                };
                if let Action::App(name) = action {
                    let tag = ACTIONS.with_borrow_mut(|a| {
                        a.push(name);
                        a.len() - 1
                    });
                    item.setTag(tag as isize);
                    // SAFETY : `Target` répond à `featherMenu:` et vit jusqu'à la fin.
                    unsafe { item.setTarget(Some(target)) };
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Shortcut;

    /// Chaque entrée de Feather doit nommer un vrai raccourci : une faute de
    /// frappe donnerait une entrée qui ne fait rien.
    #[test]
    fn entrees_connues() {
        let mut n = 0;
        for (_, entries) in menus() {
            for (label, action, _, _) in entries.into_iter().flatten() {
                if let Action::App(name) = action
                    && name != CLEAR_HISTORY
                {
                    let json = format!("\"{name}\"");
                    assert!(serde_json::from_str::<Shortcut>(&json).is_ok(), "{label} : « {name} » inconnu");
                    n += 1;
                }
            }
        }
        assert!(n > 20);
    }
}
