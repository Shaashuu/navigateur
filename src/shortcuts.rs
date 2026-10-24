//! Les raccourcis clavier de Feather, en une seule table : page.js (touches
//! tapées dans une page), l'interface (chrome.html), le menu du Mac et l'aide
//! des Paramètres en dérivent. Avant, chacun avait sa copie de la table, et
//! rien ne vérifiait qu'elles disaient la même chose.
//!
//! La table de ce système et le calcul qui la lit (`assets/shortcuts.js`) sont
//! posés dans page.js et chrome.html à la place de `__FEATHER_SHORTCUTS__` :
//! la première ligne qui convient à la touche gagne.

use std::sync::LazyLock;

use serde_json::{Value, json};

/// Vrai sous macOS : la table, l'aide et les menus de ce système.
const MAC: bool = cfg!(target_os = "macos");

/// Le calcul, commun à page.js et chrome.html.
const MATCHER: &str = include_str!("../assets/shortcuts.js");

/// Touches de modification exigées. « Ctrl » vaut aussi ⌘ (comme avant dans
/// les pages : les deux ne se distinguent pas) ; « Contrôle » est la touche
/// Ctrl seule, ⌃ sous macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mods {
    /// Rien d'exigé (F5, F11).
    Any,
    /// Ctrl ou ⌘, sans Alt ; Maj indifférente.
    Ctrl,
    /// Ctrl ou ⌘, sans Alt ni Maj.
    CtrlOnly,
    /// Ctrl ou ⌘ avec Maj, sans Alt.
    CtrlShift,
    /// Contrôle, sans ⌘ ni Alt ; Maj indifférente.
    Control,
    /// Contrôle et Maj, sans ⌘ ni Alt.
    ControlShift,
    /// Alt (⌥) seule ; Maj indifférente.
    Alt,
    /// ⌘ seule.
    Cmd,
    /// ⌘ et Maj.
    CmdShift,
    /// ⌘ et ⌥.
    CmdAlt,
    /// Maj seule.
    Shift,
    /// Alt et Maj, sans Ctrl ni ⌘ (Maj+Alt+N sous Windows, comme Chrome).
    AltShift,
}

impl Mods {
    /// Ce que vérifie `assets/shortcuts.js`, une lettre par touche (Ctrl, ⌘,
    /// Alt, Maj) : « 1 » appuyée, « 0 » relâchée, « . » indifférente ; « pp » :
    /// Ctrl ou ⌘.
    const fn pattern(self) -> &'static str {
        match self {
            Self::Any => "....",
            Self::Ctrl => "pp0.",
            Self::CtrlOnly => "pp00",
            Self::CtrlShift => "pp01",
            Self::Control => "100.",
            Self::ControlShift => "1001",
            Self::Alt => "001.",
            Self::Cmd => "0100",
            Self::CmdShift => "0101",
            Self::CmdAlt => "0110",
            Self::Shift => "0001",
            Self::AltShift => "0011",
        }
    }

    /// Les touches affichées (aide, menu du Mac), dans l'ordre de macOS :
    /// Contrôle, Alt, Maj, ⌘. « Ctrl ou ⌘ » s'écrit ⌘ sous macOS, Ctrl ailleurs.
    const fn held(self, mac: bool) -> Held {
        let (control, alt, shift, cmd) = match self {
            Self::Any => (false, false, false, false),
            Self::Ctrl | Self::CtrlOnly => (!mac, false, false, mac),
            Self::CtrlShift => (!mac, false, true, mac),
            Self::Control => (true, false, false, false),
            Self::ControlShift => (true, false, true, false),
            Self::Alt => (false, true, false, false),
            Self::Cmd => (false, false, false, true),
            Self::CmdShift => (false, false, true, true),
            Self::CmdAlt => (false, true, false, true),
            Self::Shift => (false, false, true, false),
            Self::AltShift => (false, true, true, false),
        };
        Held { control, alt, shift, cmd }
    }
}

/// Touches de modification à afficher, ou à poser sur une entrée de menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

/// La touche elle-même.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// `e.key` en minuscules : « t », « + », « f5 », « arrowleft ».
    Named(&'static str),
    /// La touche physique `e.code` (« KeyY », « BracketLeft ») : ⌥ change le
    /// caractère (⌥⌘L donne « ¬ »), pas la touche.
    Code(&'static str),
    /// Les chiffres 1 à 9 de la rangée du haut (`Digit1` à `Digit9`, touches
    /// physiques : un clavier AZERTY les donne avec Maj). Le chiffre complète le
    /// nom du raccourci (« tab » : « tab1 » à « tab9 »).
    Digits,
}

/// Systèmes où une ligne vaut, ou s'affiche dans l'aide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    All,
    Mac,
    NotMac,
}

impl Os {
    const fn has(self, mac: bool) -> bool {
        match self {
            Self::All => true,
            Self::Mac => mac,
            Self::NotMac => !mac,
        }
    }
}

/// Une ligne de la table.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    /// Le raccourci (`ui::Shortcut`, en snake_case) ; avec `Key::Digits`, le
    /// début de son nom.
    pub action: &'static str,
    pub key: Key,
    pub mods: Mods,
    pub os: Os,
    /// Où la touche s'affiche dans l'aide (`None` : nulle part, pour une
    /// seconde touche du même raccourci).
    pub shown: Option<Os>,
    /// Vaut aussi pendant la saisie dans un champ de texte : non pour ⌥← et
    /// ⌥→, qui y déplacent le curseur d'un mot sous macOS.
    pub typing: bool,
    /// Seulement dans le menu du Mac : la page reçoit la touche d'abord (⌘S
    /// d'un éditeur en ligne), et le menu ne la prend que si elle n'en fait rien.
    pub menu_only: bool,
}

const fn on(action: &'static str, key: Key, mods: Mods) -> Binding {
    Binding { action, key, mods, os: Os::All, shown: Some(Os::All), typing: true, menu_only: false }
}

const fn key(name: &'static str) -> Key {
    Key::Named(name)
}

const fn code(code: &'static str) -> Key {
    Key::Code(code)
}

impl Binding {
    const fn mac(self) -> Self {
        Self { os: Os::Mac, ..self }
    }

    const fn not_mac(self) -> Self {
        Self { os: Os::NotMac, ..self }
    }

    const fn hidden(self) -> Self {
        Self { shown: None, ..self }
    }

    const fn shown_on(self, os: Os) -> Self {
        Self { shown: Some(os), ..self }
    }

    const fn not_typing(self) -> Self {
        Self { typing: false, ..self }
    }

    const fn menu_only(self) -> Self {
        Self { menu_only: true, ..self }
    }
}

use Mods::*;

/// La table. L'ordre compte : la première ligne qui convient gagne (Ctrl+Maj+T
/// passe avant Ctrl+T, où Maj est indifférente), et la première ligne d'un
/// raccourci pour macOS est la touche de son entrée de menu.
pub const BINDINGS: &[Binding] = &[
    // macOS d'abord : ⇧⌘R, ⇧⌘D et ⇧⌘H passent avant ⌘R et ⌘D.
    on("reader", code("KeyR"), CmdShift).mac(),
    on("bookmark_all", code("KeyD"), CmdShift).mac(),
    on("home", code("KeyH"), CmdShift).mac(),
    on("previous_tab", code("BracketLeft"), CmdShift).mac(),
    on("next_tab", code("BracketRight"), CmdShift).mac(),
    on("previous_tab", key("arrowleft"), CmdAlt).mac(),
    on("next_tab", key("arrowright"), CmdAlt).mac(),
    on("history", code("KeyY"), Cmd).mac(),
    on("downloads", code("KeyL"), CmdAlt).mac(),
    on("bookmarks", code("KeyB"), CmdAlt).mac(),
    // ⌥N donne un accent mort sous macOS : la touche physique.
    on("split_view", code("KeyN"), CmdAlt).mac(),
    on("back", key("["), Cmd).mac(),
    on("forward", key("]"), Cmd).mac(),
    on("open_file", key("o"), Cmd).mac().menu_only(),
    on("save_page", key("s"), Cmd).mac().menu_only(),
    on("find_next", key("g"), Cmd).mac().menu_only(),
    on("find_previous", key("g"), CmdShift).mac().menu_only(),
    // Windows et Linux.
    on("history", key("h"), CtrlOnly).not_mac(),
    on("downloads", key("j"), CtrlOnly).not_mac(),
    on("bookmarks", key("o"), CtrlShift).not_mac(),
    on("fullscreen", key("f11"), Any).not_mac(),
    on("split_view", code("KeyN"), AltShift).not_mac(),
    // Partout.
    on("tab", Key::Digits, CtrlOnly),
    on("reopen_tab", key("t"), CtrlShift),
    on("capture", key("s"), CtrlShift),
    on("new_private_tab", key("n"), CtrlShift),
    on("focus", key("f"), CtrlShift),
    on("find", key("f"), CtrlOnly),
    on("print", key("p"), CtrlOnly),
    on("zoom_in", key("+"), Ctrl),
    on("zoom_in", key("="), Ctrl).hidden(),
    on("zoom_out", key("-"), Ctrl),
    on("zoom_out", key("_"), Ctrl).hidden(),
    on("zoom_reset", key("0"), Ctrl),
    // ⌥← et ⌥→ déplacent le curseur d'un mot dans un champ de texte sous macOS.
    on("back", key("arrowleft"), Alt).mac().not_typing(),
    on("forward", key("arrowright"), Alt).mac().not_typing(),
    on("back", key("arrowleft"), Alt).not_mac(),
    on("forward", key("arrowright"), Alt).not_mac(),
    on("reload", key("r"), Ctrl),
    on("reload", key("f5"), Any),
    on("focus_bar", key("l"), Ctrl),
    on("focus_bar", key("k"), Ctrl).hidden(),
    on("new_tab", key("t"), Ctrl),
    on("close_tab", key("w"), Ctrl),
    on("toggle_mute", key("m"), Control),
    on("previous_tab", key("tab"), ControlShift).shown_on(Os::NotMac),
    on("next_tab", key("tab"), Control).shown_on(Os::NotMac),
    on("previous_tab", key("pageup"), Control).shown_on(Os::NotMac),
    on("next_tab", key("pagedown"), Control).shown_on(Os::NotMac),
    on("settings", key(","), Ctrl),
    on("toggle_bookmark", key("d"), Ctrl),
    on("task_manager", key("escape"), Shift),
];

/// L'aide des Paramètres, dans l'ordre : chaque ligne montre les touches de
/// ses raccourcis sur ce système (une ligne sans touche n'est pas montrée).
pub const HELP: &[(&str, &[&str])] = &[
    ("Rechercher ou aller à une adresse", &["focus_bar"]),
    ("Nouvel onglet", &["new_tab"]),
    ("Fermer l'onglet", &["close_tab"]),
    ("Rouvrir l'onglet fermé", &["reopen_tab"]),
    ("Onglet privé", &["new_private_tab"]),
    ("Vue partagée", &["split_view"]),
    ("Onglet précédent / suivant", &["previous_tab", "next_tab"]),
    ("Page précédente / suivante", &["back", "forward"]),
    ("Actualiser", &["reload"]),
    ("Chercher dans la page", &["find"]),
    ("Occurrence suivante / précédente", &["find_next", "find_previous"]),
    ("Ajouter aux favoris", &["toggle_bookmark"]),
    ("Ajouter tous les onglets aux favoris", &["bookmark_all"]),
    ("Couper le son de l'onglet", &["toggle_mute"]),
    ("Imprimer", &["print"]),
    ("Zoom", &["zoom_in", "zoom_out", "zoom_reset"]),
    ("Capture de la page", &["capture"]),
    ("Enregistrer la page", &["save_page"]),
    ("Ouvrir un fichier", &["open_file"]),
    ("Utilisation de la mémoire", &["task_manager"]),
    ("Mode concentration", &["focus"]),
    ("Mode lecture", &["reader"]),
    ("Paramètres", &["settings"]),
    ("Aller à l'onglet 1 à 8, ou au dernier (9)", &["tab"]),
    ("Page d'accueil", &["home"]),
    ("Historique", &["history"]),
    ("Téléchargements", &["downloads"]),
    ("Favoris", &["bookmarks"]),
    ("Plein écran", &["fullscreen"]),
];

/// Raccourcis du système, rappelés à la fin de l'aide (Feather ne les lit pas).
const SYSTEM_MAC: &[(&str, &[&str])] = &[
    ("Copier · Couper · Coller", &["⌘C", "⌘X", "⌘V"]),
    ("Tout sélectionner · Annuler", &["⌘A", "⌘Z"]),
    ("Plein écran", &["⌃⌘F"]),
    ("Réduire · Masquer · Quitter", &["⌘M", "⌘H", "⌘Q"]),
];
const SYSTEM_OTHER: &[(&str, &[&str])] = &[
    ("Copier · Couper · Coller", &["Ctrl+C", "Ctrl+X", "Ctrl+V"]),
    ("Tout sélectionner · Annuler", &["Ctrl+A", "Ctrl+Z"]),
];

/// Le calcul et la table de ce système, prêts à poser dans une page.
pub fn script() -> &'static str {
    static SCRIPT: LazyLock<String> = LazyLock::new(|| script_for(MAC));
    &SCRIPT
}

fn script_for(mac: bool) -> String {
    format!("{}({})", MATCHER.trim_end(), table(mac))
}

/// La table lue par `assets/shortcuts.js` : `a` le raccourci, `k` la touche
/// (`c` : touche physique ; `d` : les chiffres), `m` les touches de
/// modification (voir `Mods::pattern`), `t: false` : pas pendant la saisie.
fn table(mac: bool) -> Value {
    BINDINGS
        .iter()
        .filter(|b| b.os.has(mac) && !b.menu_only)
        .map(|b| {
            let mut row = json!({ "a": b.action, "m": b.mods.pattern() });
            match b.key {
                Key::Named(k) => row["k"] = k.into(),
                Key::Code(c) => {
                    row["k"] = c.into();
                    row["c"] = true.into();
                }
                Key::Digits => row["d"] = true.into(),
            }
            if !b.typing {
                row["t"] = false.into();
            }
            row
        })
        .collect()
}

/// Les lignes de l'aide de ce système, en JSON : `[[libellé, [touches]], …]`.
pub fn help_json() -> String {
    serde_json::to_string(&help_rows(MAC)).unwrap_or_else(|_| "[]".into())
}

fn help_rows(mac: bool) -> Vec<(&'static str, Vec<String>)> {
    let mut rows: Vec<(&'static str, Vec<String>)> = HELP
        .iter()
        .filter_map(|(label, actions)| {
            let keys: Vec<String> = BINDINGS
                .iter()
                .filter(|b| actions.contains(&b.action) && b.os.has(mac) && b.shown.is_some_and(|os| os.has(mac)))
                .map(|b| written(b, mac))
                .collect();
            (!keys.is_empty()).then_some((*label, keys))
        })
        .collect();
    let system = if mac { SYSTEM_MAC } else { SYSTEM_OTHER };
    rows.extend(system.iter().map(|(label, keys)| (*label, keys.iter().map(|k| k.to_string()).collect())));
    rows
}

/// Une touche telle que l'aide l'écrit : « Ctrl+Maj+T », ou « ⇧⌘T » sous macOS.
fn written(b: &Binding, mac: bool) -> String {
    let held = b.mods.held(mac);
    let key = key_name(b.key, mac);
    if mac {
        let marks = [(held.control, "⌃"), (held.alt, "⌥"), (held.shift, "⇧"), (held.cmd, "⌘")];
        marks.iter().filter(|(on, _)| *on).map(|(_, m)| *m).chain([key.as_str()]).collect()
    } else {
        let names = [(held.control, "Ctrl"), (held.cmd, "Win"), (held.alt, "Alt"), (held.shift, "Maj")];
        names.iter().filter(|(on, _)| *on).map(|(_, n)| *n).chain([key.as_str()]).collect::<Vec<_>>().join("+")
    }
}

fn key_name(key: Key, mac: bool) -> String {
    match key {
        Key::Digits => "1…9".into(),
        Key::Code(code) => match code {
            "BracketLeft" => "[".into(),
            "BracketRight" => "]".into(),
            _ => code.strip_prefix("Key").or_else(|| code.strip_prefix("Digit")).unwrap_or(code).into(),
        },
        Key::Named(name) => match name {
            "arrowleft" => "←".into(),
            "arrowright" => "→".into(),
            "escape" => "Échap".into(),
            "tab" => "Tab".into(),
            "pageup" if mac => "⇞".into(),
            "pagedown" if mac => "⇟".into(),
            "pageup" => "Pg préc".into(),
            "pagedown" => "Pg suiv".into(),
            // « t » → « T », « f5 » → « F5 » ; « + » reste « + ».
            _ => name.to_uppercase(),
        },
    }
}

/// La touche d'une entrée du menu du Mac : le caractère attendu par AppKit et
/// les touches de modification, d'après la première ligne du raccourci pour
/// macOS. `None` : pas de touche.
#[cfg(any(target_os = "macos", test))]
pub fn menu_key(action: &str) -> Option<(String, Held)> {
    let b = BINDINGS.iter().find(|b| b.action == action && b.os.has(true))?;
    let mut held = b.mods.held(true);
    let key = match b.key {
        Key::Digits => return None,
        // ⇧⌘[ et ⇧⌘] : AppKit les écrit avec l'accolade, sans Maj.
        Key::Code("BracketLeft") if held.shift => {
            held.shift = false;
            "{".to_string()
        }
        Key::Code("BracketRight") if held.shift => {
            held.shift = false;
            "}".to_string()
        }
        Key::Code(_) => key_name(b.key, true).to_lowercase(),
        Key::Named(name) => match name {
            "escape" => "\u{1b}".into(),
            "tab" => "\t".into(),
            "arrowleft" => "\u{f702}".into(),
            "arrowright" => "\u{f703}".into(),
            "pageup" => "\u{f72c}".into(),
            "pagedown" => "\u{f72d}".into(),
            "f5" => "\u{f708}".into(),
            "f11" => "\u{f70e}".into(),
            _ => name.into(),
        },
    };
    Some((key, held))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Shortcut;

    fn shortcut_exists(name: &str) -> bool {
        serde_json::from_str::<Shortcut>(&format!("\"{name}\"")).is_ok()
    }

    /// Chaque ligne nomme un vrai raccourci : une faute de frappe donnerait une
    /// touche qui ne fait rien.
    #[test]
    fn raccourcis_connus() {
        for b in BINDINGS {
            if b.key == Key::Digits {
                assert!((1..=9).all(|n| shortcut_exists(&format!("{}{n}", b.action))), "{}", b.action);
            } else {
                assert!(shortcut_exists(b.action), "« {} » inconnu", b.action);
            }
        }
        for (label, actions) in HELP {
            for action in *actions {
                assert!(BINDINGS.iter().any(|b| b.action == *action), "{label} : « {action} » sans touche");
            }
        }
        // Une touche affichée doit avoir sa ligne dans l'aide.
        for b in BINDINGS.iter().filter(|b| b.shown.is_some()) {
            assert!(HELP.iter().any(|(_, actions)| actions.contains(&b.action)), "« {} » absent de l'aide", b.action);
        }
    }

    #[test]
    fn aide_ecrite() {
        let row = |mac: bool, label: &str| {
            help_rows(mac).into_iter().find(|(l, _)| *l == label).map(|(_, keys)| keys.join(" · ")).unwrap_or_default()
        };
        assert_eq!(row(false, "Rouvrir l'onglet fermé"), "Ctrl+Maj+T");
        assert_eq!(row(true, "Rouvrir l'onglet fermé"), "⇧⌘T");
        assert_eq!(row(false, "Onglet précédent / suivant"), "Ctrl+Maj+Tab · Ctrl+Tab · Ctrl+Pg préc · Ctrl+Pg suiv");
        assert_eq!(row(true, "Onglet précédent / suivant"), "⇧⌘[ · ⇧⌘] · ⌥⌘← · ⌥⌘→");
        assert_eq!(row(false, "Page précédente / suivante"), "Alt+← · Alt+→");
        assert_eq!(row(true, "Page précédente / suivante"), "⌘[ · ⌘] · ⌥← · ⌥→");
        assert_eq!(row(false, "Actualiser"), "Ctrl+R · F5");
        assert_eq!(row(false, "Zoom"), "Ctrl++ · Ctrl+- · Ctrl+0");
        assert_eq!(row(true, "Couper le son de l'onglet"), "⌃M");
        assert_eq!(row(false, "Utilisation de la mémoire"), "Maj+Échap");
        assert_eq!(row(true, "Téléchargements"), "⌥⌘L");
        assert_eq!(row(false, "Téléchargements"), "Ctrl+J");
        assert_eq!(row(true, "Aller à l'onglet 1 à 8, ou au dernier (9)"), "⌘1…9");
        assert_eq!(row(true, "Plein écran"), "⌃⌘F");
        assert_eq!(row(false, "Plein écran"), "F11");
        assert_eq!(row(false, "Enregistrer la page"), "");
        assert_eq!(row(true, "Vue partagée"), "⌥⌘N");
        assert_eq!(row(false, "Vue partagée"), "Alt+Maj+N");
        assert!(help_json().starts_with("[[\""));
    }

    /// Le menu du Mac garde ses touches d'avant la table.
    #[test]
    fn menu_du_mac() {
        let menu = |action: &str| {
            menu_key(action).map(|(key, h)| {
                let marks = [(h.control, "⌃"), (h.alt, "⌥"), (h.shift, "⇧"), (h.cmd, "⌘")];
                marks.iter().filter(|(on, _)| *on).map(|(_, m)| *m).collect::<String>() + &key
            })
        };
        let cases = [
            ("settings", "⌘,"),
            ("new_tab", "⌘t"),
            ("new_private_tab", "⇧⌘n"),
            ("focus_bar", "⌘l"),
            ("open_file", "⌘o"),
            ("close_tab", "⌘w"),
            ("save_page", "⌘s"),
            ("capture", "⇧⌘s"),
            ("print", "⌘p"),
            ("find", "⌘f"),
            ("find_next", "⌘g"),
            ("find_previous", "⇧⌘g"),
            ("reload", "⌘r"),
            ("reader", "⇧⌘r"),
            ("zoom_in", "⌘+"),
            ("zoom_out", "⌘-"),
            ("zoom_reset", "⌘0"),
            ("focus", "⇧⌘f"),
            ("downloads", "⌥⌘l"),
            ("home", "⇧⌘h"),
            ("back", "⌘["),
            ("forward", "⌘]"),
            ("reopen_tab", "⇧⌘t"),
            ("history", "⌘y"),
            ("toggle_bookmark", "⌘d"),
            ("bookmark_all", "⇧⌘d"),
            ("bookmarks", "⌥⌘b"),
            ("toggle_mute", "⌃m"),
            ("next_tab", "⌘}"),
            ("previous_tab", "⌘{"),
            ("task_manager", "⇧\u{1b}"),
            ("split_view", "⌥⌘n"),
        ];
        for (action, want) in cases {
            assert_eq!(menu(action).as_deref(), Some(want), "{action}");
        }
        assert_eq!(menu("pin_tab"), None);
        assert_eq!(menu("fullscreen"), None);
    }

    /// Une touche essayée : macOS, touches de modification, `e.key`, `e.code`,
    /// dans un champ de texte, raccourci attendu.
    type Case = (bool, &'static str, &'static str, &'static str, bool, Option<&'static str>);

    /// Le vrai calcul (`assets/shortcuts.js`), avec node quand il est installé
    /// (c'est le cas sur les machines de GitHub) : chaque touche donne le
    /// raccourci attendu, sur macOS et ailleurs.
    #[test]
    fn calcul_javascript() {
        let cases: &[Case] = &[
            (true, "cmd shift", "r", "KeyR", false, Some("reader")),
            (false, "ctrl shift", "r", "KeyR", false, Some("reload")),
            (true, "cmd", "r", "KeyR", false, Some("reload")),
            (true, "cmd shift", "d", "KeyD", false, Some("bookmark_all")),
            (true, "cmd", "d", "KeyD", false, Some("toggle_bookmark")),
            (false, "ctrl shift", "d", "KeyD", false, Some("toggle_bookmark")),
            (true, "cmd", "h", "KeyH", false, None),
            (true, "cmd shift", "{", "BracketLeft", false, Some("previous_tab")),
            (true, "cmd", "[", "BracketLeft", false, Some("back")),
            (false, "cmd", "[", "BracketLeft", false, None),
            (true, "cmd alt", "¬", "KeyL", false, Some("downloads")),
            (true, "cmd", "l", "KeyL", false, Some("focus_bar")),
            (true, "cmd", "y", "KeyY", false, Some("history")),
            (false, "ctrl", "h", "KeyH", false, Some("history")),
            (true, "cmd", "s", "KeyS", false, None),
            (true, "cmd", "o", "KeyO", false, None),
            (false, "ctrl shift", "o", "KeyO", false, Some("bookmarks")),
            (true, "cmd", "&", "Digit1", false, Some("tab1")),
            (false, "ctrl", "9", "Digit9", false, Some("tab9")),
            (false, "ctrl shift", "t", "KeyT", false, Some("reopen_tab")),
            (false, "ctrl", "t", "KeyT", false, Some("new_tab")),
            (false, "ctrl alt", "t", "KeyT", false, None),
            (false, "ctrl shift", "tab", "Tab", false, Some("previous_tab")),
            (false, "ctrl", "tab", "Tab", false, Some("next_tab")),
            (false, "ctrl", "pagedown", "PageDown", false, Some("next_tab")),
            (true, "cmd alt", "arrowleft", "ArrowLeft", false, Some("previous_tab")),
            (true, "alt", "arrowleft", "ArrowLeft", false, Some("back")),
            (true, "alt", "arrowleft", "ArrowLeft", true, None),
            (true, "alt", "arrowright", "ArrowRight", false, Some("forward")),
            (false, "alt", "arrowright", "ArrowRight", true, Some("forward")),
            (false, "", "f5", "F5", false, Some("reload")),
            (false, "ctrl", "f5", "F5", false, Some("reload")),
            (false, "", "f11", "F11", false, Some("fullscreen")),
            (true, "", "f11", "F11", false, None),
            (false, "ctrl shift", "+", "Equal", false, Some("zoom_in")),
            (false, "ctrl", "=", "Equal", false, Some("zoom_in")),
            (true, "ctrl", "m", "KeyM", false, Some("toggle_mute")),
            (true, "cmd", "m", "KeyM", false, None),
            (false, "shift", "escape", "Escape", false, Some("task_manager")),
            (false, "", "escape", "Escape", false, None),
            (false, "ctrl", "f", "KeyF", true, Some("find")),
            (false, "", "a", "KeyA", false, None),
            (true, "cmd alt", "˜", "KeyN", false, Some("split_view")),
            (false, "alt shift", "N", "KeyN", false, Some("split_view")),
            (false, "ctrl shift", "n", "KeyN", false, Some("new_private_tab")),
        ];
        let mut script = String::new();
        for mac in [true, false] {
            let events: Vec<Value> = cases
                .iter()
                .filter(|c| c.0 == mac)
                .map(|(_, mods, key, code, field, _)| {
                    json!({
                        "key": key, "code": code,
                        "ctrlKey": mods.contains("ctrl"), "metaKey": mods.contains("cmd"),
                        "altKey": mods.contains("alt"), "shiftKey": mods.contains("shift"),
                        "target": { "tagName": if *field { "INPUT" } else { "DIV" } },
                    })
                })
                .collect();
            script.push_str(&format!(
                "{{ const shortcutOf = {}; out.push(...{}.map((e) => shortcutOf(e))); }}\n",
                script_for(mac),
                Value::Array(events)
            ));
        }
        let script = format!("const out = [];\n{script}process.stdout.write(JSON.stringify(out));\n");
        let path = std::env::temp_dir().join(format!("feather-raccourcis-{}.js", std::process::id()));
        std::fs::write(&path, script).unwrap();
        let output = std::process::Command::new("node").arg(&path).output();
        let _ = std::fs::remove_file(&path);
        let Ok(output) = output else {
            eprintln!("node absent : calcul JavaScript non vérifié");
            return;
        };
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let got: Vec<Option<String>> = serde_json::from_slice(&output.stdout).unwrap();
        // Même ordre que le script : macOS d'abord.
        let ordered: Vec<_> = cases.iter().filter(|c| c.0).chain(cases.iter().filter(|c| !c.0)).collect();
        assert_eq!(got.len(), ordered.len());
        for ((mac, mods, key, _, field, want), got) in ordered.into_iter().zip(&got) {
            assert_eq!(got.as_deref(), *want, "macOS : {mac}, « {mods} » {key}, champ : {field}");
        }
    }
}
