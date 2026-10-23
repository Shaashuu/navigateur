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

/// Menu Aide (macOS) : où signaler un problème.
pub const ISSUES_URL: &str = "https://github.com/Shaashuu/feather-releases/issues";

pub const CHROME_HTML: &str = include_str!("../assets/chrome.html");
const HOME_HTML: &str = include_str!("../assets/home.html");

/// Injecté dans chaque page : raccourcis clavier, son, icône du site, recherche
/// dans la page, menu des liens, mesures, autorisations (voir le fichier).
const PAGE_SCRIPT: &str = include_str!("../assets/page.js");

/// Le script des pages, avec la clé de l'onglet (voir `split_key`).
pub fn page_script(key: &str) -> String {
    PAGE_SCRIPT.replacen("__FEATHER_KEY__", key, 1)
}

/// Sépare la clé de l'onglet du message d'une page. `true` : le message vient
/// bien du script de Feather (`assets/page.js`), pas d'un script du site, qui ne
/// connaît pas la clé. Sans elle, il ne garde que `PageMessage::from_untrusted`.
pub fn split_key<'a>(raw: &'a str, key: &str) -> (bool, &'a str) {
    match raw.strip_prefix(key) {
        Some(rest) if !key.is_empty() => (true, rest),
        _ => (false, raw),
    }
}

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
    /// Plein écran : la barre a fini de s'envoler (`true`), la page prend toute la fenêtre.
    BarHidden {
        hidden: bool,
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
    /// « Toujours actif » pour un site : ses onglets ne sont jamais mis en veille.
    SetSiteAwake {
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
    /// Réponse à la fiche d'une extension téléchargée : l'installer ou non.
    ConfirmExtension {
        accept: bool,
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
    /// Texte tapé dans la recherche de l'accueil : il passe dans la barre
    /// (« / » : ses commandes ; sinon les suggestions).
    pub open_bar: Option<String>,
    /// Nouvel ordre des vignettes de favoris, après un glisser-déposer sur l'accueil.
    pub reorder_bookmarks: Option<Vec<String>>,
    /// La page joue du son (ou vient d'arrêter).
    pub audible: Option<bool>,
    /// Envoyé avec `audible` : une vidéo de la page joue, mais sans son (voir `Tab::busy`).
    pub quiet: Option<bool>,
    /// Un champ de la page a été modifié (formulaire, message en cours d'écriture).
    pub dirty: Option<bool>,
    /// La page filme ou enregistre (caméra, micro, écran).
    pub capturing: Option<bool>,
    /// Clic de molette sur un lien : adresse à ouvrir dans un nouvel onglet.
    pub open_tab: Option<String>,
    /// Clic sur un lien qui quitte la page (`true`), ou lien resté sans suite (`false`).
    pub leaving: Option<bool>,
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
    /// Position dans la page, envoyée quand l'onglet est caché (voir `Tab::scroll`).
    pub scroll: Option<ScrollPosition>,
}

/// Position dans une page, en pixels CSS.
#[derive(Debug, Default, Deserialize, Clone, Copy)]
#[serde(default)]
pub struct ScrollPosition {
    pub x: f64,
    pub y: f64,
}

impl PageMessage {
    /// Ce qu'on garde d'un message venu d'un cadre d'un autre site que la page
    /// (publicité, lecteur intégré) : le clic de molette ou le menu d'un lien
    /// (ouvertures limitées comme celles des pages) et le clic qui ferme un
    /// panneau. Le reste (raccourcis, mode lecture, images, son, autorisations…)
    /// n'appartient qu'à la page : avant, un cadre pouvait fermer l'onglet ou
    /// faire télécharger une adresse du réseau local.
    pub fn from_other_frame(self) -> Self {
        Self { open_tab: self.open_tab, link: self.link, pressed: self.pressed, ..Self::default() }
    }

    /// Ce qu'on garde d'un message sans la clé de l'onglet, donc écrit par un script
    /// du site ou par un script que Feather lance à la demande : la barre de
    /// recherche et les vignettes de l'accueil (vérifiées ensuite : seulement sur l'accueil) et
    /// l'article du mode lecture (seulement après un clic sur le bouton livre).
    pub fn from_untrusted(self) -> Self {
        Self {
            navigate: self.navigate,
            open_bar: self.open_bar,
            reorder_bookmarks: self.reorder_bookmarks,
            reader: self.reader,
            reader_failed: self.reader_failed,
            ..Self::default()
        }
    }
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
    /// Ctrl+1 à Ctrl+8 : l'onglet à cette place ; Ctrl+9 : le dernier.
    Tab1,
    Tab2,
    Tab3,
    Tab4,
    Tab5,
    Tab6,
    Tab7,
    Tab8,
    Tab9,
    /// Ctrl+H (⌘Y sous macOS) : l'historique.
    History,
    /// Ctrl+J (⌥⌘L sous macOS) : les téléchargements.
    Downloads,
    /// Ctrl+Maj+O (⌥⌘B sous macOS) : les favoris.
    Bookmarks,
    /// F11 : plein écran (⌃⌘F sous macOS, par le menu Présentation).
    Fullscreen,
    /// Menu Fichier (macOS) : importer des favoris d'un autre navigateur.
    Import,
    /// Menu Fenêtre (macOS) : la fenêtre des extensions.
    Extensions,
    /// Menus macOS : mode lecture (⇧⌘R).
    Reader,
    /// Menu Historique (macOS) : page d'accueil dans l'onglet affiché (⇧⌘H).
    Home,
    /// Menu Onglet (macOS) : épingler ou détacher l'onglet affiché.
    PinTab,
    /// Menu Onglet (macOS) : le même site dans un nouvel onglet.
    DuplicateTab,
    /// Menu Onglet (macOS) : ferme les autres onglets (sauf les épinglés).
    CloseOtherTabs,
    /// Menu Onglet (macOS) : ranger l'onglet affiché dans un groupe.
    GroupTab,
    /// Menu Édition (macOS) : occurrence suivante (⌘G) ou précédente (⇧⌘G).
    FindNext,
    FindPrevious,
    /// Menu Favoris (macOS) : tous les onglets dans les favoris (⇧⌘D).
    BookmarkAll,
    /// Menu Aide (macOS) : la page des problèmes signalés de Feather.
    ReportIssue,
    /// Menu Fichier (macOS) : archive de la page (⌘S), PDF, fichier à
    /// ouvrir (⌘O), menu de partage.
    SavePage,
    ExportPdf,
    OpenFile,
    Share,
}

impl Shortcut {
    /// Place de l'onglet visé par Ctrl+1 à Ctrl+9 (9 : le dernier).
    pub fn tab_number(self) -> Option<usize> {
        Some(match self {
            Self::Tab1 => 1,
            Self::Tab2 => 2,
            Self::Tab3 => 3,
            Self::Tab4 => 4,
            Self::Tab5 => 5,
            Self::Tab6 => 6,
            Self::Tab7 => 7,
            Self::Tab8 => 8,
            Self::Tab9 => 9,
            _ => return None,
        })
    }
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
    /// Plein écran, barre envolée : la page prend toute la fenêtre (sauf le bandeau du bas).
    pub full: bool,
}

/// Zone des pages (x, y, largeur, hauteur) dans une fenêtre de cette taille.
pub fn page_area(width: f64, height: f64, f: &Frame) -> (f64, f64, f64, f64) {
    if f.full {
        let bottom = if f.strip { STRIP_HEIGHT } else { 0.0 };
        return (0.0, 0.0, width, height - bottom);
    }
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
            ("MOTION", settings.animations.effective(settings.economy).to_string()),
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
        let f = |bar, style, side, strip, focus| Frame { bar, style, side, strip, focus, full: false };
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
        // Plein écran, barre envolée : toute la fenêtre, sauf le bandeau du bas.
        let full = Frame { full: true, ..f(BarPosition::Top, t, 56, true, false) };
        assert_eq!(page_area(1000.0, 800.0, &full), (0.0, 0.0, 1000.0, 752.0));
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
        let m: ChromeMessage = serde_json::from_str(r#"{"type":"shortcut","shortcut":"tab9"}"#).unwrap();
        assert!(matches!(m, ChromeMessage::Shortcut { shortcut: Shortcut::Tab9 }));
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
    fn cle_des_pages() {
        let key = "0123456789abcdef0123456789abcdef";
        let script = page_script(key);
        assert!(script.contains(&format!("const KEY = '{key}';")) && !script.contains("__FEATHER_KEY__"));
        assert_eq!(split_key(&format!("{key}{{\"pressed\":true}}"), key), (true, "{\"pressed\":true}"));
        // Message du site : pas de clé, ou une autre.
        assert_eq!(split_key("{\"pressed\":true}", key), (false, "{\"pressed\":true}"));
        assert!(!split_key("fedcba9876543210fedcba9876543210{}", key).0);
        assert!(!split_key("{}", "").0);
        let m: PageMessage = serde_json::from_str(
            r#"{"shortcut":"close_tab","link":{"url":"https://a.fr/","how":"tab"},"pressed":true,
                "navigate":"rust","reader_failed":true,"text":"abc"}"#,
        )
        .unwrap();
        let m = m.from_untrusted();
        assert!(m.shortcut.is_none() && m.link.is_none() && !m.pressed && m.text.is_none());
        assert!(m.navigate.as_deref() == Some("rust") && m.reader_failed);
    }

    #[test]
    fn message_d_un_autre_cadre() {
        let m: PageMessage = serde_json::from_str(
            r#"{"shortcut":"close_tab","image":{"url":"http://192.168.1.1/","how":"save"},"audible":true,
                "open_tab":"https://exemple.fr/","pressed":true,"reader_failed":true}"#,
        )
        .unwrap();
        let m = m.from_other_frame();
        assert!(m.shortcut.is_none() && m.image.is_none() && m.audible.is_none() && !m.reader_failed);
        assert_eq!(m.open_tab.as_deref(), Some("https://exemple.fr/"));
        assert!(m.pressed);
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
