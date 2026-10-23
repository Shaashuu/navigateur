//! macOS : la barre des menus de Feather.
//!
//! Sans menu « Édition », macOS n'envoie pas ⌘C, ⌘V, ⌘X, ⌘A ni ⌘Z aux pages :
//! copier-coller ne marchait nulle part. Ces entrées envoient l'action
//! standard (`copy:`, `paste:`…) à ce qui a le focus (page, barre d'adresse).
//!
//! Les menus Fichier, Présentation, Historique, Favoris, Onglet, Fenêtre et
//! Aide lancent les actions de Feather : chaque entrée passe par la même
//! file que les raccourcis des pages (`Shortcut`), donc fait exactement la même
//! chose. Leurs touches viennent de la table des raccourcis (`shortcuts.rs`),
//! la même que celle de page.js et de l'interface : quand une page a le focus,
//! page.js traite la touche (et l'arrête), le menu ne la reçoit pas en double.
//! Le menu prend le relais là où page.js ne tourne pas (fenêtre d'une
//! extension, page qui n'a pas encore chargé).
//!
//! Comme dans Safari, Historique liste les dernières pages et les onglets
//! fermés, Favoris les favoris : ces entrées sont refaites à chaque ouverture
//! du menu (`menuNeedsUpdate:`), rien n'est tenu à jour en arrière-plan.

use std::cell::RefCell;
use std::sync::{Arc, RwLock};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSEventModifierFlags, NSMenu, NSMenuDelegate, NSMenuItem,
};
use objc2_foundation::NSString;
use tao::event_loop::EventLoopProxy;

use crate::UserEvent;
use crate::library::Library;
use crate::shortcuts::{self, Held};

/// Pages récentes dans le menu Historique, favoris dans le menu Favoris.
const RECENT: usize = 15;
const BOOKMARKS: usize = 40;
/// Longueur d'un titre dans un menu, au-delà coupé par « … ».
const TITLE_CHARS: usize = 60;

/// Les entrées refaites à chaque ouverture ont un `tag` à partir de ces
/// valeurs (une plage par liste) ; les entrées fixes ont un `tag` plus petit.
const LIST_HISTORY: isize = 10_000;
const LIST_CLOSED: isize = 20_000;
const LIST_BOOKMARKS: isize = 30_000;

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
type Entry = (&'static str, Action, String, NSEventModifierFlags);

/// Pas un raccourci : l'historique s'efface après une confirmation (voir `confirm`).
const CLEAR_HISTORY: &str = "clear_history";

thread_local! {
    /// Où envoyer les actions de Feather (fil principal seulement).
    static PROXY: RefCell<Option<EventLoopProxy<UserEvent>>> = const { RefCell::new(None) };
    /// Noms des raccourcis, rangés selon le `tag` de leur entrée.
    static ACTIONS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    /// Historique et favoris, relus à l'ouverture des menus.
    static LIBRARY: RefCell<Option<Arc<RwLock<Library>>>> = const { RefCell::new(None) };
    /// Onglets fermés, le plus ancien d'abord (copiés par `set_closed`).
    static CLOSED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Adresses des entrées refaites à l'ouverture, par liste (même ordre que les `tag`).
    static LISTS: RefCell<[Vec<String>; 3]> = const { RefCell::new([Vec::new(), Vec::new(), Vec::new()]) };
}

/// La bibliothèque lue par les menus Historique et Favoris.
pub fn set_library(library: Arc<RwLock<Library>>) {
    LIBRARY.with_borrow_mut(|l| *l = Some(library));
}

/// Les onglets fermés ont changé (fermeture, réouverture).
pub fn set_closed(closed: &[String]) {
    CLOSED.with_borrow_mut(|c| *c = closed.to_vec());
}

fn send(event: UserEvent) {
    PROXY.with_borrow(|p| {
        if let Some(proxy) = p {
            let _ = proxy.send_event(event);
        }
    });
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
            send(UserEvent::Chrome(message));
        }

        /// Une page de l'historique, un onglet fermé ou un favori a été choisi.
        #[unsafe(method(featherOpen:))]
        fn feather_open(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            let list = ((tag - LIST_HISTORY) / 10_000) as usize;
            let index = ((tag - LIST_HISTORY) % 10_000) as usize;
            let Some(url) = LISTS.with_borrow(|l| l.get(list).and_then(|l| l.get(index)).cloned()) else {
                return;
            };
            if (LIST_CLOSED..LIST_BOOKMARKS).contains(&tag) {
                send(UserEvent::ReopenClosed(url));
            } else {
                let message = serde_json::json!({ "type": "open_url", "url": url });
                send(UserEvent::Chrome(message.to_string()));
            }
        }
    }

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSMenuDelegate for Target {
        /// Juste avant l'ouverture du menu Historique ou Favoris : ses listes sont refaites.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            refresh(self.mtm(), menu, self);
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

/// Refait les listes du menu Historique ou Favoris, juste avant son ouverture.
fn refresh(mtm: MainThreadMarker, menu: &NSMenu, target: &Target) {
    // Les entrées de la fois précédente partent (les fixes ont un `tag` plus petit).
    for i in (0..menu.numberOfItems()).rev() {
        if menu.itemAtIndex(i).is_some_and(|item| item.tag() >= LIST_HISTORY) {
            menu.removeItemAtIndex(i);
        }
    }
    let Some(library) = LIBRARY.with_borrow(|l| l.clone()) else { return };
    let Ok(library) = library.read() else { return };
    // Le marqueur des séparateurs et du sous-menu : jamais une adresse.
    let marker = LIST_BOOKMARKS + 9_999;
    match menu.title().to_string().as_str() {
        "Historique" => {
            let closed: Vec<String> = CLOSED.with_borrow(|c| c.iter().rev().take(RECENT).cloned().collect());
            if !closed.is_empty() {
                let sub = self::menu(mtm, "Onglets fermés récemment");
                for (i, url) in closed.iter().enumerate() {
                    let title = library.title_of(url).unwrap_or(url);
                    sub.addItem(&list_item(mtm, title, url, LIST_CLOSED + i as isize, target));
                }
                add_marked(menu, NSMenuItem::separatorItem(mtm), marker);
                let holder = NSMenuItem::new(mtm);
                holder.setTitle(&NSString::from_str("Onglets fermés récemment"));
                holder.setSubmenu(Some(&sub));
                add_marked(menu, holder, marker);
            }
            let mut recent: Vec<(String, String)> = Vec::new();
            for visit in library.search_history("", RECENT * 4) {
                if recent.len() < RECENT && !recent.iter().any(|(u, _)| *u == visit.url) {
                    recent.push((visit.url.clone(), visit.title.clone()));
                }
            }
            if !recent.is_empty() {
                add_marked(menu, NSMenuItem::separatorItem(mtm), marker);
            }
            for (i, (url, title)) in recent.iter().enumerate() {
                menu.addItem(&list_item(mtm, title, url, LIST_HISTORY + i as isize, target));
            }
            LISTS.with_borrow_mut(|l| {
                l[0] = recent.into_iter().map(|(u, _)| u).collect();
                l[1] = closed;
            });
        }
        "Favoris" => {
            let marks: Vec<_> = library.bookmarks().iter().take(BOOKMARKS).collect();
            if !marks.is_empty() {
                add_marked(menu, NSMenuItem::separatorItem(mtm), marker);
            }
            for (i, b) in marks.iter().enumerate() {
                menu.addItem(&list_item(mtm, &b.title, &b.url, LIST_BOOKMARKS + i as isize, target));
            }
            LISTS.with_borrow_mut(|l| l[2] = marks.iter().map(|b| b.url.clone()).collect());
        }
        _ => {}
    }
}

fn add_marked(menu: &NSMenu, item: Retained<NSMenuItem>, tag: isize) {
    item.setTag(tag);
    menu.addItem(&item);
}

/// Une page à ouvrir : son titre (ou son adresse), coupé s'il est trop long.
fn list_item(mtm: MainThreadMarker, title: &str, url: &str, tag: isize, target: &Target) -> Retained<NSMenuItem> {
    let title = if title.trim().is_empty() { url } else { title.trim() };
    let label: String = if title.chars().count() > TITLE_CHARS {
        title.chars().take(TITLE_CHARS - 1).chain(['…']).collect()
    } else {
        title.to_string()
    };
    // SAFETY : `featherOpen:` de `Target`, qui attend une entrée de menu.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(&label),
            Some(sel!(featherOpen:)),
            &NSString::from_str(""),
        )
    };
    item.setTag(tag);
    item.setToolTip(Some(&NSString::from_str(url)));
    // SAFETY : `Target` répond à `featherOpen:` et vit jusqu'à la fin.
    unsafe { item.setTarget(Some(target)) };
    item
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
/// fenêtres, « Aide » la recherche dans les menus. Les touches des entrées de
/// Feather viennent de la table des raccourcis (`shortcuts.rs`).
fn menus() -> Vec<(&'static str, Vec<Option<Entry>>)> {
    let cmd = NSEventModifierFlags::Command;
    let shift = NSEventModifierFlags::Shift;
    let alt = NSEventModifierFlags::Option;
    let ctrl = NSEventModifierFlags::Control;
    let none = NSEventModifierFlags::empty();
    // Action standard d'AppKit, avec sa touche.
    let standard = |label, sel, key: &str, mods| Some((label, Action::Std(sel), key.to_string(), mods));
    // Action de Feather, avec la touche de la table.
    let feather = |label, name| {
        let (key, mods) = shortcuts::menu_key(name).map_or((String::new(), none), |(key, held)| (key, modifiers(held)));
        Some((label, Action::App(name), key, mods))
    };

    let app = vec![
        standard("À propos de Feather", sel!(orderFrontStandardAboutPanel:), "", none),
        None,
        feather("Paramètres…", "settings"),
        feather("Extensions…", "extensions"),
        None,
        feather("Effacer l'historique…", CLEAR_HISTORY),
        None,
        standard("Masquer Feather", sel!(hide:), "h", cmd),
        standard("Masquer les autres", sel!(hideOtherApplications:), "h", cmd | alt),
        standard("Tout afficher", sel!(unhideAllApplications:), "", none),
        None,
        // Passe par la fermeture de l'application : tao envoie alors
        // `LoopDestroyed`, et Feather enregistre tout (voir main.rs).
        standard("Quitter Feather", sel!(terminate:), "q", cmd),
    ];
    let file = vec![
        feather("Nouvel onglet", "new_tab"),
        feather("Nouvel onglet privé", "new_private_tab"),
        feather("Ouvrir une adresse…", "focus_bar"),
        feather("Ouvrir un fichier…", "open_file"),
        None,
        feather("Fermer l'onglet", "close_tab"),
        None,
        feather("Enregistrer la page", "save_page"),
        feather("Exporter en PDF", "export_pdf"),
        feather("Partager…", "share"),
        None,
        feather("Importer des favoris…", "import"),
        None,
        feather("Capturer la page", "capture"),
        feather("Imprimer…", "print"),
    ];
    let edit = vec![
        standard("Annuler", sel!(undo:), "z", cmd),
        standard("Rétablir", sel!(redo:), "z", cmd | shift),
        None,
        standard("Couper", sel!(cut:), "x", cmd),
        standard("Copier", sel!(copy:), "c", cmd),
        standard("Coller", sel!(paste:), "v", cmd),
        standard("Coller sans la mise en forme", sel!(pasteAsPlainText:), "v", cmd | shift | alt),
        standard("Supprimer", sel!(delete:), "", none),
        standard("Tout sélectionner", sel!(selectAll:), "a", cmd),
        None,
        feather("Rechercher dans la page…", "find"),
        feather("Occurrence suivante", "find_next"),
        feather("Occurrence précédente", "find_previous"),
    ];
    let view = vec![
        feather("Actualiser la page", "reload"),
        feather("Mode lecture", "reader"),
        None,
        feather("Zoom avant", "zoom_in"),
        feather("Zoom arrière", "zoom_out"),
        feather("Taille réelle", "zoom_reset"),
        None,
        feather("Mode concentration", "focus"),
        standard("Plein écran", sel!(toggleFullScreen:), "f", cmd | ctrl),
        None,
        // Comme Safari : pas de menu Téléchargements, la liste est ici.
        feather("Afficher les téléchargements", "downloads"),
    ];
    let history = vec![
        feather("Accueil", "home"),
        feather("Précédent", "back"),
        feather("Suivant", "forward"),
        None,
        feather("Rouvrir le dernier onglet fermé", "reopen_tab"),
        None,
        feather("Afficher l'historique", "history"),
    ];
    let bookmarks = vec![
        feather("Ajouter aux favoris", "toggle_bookmark"),
        feather("Ajouter tous les onglets aux favoris", "bookmark_all"),
        None,
        feather("Afficher les favoris", "bookmarks"),
        feather("Importer des favoris…", "import"),
    ];
    // Comme le menu Onglet de Chrome.
    let tab = vec![
        feather("Épingler ou détacher l'onglet", "pin_tab"),
        feather("Dupliquer l'onglet", "duplicate_tab"),
        feather("Ranger dans un groupe…", "group_tab"),
        None,
        feather("Couper le son de l'onglet", "toggle_mute"),
        None,
        feather("Fermer les autres onglets", "close_other_tabs"),
    ];
    let window = vec![
        standard("Réduire", sel!(performMiniaturize:), "m", cmd),
        standard("Agrandir", sel!(performZoom:), "", none),
        None,
        feather("Onglet suivant", "next_tab"),
        feather("Onglet précédent", "previous_tab"),
        None,
        feather("Utilisation de la mémoire", "task_manager"),
    ];
    // macOS ajoute seul, en tête, la recherche dans tous les menus.
    let help = vec![feather("Signaler un problème…", "report_issue")];

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

/// Touches de modification d'une entrée, d'après la table des raccourcis.
fn modifiers(held: Held) -> NSEventModifierFlags {
    [
        (held.control, NSEventModifierFlags::Control),
        (held.alt, NSEventModifierFlags::Option),
        (held.shift, NSEventModifierFlags::Shift),
        (held.cmd, NSEventModifierFlags::Command),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(NSEventModifierFlags::empty(), |mods, (_, flag)| mods | flag)
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
            "Historique" | "Favoris" => sub.setDelegate(Some(ProtocolObject::from_ref(&*target))),
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
