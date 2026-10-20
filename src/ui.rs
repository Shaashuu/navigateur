//! L'interface du navigateur (la « chrome ») : une webview HTML/CSS placée
//! sous les pages, qui dessine la colonne de gauche (paramètres, pastille de
//! recherche, onglets) et la barre de recherche qui s'ouvre par-dessus la page.
//! Ce module définit aussi les messages échangés avec elle.

use serde::Deserialize;
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::window::Window;
use wry::{Rect, WebView, WebViewBuilder};

use crate::library::Bookmark;
use crate::settings::{BarPosition, Settings, Theme};
use crate::sitedata::Permission;
use crate::tab_manager::TabId;

/// Largeur la plus étroite de la colonne des pastilles (pixels logiques).
pub const SIDEBAR_WIDTH: f64 = crate::settings::SIDEBAR_MIN as f64;
/// Hauteur de la barre quand elle est placée en haut.
pub const TOP_BAR_HEIGHT: f64 = 44.0;
/// Hauteur du bandeau du bas (recherche dans la page, question d'autorisation,
/// fin d'un minuteur) : la page rétrécit pour le laisser voir.
pub const STRIP_HEIGHT: f64 = 48.0;

pub const CHROME_HTML: &str = include_str!("../assets/chrome.html");
const HOME_HTML: &str = include_str!("../assets/home.html");

/// Injecté dans chaque page : raccourcis clavier, son, icône du site, recherche
/// dans la page, menu des liens, mesures, autorisations (voir le fichier).
pub const PAGE_SCRIPT: &str = include_str!("../assets/page.js");

/// Mode de diagnostic (FEATHER_DIAG=1) : affiche sur la page les derniers événements de
/// souris reçus (type, bouton, élément visé), pour comprendre des clics perdus.
pub const DIAG_SCRIPT: &str = r#"
(() => {
  if (window.top !== window) return;
  const lines = [];
  let hud = null;
  const show = () => {
    if (!hud) {
      hud = document.createElement('pre');
      hud.style.cssText = 'position:fixed;right:8px;bottom:8px;z-index:2147483647;margin:0;padding:8px;' +
        'background:rgba(0,0,0,.8);color:#7CFC00;font:11px/1.3 monospace;pointer-events:none;max-width:60vw;overflow:hidden';
      (document.body || document.documentElement).appendChild(hud);
    }
    hud.textContent = lines.join('\n');
  };
  const who = (t) => t && t.tagName ? t.tagName.toLowerCase() + (t.id ? '#' + t.id : '') : '?';
  let moves = 0;
  const log = (e) => {
    if (e.type === 'mousemove') { moves++; if (moves % 20) return; }
    lines.push(e.type + ' b=' + e.button + ' ' + Math.round(e.clientX) + ',' + Math.round(e.clientY) + ' ' + who(e.target) + (e.type === 'mousemove' ? ' (x' + moves + ')' : ''));
    if (lines.length > 12) lines.shift();
    show();
  };
  for (const type of ['pointerdown', 'mousedown', 'mouseup', 'click', 'auxclick', 'contextmenu', 'mousemove']) {
    window.addEventListener(type, log, true);
  }
})();
"#;

/// Messages envoyés par l'interface (JSON `{"type": "...", ...}`).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChromeMessage {
    Ready,
    Navigate {
        input: String,
    },
    NewTab,
    ToggleMute {
        id: TabId,
    },
    MoveTab {
        id: TabId,
        index: usize,
    },
    CloseTab {
        id: Option<TabId>,
    },
    ActivateTab {
        id: TabId,
    },
    /// Un raccourci clavier tapé dans l'interface, traité comme ceux des pages.
    Shortcut {
        shortcut: Shortcut,
    },
    Back,
    Forward,
    Reload,
    Home,
    /// L'interface ouvre (ou ferme) un panneau : elle passe au-dessus de la page
    /// (macOS, Windows) ou la cache (Linux).
    Expand {
        open: bool,
    },
    /// Forme de l'interface pendant qu'un panneau est ouvert : la barre et les panneaux.
    Shape {
        rects: Vec<crate::layers::ShapeRect>,
    },
    /// La barre latérale est élargie ou rétrécie à la souris ; `done` : bouton relâché.
    SidebarWidth {
        width: u32,
        #[serde(default)]
        done: bool,
    },
    /// L'utilisateur a quitté la barre : rendre le focus à la page.
    FocusPage,
    /// Ctrl+L : barre de l'accueil si l'onglet est sur l'accueil, sinon barre par-dessus la page.
    FocusBar,
    SaveSettings {
        settings: Settings,
    },
    CheckUpdate,
    InstallUpdate,
    RestartUpdate,
    OpenReleasePage,
    /// Ajoute aux favoris (ou en retire) un onglet, l'onglet actif par défaut.
    ToggleBookmark {
        id: Option<TabId>,
    },
    /// Le panneau Bibliothèque demande une liste : favoris, historique ou téléchargements.
    Library {
        kind: LibraryKind,
        #[serde(default)]
        query: String,
    },
    /// Ouvre une adresse de la bibliothèque, dans l'onglet actif ou un nouvel onglet.
    OpenUrl {
        url: String,
        #[serde(default)]
        new_tab: bool,
        #[serde(default)]
        private: bool,
    },
    /// Le bandeau du bas s'ouvre ou se ferme : la page doit lui laisser la place.
    Strip {
        open: bool,
    },
    /// Recherche dans la page active (`backwards` : occurrence précédente).
    Find {
        text: String,
        #[serde(default)]
        backwards: bool,
    },
    /// Fin de la recherche dans la page : on efface la sélection.
    FindClose,
    /// Suggestions de la barre (favoris et historique) pour ce texte.
    Suggest {
        query: String,
    },
    /// Commande native de la barre (« /calcul 2+2 ») : aperçu ou exécution.
    Command {
        input: String,
        #[serde(default)]
        run: bool,
    },
    /// Réponse de l'utilisateur à une demande d'autorisation.
    PermissionAnswer {
        request: u32,
        allow: bool,
        #[serde(default)]
        remember: bool,
    },
    /// Oublie une décision retenue (paramètres).
    ForgetPermission {
        host: String,
        permission: Permission,
    },
    /// « Pas de transparence sur ce site » (ou l'inverse), l'onglet actif par défaut.
    ToggleGlassSite {
        id: Option<TabId>,
    },
    /// Met un onglet en veille (menu de l'onglet).
    SleepTab {
        id: TabId,
    },
    /// Annule un téléchargement en cours.
    CancelDownload {
        id: u32,
    },
    RemoveBookmark {
        url: String,
    },
    RemoveVisit {
        url: String,
    },
    ClearHistory,
    RevealDownload {
        id: u32,
    },
    OpenDownload {
        id: u32,
    },
    ClearDownloads,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LibraryKind {
    Bookmarks,
    History,
    Downloads,
}

/// Message envoyé par une page : un raccourci clavier, ou (depuis la page
/// d'accueil seulement, vérifié côté Rust) une saisie de sa barre de recherche.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PageMessage {
    pub shortcut: Option<Shortcut>,
    pub navigate: Option<String>,
    /// Texte commençant par « / » : ouvrir la barre des commandes.
    pub open_bar: Option<String>,
    /// La page joue du son (ou vient d'arrêter).
    pub audible: Option<bool>,
    /// Clic de molette sur un lien : adresse à ouvrir dans un nouvel onglet.
    pub open_tab: Option<String>,
    /// Icône et nom du site (voir `clean_site`).
    pub site: Option<SiteInfo>,
    /// Résultat d'une recherche dans la page.
    pub find_result: Option<FindResult>,
    /// Choix fait dans le menu d'un lien (clic droit).
    pub link: Option<LinkAction>,
    /// Mesures de la page (voir `weight.rs`).
    pub metrics: Option<PageMetrics>,
    /// Texte lisible de la page, pour la recherche dans l'historique.
    pub text: Option<String>,
    /// La page demande une autorisation (caméra, position…).
    pub permission: Option<PermissionRequest>,
    /// La page est prête : elle demande si son fond doit être en verre.
    pub glass: bool,
    /// Clic dans la page : un panneau ouvert par-dessus se ferme.
    pub pressed: bool,
}

#[derive(Debug, Default, Deserialize, serde::Serialize, Clone, Copy)]
#[serde(default)]
pub struct FindResult {
    pub current: u32,
    pub total: u32,
}

#[derive(Debug, Deserialize)]
pub struct LinkAction {
    pub url: String,
    pub how: LinkHow,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkHow {
    /// Nouvel onglet affiché.
    Tab,
    /// Nouvel onglet en arrière-plan.
    Background,
    /// Onglet privé.
    Private,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PageMetrics {
    pub heap_mb: Option<u32>,
    pub lag_ms: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct PermissionRequest {
    /// Numéro donné par la page, rendu avec la réponse.
    pub id: u32,
    pub permission: Permission,
}

/// Ce que la page dit d'elle-même : l'adresse de son icône et le nom du site.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SiteInfo {
    pub icon: Option<String>,
    pub name: Option<String>,
}

/// Nettoie ce qu'une page envoie : l'icône doit être une adresse http(s) ou une
/// petite image intégrée, et le nom un texte court sans caractères de contrôle.
pub fn clean_site(info: SiteInfo) -> (Option<String>, Option<String>) {
    let icon = info.icon.filter(|i| {
        (crate::network::is_web(i) && i.len() <= 2048) || (i.starts_with("data:image/") && i.len() <= 32_768)
    });
    let name = info
        .name
        .map(|n| {
            n.chars()
                .filter(|c| !c.is_control())
                .take(60)
                .collect::<String>()
        })
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    (icon, name)
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Shortcut {
    FocusBar,
    Back,
    Forward,
    Reload,
    NewTab,
    CloseTab,
    ToggleMute,
    NextTab,
    PreviousTab,
    Settings,
    ToggleBookmark,
    Find,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ReopenTab,
    Print,
    NewPrivateTab,
}

/// Zone occupée par les pages : tout sauf la barre (à gauche, à droite ou en
/// haut) et, s'il est ouvert, le bandeau du bas.
/// `side` : largeur de la barre quand elle est à gauche ou à droite.
pub fn page_bounds(window: &Window, bar: BarPosition, side: u32, strip: bool) -> Rect {
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    let strip = if strip { STRIP_HEIGHT } else { 0.0 };
    // La barre ne prend jamais plus de la moitié de la fenêtre.
    let side = f64::from(side).clamp(SIDEBAR_WIDTH, (size.width / 2.0).max(SIDEBAR_WIDTH));
    let (x, y, w, h) = match bar {
        BarPosition::Left => (side, 0.0, size.width - side, size.height - strip),
        BarPosition::Right => (0.0, 0.0, size.width - side, size.height - strip),
        BarPosition::Top => (0.0, TOP_BAR_HEIGHT, size.width, size.height - TOP_BAR_HEIGHT - strip),
    };
    Rect {
        position: LogicalPosition::new(x, y).into(),
        size: LogicalSize::new(w.max(1.0), h.max(1.0)).into(),
    }
}

/// Zone de l'interface : toute la fenêtre (elle est dessinée sous les pages).
pub fn chrome_bounds(window: &Window) -> Rect {
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: LogicalSize::new(size.width, size.height).into(),
    }
}

/// Génère la page d'accueil (heure, date, bienvenue).
pub fn home_page(settings: &Settings, bookmarks: &[Bookmark]) -> String {
    let theme = match settings.theme {
        Theme::Auto => "auto",
        Theme::Light => "light",
        Theme::Dark => "dark",
    };
    fill_template(
        HOME_HTML,
        &[
            ("THEME", theme.to_string()),
            ("NAME", escape_html(&settings.name)),
            ("BOOKMARKS", bookmark_tiles(bookmarks)),
        ],
    )
}

/// Remplace les `{{CLÉ}}` du modèle en une seule passe. Avec des `replace`
/// enchaînés, un prénom contenant « {{BOOKMARKS}} » était lui-même remplacé
/// par les favoris : le texte inséré n'est plus jamais relu ici.
fn fill_template(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let value = after.find("}}").and_then(|end| {
            let key = &after[..end];
            values.iter().find(|(k, _)| *k == key).map(|(_, v)| (v, end))
        });
        match value {
            Some((v, end)) => {
                out.push_str(v);
                rest = &after[end + 2..];
            }
            None => {
                // Pas une clé connue : on garde le texte tel quel.
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Les favoris de la page d'accueil : des vignettes avec l'icône du site (ou son initiale).
fn bookmark_tiles(bookmarks: &[Bookmark]) -> String {
    let mut html = String::new();
    for b in bookmarks.iter().take(12) {
        let icon = match &b.icon {
            Some(src) => format!(r#"<img src="{}" alt="" referrerpolicy="no-referrer">"#, escape_html(src)),
            None => escape_html(&b.title.chars().next().unwrap_or('?').to_uppercase().to_string()),
        };
        html.push_str(&format!(
            r#"<a class="fav" href="{}" title="{}"><span class="fi">{}</span><span class="ft">{}</span></a>"#,
            escape_html(&b.url),
            escape_html(&b.title),
            icon,
            escape_html(&b.title)
        ));
    }
    if html.is_empty() {
        html
    } else {
        format!(r#"<nav id="favs">{html}</nav>"#)
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Où l'on accroche les webviews dans la fenêtre.
///
/// Sur Windows et macOS, chaque webview est une fenêtre enfant. Sur Linux, il
/// faut un conteneur GTK (`gtk::Fixed`) dans lequel on peut les positionner.
/// Les webviews créées en dernier passent au-dessus : l'interface est créée
/// en premier pour rester sous les pages.
pub struct Surface {
    #[cfg(target_os = "linux")]
    fixed: gtk::Fixed,
}

impl Surface {
    #[cfg(target_os = "linux")]
    pub fn new(window: &Window) -> Self {
        use gtk::prelude::*;
        use tao::platform::unix::WindowExtUnix;
        let fixed = gtk::Fixed::new();
        let vbox = window.default_vbox().expect("fenêtre GTK sans conteneur");
        vbox.pack_start(&fixed, true, true, 0);
        fixed.show_all();
        Self { fixed }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new(_window: &Window) -> Self {
        Self {}
    }

    #[cfg(target_os = "linux")]
    pub fn attach(&self, builder: WebViewBuilder, _window: &Window) -> wry::Result<WebView> {
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(&self.fixed)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn attach(&self, builder: WebViewBuilder, window: &Window) -> wry::Result<WebView> {
        builder.build_as_child(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_interface() {
        let m: ChromeMessage =
            serde_json::from_str(r#"{"type":"navigate","input":"rust"}"#).unwrap();
        assert!(matches!(m, ChromeMessage::Navigate { input } if input == "rust"));
        let m: ChromeMessage = serde_json::from_str(r#"{"type":"close_tab","id":null}"#).unwrap();
        assert!(matches!(m, ChromeMessage::CloseTab { id: None }));
        assert!(serde_json::from_str::<ChromeMessage>(r#"{"type":"supprimer_tout"}"#).is_err());
        let m: ChromeMessage =
            serde_json::from_str(r#"{"type":"shortcut","shortcut":"next_tab"}"#).unwrap();
        assert!(matches!(m, ChromeMessage::Shortcut { shortcut: Shortcut::NextTab }));
    }

    #[test]
    fn nettoyage_icone_et_nom() {
        let site = |icon: &str, name: &str| SiteInfo {
            icon: Some(icon.into()),
            name: Some(name.into()),
        };
        let (icon, name) = clean_site(site("https://exemple.fr/favicon.ico", "  Mon Blog\n"));
        assert_eq!(icon.as_deref(), Some("https://exemple.fr/favicon.ico"));
        assert_eq!(name.as_deref(), Some("Mon Blog"));
        assert_eq!(clean_site(site("javascript:alert(1)", "x")).0, None);
        assert_eq!(clean_site(site("file:///etc/passwd", "x")).0, None);
        assert!(
            clean_site(site("data:image/png;base64,AAAA", "x"))
                .0
                .is_some()
        );
        let long = "a".repeat(200);
        assert_eq!(
            clean_site(site("https://a.fr/i.png", &long))
                .1
                .unwrap()
                .len(),
            60
        );
        assert_eq!(clean_site(site("https://a.fr/i.png", "  ")).1, None);
    }

    #[test]
    fn accueil_echappe_le_nom() {
        let s = Settings {
            name: "<b>Léa</b>".into(),
            ..Default::default()
        };
        let html = home_page(&s, &[]);
        assert!(html.contains("&lt;b&gt;Léa&lt;/b&gt;"));
        assert!(!html.contains("{{NAME}}"));
    }

    #[test]
    fn modele_en_une_passe() {
        // Le prénom n'est pas relu comme un modèle.
        let s = Settings {
            name: "{{BOOKMARKS}}".into(),
            ..Default::default()
        };
        let html = home_page(&s, &[]);
        assert!(html.contains(r#"data-name="{{BOOKMARKS}}""#));
        assert_eq!(
            fill_template("a {{X}} b {{Y}} {{", &[("X", "1".into())]),
            "a 1 b {{Y}} {{"
        );
    }
}
