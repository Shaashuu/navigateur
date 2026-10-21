//! L'interface du navigateur (la « chrome ») : une webview HTML/CSS placée
//! sous les pages, qui dessine la colonne de gauche (paramètres, pastille de
//! recherche, onglets) et la barre de recherche qui s'ouvre par-dessus la page.
//! Ce module définit aussi les messages échangés avec elle.

use serde::Deserialize;
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::window::Window;
use wry::{Rect, WebView, WebViewBuilder};

use crate::library::Bookmark;
use crate::library::Visit;
use crate::settings::{BarPosition, Settings, SidebarStyle, Theme};
use crate::sitedata::Permission;
use crate::tab_manager::TabId;

/// Largeur la plus étroite de la colonne des pastilles (pixels logiques).
pub const SIDEBAR_WIDTH: f64 = crate::settings::SIDEBAR_MIN as f64;
/// Hauteur de la barre quand elle est placée en haut.
pub const TOP_BAR_HEIGHT: f64 = 44.0;
/// Hauteur du bandeau du bas (recherche dans la page, question d'autorisation,
/// fin d'un minuteur) : la page rétrécit pour le laisser voir.
pub const STRIP_HEIGHT: f64 = 48.0;
/// Hauteur du bandeau d'adresse (style « bandeau » de la barre latérale).
pub const BANNER_HEIGHT: f64 = 44.0;
/// Marge autour de la page : elle est posée comme une carte sur le fond.
pub const PAGE_GAP: f64 = 8.0;
/// Mode concentration : il ne reste qu'un mince rail d'onglets.
pub const FOCUS_RAIL: f64 = 14.0;

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
        /// Recherche de la pastille à la loupe, réglée sur « nouvel onglet ».
        #[serde(default)]
        new_tab: bool,
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
    /// L'interface a besoin de savoir où est la souris même hors d'elle (la
    /// poignée de la barre est survolée) : Rust la lui envoie (`navigateur.pointer`).
    WatchPointer {
        on: bool,
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
    /// Met un onglet en veille (menu de l'onglet, gestionnaire des onglets).
    SleepTab {
        id: TabId,
    },
    /// Met en veille tous les onglets cachés (gestionnaire des onglets).
    SleepHidden,
    /// Le panneau de la mémoire s'ouvre ou se ferme : tant qu'il est ouvert, la
    /// mémoire affichée suit celle des onglets.
    WatchMemory {
        on: bool,
    },
    /// Change une autorisation retenue pour un site (fiche du site).
    SetPermission {
        host: String,
        permission: Permission,
        allow: bool,
    },
    /// Efface les cookies et le stockage du site de l'onglet actif (fiche du site).
    ClearSiteData,
    /// Active ou coupe le bloqueur de publicités sur un site (fiche du site).
    SetSiteAds {
        host: String,
        on: bool,
    },
    /// Télécharge de nouveau les listes du bloqueur (paramètres).
    UpdateAdLists,
    /// Épingle un onglet, ou le détache.
    PinTab {
        id: TabId,
        pinned: bool,
    },
    /// Range un onglet dans un groupe existant (`None` : le sort de son groupe).
    GroupTab {
        id: TabId,
        group: Option<u32>,
    },
    /// Crée un groupe avec cet onglet.
    NewGroup {
        id: TabId,
        name: String,
    },
    RenameGroup {
        group: u32,
        name: String,
    },
    /// Défait un groupe (ses onglets restent ouverts).
    Ungroup {
        group: u32,
    },
    /// Ferme tous les onglets d'un groupe.
    CloseGroup {
        group: u32,
    },
    /// Replie (ses onglets passent en veille) ou déplie un groupe.
    ToggleGroup {
        group: u32,
    },
    /// Ouvre la page de l'onglet actif en mode lecture (ou en sort).
    Reader,
    /// Liste des navigateurs dont on peut importer les favoris.
    ImportSources,
    /// Importe les favoris des navigateurs choisis et des fichiers d'export
    /// (contenu HTML lu par l'interface).
    Import {
        #[serde(default)]
        sources: Vec<String>,
        #[serde(default)]
        files: Vec<String>,
    },
    /// Installe une extension depuis un lien (Chrome Web Store, Firefox Add-ons).
    InstallExtension {
        input: String,
    },
    /// Installe une extension depuis un fichier choisi (`data:` URL).
    InstallExtensionFile {
        name: String,
        data: String,
    },
    /// Active ou désactive une extension.
    SetExtension {
        id: String,
        enabled: bool,
    },
    RemoveExtension {
        id: String,
    },
    /// Ouvre la page d'options d'une extension.
    ExtensionOptions {
        id: String,
    },
    /// Clic sur le bouton d'une extension : sa petite fenêtre, contre ce rectangle.
    ExtensionAction {
        id: String,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    },
    CloseExtensionPopup,
    /// Le panneau a montré l'erreur d'installation : on l'oublie.
    ClearExtensionError,
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
    /// Choix fait dans le menu d'une image (clic droit).
    pub image: Option<ImageAction>,
    /// Mesures de la page (voir `weight.rs`).
    pub metrics: Option<PageMetrics>,
    /// Texte lisible de la page, pour la recherche dans l'historique.
    pub text: Option<String>,
    /// La page demande une autorisation (caméra, position…).
    pub permission: Option<PermissionRequest>,
    /// Clic dans la page : un panneau ouvert par-dessus se ferme.
    pub pressed: bool,
    /// La page ressemble à un article (mode lecture possible).
    pub readable: Option<bool>,
    /// Article extrait pour le mode lecture (voir `reader.rs`).
    pub reader: Option<crate::reader::Article>,
    /// L'extraction n'a trouvé aucun article.
    pub reader_failed: bool,
    /// `#id` et `.classes` présents dans la page, pour le masquage des publicités.
    pub cosmetic: Option<CosmeticRequest>,
    /// Bouton « Ajouter à Feather » sur le Chrome Web Store : adresse de la
    /// page de l'extension (vérifiée côté Rust, voir `extensions::store_page`).
    pub store_install: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CosmeticRequest {
    pub tokens: Vec<String>,
    /// Premier envoi de la page (elle reçoit aussi les règles propres au site).
    pub first: bool,
}

#[derive(Debug, Default, Deserialize, serde::Serialize, Clone, Copy)]
#[serde(default)]
pub struct FindResult {
    pub current: u32,
    pub total: u32,
}

#[derive(Debug, Deserialize)]
pub struct ImageAction {
    pub url: String,
    pub how: ImageHow,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageHow {
    /// Dans le dossier des téléchargements.
    Save,
    /// Dans l'app Photos (macOS).
    Photos,
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
    /// Maj+Échap : gestionnaire des onglets (mémoire et processeur).
    TaskManager,
    /// Ctrl+Maj+S : capture de la page visible.
    Capture,
    /// Ctrl+Maj+F : mode concentration (la barre se réduit à un rail).
    Focus,
}

/// Ce qui entoure la page : la barre (position, style, largeur), le bandeau du
/// bas et le mode concentration.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub bar: BarPosition,
    pub style: SidebarStyle,
    /// Largeur de la barre quand elle est à gauche ou à droite.
    pub side: u32,
    pub strip: bool,
    pub focus: bool,
}

/// Zone des pages (x, y, largeur, hauteur) dans une fenêtre de cette taille.
pub fn page_area(width: f64, height: f64, f: &Frame) -> (f64, f64, f64, f64) {
    let bottom = if f.strip { STRIP_HEIGHT } else { PAGE_GAP };
    if f.focus {
        // Le rail reste du côté de la barre (à gauche quand elle est en haut).
        let x = if f.bar == BarPosition::Right { PAGE_GAP / 2.0 } else { FOCUS_RAIL };
        let bottom = if f.strip { STRIP_HEIGHT } else { PAGE_GAP / 2.0 };
        return (x, PAGE_GAP / 2.0, width - FOCUS_RAIL - PAGE_GAP / 2.0, height - PAGE_GAP / 2.0 - bottom);
    }
    if f.bar == BarPosition::Top {
        return (PAGE_GAP, TOP_BAR_HEIGHT, width - 2.0 * PAGE_GAP, height - TOP_BAR_HEIGHT - bottom);
    }
    let side = if f.style == SidebarStyle::Wide { f.side.max(crate::settings::WIDE_MIN) } else { f.side };
    // La barre ne prend jamais plus de la moitié de la fenêtre.
    let side = f64::from(side).clamp(SIDEBAR_WIDTH, (width / 2.0).max(SIDEBAR_WIDTH));
    let top = if f.style == SidebarStyle::Banner { BANNER_HEIGHT } else { PAGE_GAP };
    let x = if f.bar == BarPosition::Right { PAGE_GAP } else { side };
    (x, top, width - side - PAGE_GAP, height - top - bottom)
}

/// Zone occupée par les pages : tout sauf la barre, ses bandeaux et la marge autour.
pub fn page_bounds(window: &Window, frame: &Frame) -> Rect {
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    let (x, y, w, h) = page_area(size.width, size.height, frame);
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
pub fn home_page(settings: &Settings, bookmarks: &[Bookmark], recent: &[&Visit]) -> String {
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
            ("RECENT", recent_cards(recent)),
        ],
    )
}

/// Remplace les `{{CLÉ}}` du modèle en une seule passe. Avec des `replace`
/// enchaînés, un prénom contenant « {{BOOKMARKS}} » était lui-même remplacé
/// par les favoris : le texte inséré n'est plus jamais relu ici.
pub(crate) fn fill_template(template: &str, values: &[(&str, String)]) -> String {
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

/// « Reprendre » : les dernières pages visitées (une fois chacune), sous les favoris.
fn recent_cards(recent: &[&Visit]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    let mut html = String::new();
    for v in recent {
        if seen.contains(&v.url.as_str()) || seen.len() == 4 {
            continue;
        }
        seen.push(&v.url);
        let host = url::Url::parse(&v.url).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string())).unwrap_or_default();
        let title = if v.title.is_empty() { &v.url } else { &v.title };
        html.push_str(&format!(
            r#"<a class="rc" href="{}"><span class="rt">{}</span><span class="rh">{}</span></a>"#,
            escape_html(&v.url),
            escape_html(title),
            escape_html(&host)
        ));
    }
    if html.is_empty() {
        html
    } else {
        format!(r#"<section id="recent"><h2>Reprendre</h2><div class="rcs">{html}</div></section>"#)
    }
}

pub(crate) fn escape_html(text: &str) -> String {
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
    fn zone_de_la_page() {
        let f = |bar, style, side, strip, focus| Frame { bar, style, side, strip, focus };
        let (l, t) = (BarPosition::Left, SidebarStyle::Palette);
        // Barre à gauche : la page est posée en carte, avec une marge autour.
        assert_eq!(page_area(1000.0, 800.0, &f(l, t, 56, false, false)), (56.0, 8.0, 936.0, 784.0));
        // Bandeau d'adresse au-dessus de la page.
        assert_eq!(page_area(1000.0, 800.0, &f(l, SidebarStyle::Banner, 56, false, false)), (56.0, 44.0, 936.0, 748.0));
        // Barre large : au moins 240 px.
        assert_eq!(page_area(1000.0, 800.0, &f(l, SidebarStyle::Wide, 56, false, false)).0, 240.0);
        // À droite, avec le bandeau du bas.
        assert_eq!(page_area(1000.0, 800.0, &f(BarPosition::Right, t, 100, true, false)), (8.0, 8.0, 892.0, 744.0));
        // En haut.
        assert_eq!(page_area(1000.0, 800.0, &f(BarPosition::Top, t, 56, false, false)), (8.0, 44.0, 984.0, 748.0));
        // Mode concentration : un rail de 14 px.
        assert_eq!(page_area(1000.0, 800.0, &f(l, SidebarStyle::Wide, 300, false, true)), (14.0, 4.0, 982.0, 792.0));
    }

    #[test]
    fn messages_interface() {
        let m: ChromeMessage =
            serde_json::from_str(r#"{"type":"navigate","input":"rust"}"#).unwrap();
        assert!(matches!(m, ChromeMessage::Navigate { input, new_tab: false } if input == "rust"));
        let m: ChromeMessage =
            serde_json::from_str(r#"{"type":"navigate","input":"rust","new_tab":true}"#).unwrap();
        assert!(matches!(m, ChromeMessage::Navigate { new_tab: true, .. }));
        let m: ChromeMessage = serde_json::from_str(r#"{"type":"close_tab","id":null}"#).unwrap();
        assert!(matches!(m, ChromeMessage::CloseTab { id: None }));
        assert!(serde_json::from_str::<ChromeMessage>(r#"{"type":"supprimer_tout"}"#).is_err());
        let m: ChromeMessage =
            serde_json::from_str(r#"{"type":"shortcut","shortcut":"next_tab"}"#).unwrap();
        assert!(matches!(m, ChromeMessage::Shortcut { shortcut: Shortcut::NextTab }));
        let m: ChromeMessage = serde_json::from_str(
            r#"{"type":"set_permission","host":"meet.jit.si","permission":"camera","allow":true}"#,
        )
        .unwrap();
        assert!(matches!(m, ChromeMessage::SetPermission { allow: true, .. }));
        assert!(matches!(
            serde_json::from_str::<ChromeMessage>(r#"{"type":"sleep_hidden"}"#).unwrap(),
            ChromeMessage::SleepHidden
        ));
        assert!(matches!(
            serde_json::from_str::<ChromeMessage>(r#"{"type":"watch_memory","on":true}"#).unwrap(),
            ChromeMessage::WatchMemory { on: true }
        ));
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
        let html = home_page(&s, &[], &[]);
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
        let html = home_page(&s, &[], &[]);
        assert!(html.contains(r#"data-name="{{BOOKMARKS}}""#));
        assert_eq!(
            fill_template("a {{X}} b {{Y}} {{", &[("X", "1".into())]),
            "a 1 b {{Y}} {{"
        );
    }
}
