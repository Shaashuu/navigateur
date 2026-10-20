// En mode release sur Windows, pas de console noire derrière la fenêtre.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Feather : une fenêtre tao, une webview pour l'interface, et une
//! webview par onglet. Tout tourne sur le thread principal ; les webviews
//! communiquent avec le code Rust par des messages (`UserEvent`).
//!
//! Ce fichier contient le cœur (onglets, navigation, état de l'interface) ;
//! `features.rs` ajoute la recherche dans la page, le zoom, les autorisations,
//! les commandes natives, la mise en veille et le suivi des téléchargements.

mod commands;
mod downloads;
mod errorpage;
mod features;
mod fulltext;
mod layers;
mod library;
#[cfg(target_os = "macos")]
mod mac_mouse;
mod network;
mod settings;
mod siteclear;
mod sitedata;
mod softsleep;
mod storage;
mod tab_manager;
mod trackers;
mod ui;
mod update;
mod weight;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Icon, Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

use commands::Notes;
use downloads::Download;
use features::PendingPermission;
use fulltext::FullText;
use library::{Library, Session};
use settings::{Settings, Startup};
use sitedata::SiteData;
use tab_manager::{PageContext, TabId, TabManager};
use ui::{ChromeMessage, LibraryKind, LinkHow, PageMessage, Shortcut, Surface};
use weight::Sampler;

/// Une page peut ouvrir au plus ce nombre d'onglets (clic de molette)…
const PAGE_TAB_LIMIT: usize = 10;
/// … sur cette durée : au-delà, une page malveillante pourrait en ouvrir sans fin.
const PAGE_TAB_WINDOW: Duration = Duration::from_secs(10);

/// Événements envoyés par les webviews à la boucle principale.
#[derive(Debug)]
pub enum UserEvent {
    /// Message JSON venant de l'interface.
    Chrome(String),
    /// Message JSON venant d'une page (raccourci clavier, barre de l'accueil).
    Page(TabId, String),
    Title(TabId, String),
    /// Début (`true`) ou fin (`false`) de chargement, avec l'adresse.
    PageLoad(TabId, bool, String),
    /// Le chargement de cette adresse a échoué : on affiche une page d'erreur Feather.
    LoadFailed { id: TabId, url: String, kind: errorpage::ErrorKind },
    /// `window.open` ; `private` : la page qui l'a demandé est privée.
    OpenInNewTab { url: String, private: bool },
    Update(UpdateEvent),
    /// Les onglets fermés peuvent être détruits.
    Reap,
    /// Un téléchargement commence, rangé à cet endroit.
    DownloadStarted { url: String, path: PathBuf, private: bool },
    DownloadDone { url: String, path: Option<PathBuf>, success: bool },
    /// Taille annoncée par le serveur pour un téléchargement.
    DownloadSize { id: u32, total: u64 },
    /// Moment d'enregistrer l'historique, les favoris et la session.
    Persist,
    /// Chaque seconde : progression des téléchargements, poids et veille des onglets.
    Tick,
    /// Un minuteur (`/minuteur`) est arrivé à son terme.
    TimerDone { seconds: u64, label: String },
}

/// Résultat d'un travail de mise à jour fait en arrière-plan.
#[derive(Debug)]
pub enum UpdateEvent {
    Checked { found: Result<Option<update::Available>, String>, manual: bool },
    Installed(Result<update::Restart, String>),
}

/// Où en est la mise à jour (affiché dans les paramètres).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available { info: update::Available },
    Downloading { info: update::Available },
    Ready { info: update::Available },
    Error { message: String },
}

struct App {
    window: Window,
    surface: Surface,
    chrome: WebView,
    tabs: TabManager,
    settings: Arc<RwLock<Settings>>,
    proxy: EventLoopProxy<UserEvent>,
    /// Un panneau de l'interface (commandes, paramètres) recouvre la page.
    expanded: bool,
    /// L'interface passe au-dessus des pages quand un panneau est ouvert.
    layers: layers::Layers,
    /// Le bandeau du bas (recherche, question, minuteur) est ouvert : la page rétrécit.
    strip: bool,
    update: UpdateState,
    /// Programme à relancer quand la mise à jour est installée.
    restart: Option<update::Restart>,
    library: Arc<RwLock<Library>>,
    /// Zoom et autorisations retenus par site.
    sites: Arc<RwLock<SiteData>>,
    /// Texte des pages visitées (recherche dans l'historique).
    fulltext: FullText,
    /// Notes de la commande `/note`.
    notes: Notes,
    /// Téléchargements de cette session.
    downloads: Vec<Download>,
    /// Prochain numéro de téléchargement (jamais réutilisé, même après « Vider la liste »).
    next_download_id: u32,
    /// Un enregistrement est déjà prévu (voir `schedule_persist`).
    persist_pending: Cell<bool>,
    /// Dernière session écrite sur le disque, pour ne pas la réécrire à l'identique.
    saved_session: RefCell<Option<Session>>,
    /// Heures des derniers onglets ouverts à la demande d'une page (voir `PAGE_TAB_LIMIT`).
    page_opens: VecDeque<Instant>,
    /// Demandes d'autorisation en attente d'une réponse de l'utilisateur.
    permission_requests: HashMap<u32, PendingPermission>,
    next_permission_request: u32,
    /// Mesure du processus de chaque page (macOS).
    sampler: Sampler,
    /// Secondes écoulées (pour ne mesurer le poids des onglets que toutes les 5 s).
    ticks: u64,
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("Feather")
        .with_window_icon(window_icon())
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(LogicalSize::new(480.0, 360.0))
        .build(&event_loop)
        .expect("impossible de créer la fenêtre");

    #[cfg(target_os = "macos")]
    mac_mouse::install(proxy.clone());

    let surface = Surface::new(&window);

    // L'interface est créée en premier pour rester sous les pages.
    let chrome_proxy = proxy.clone();
    let chrome = surface.attach(
        WebViewBuilder::new()
            .with_bounds(ui::chrome_bounds(&window))
            .with_html(ui::CHROME_HTML)
            // Transparente là où les panneaux laissent voir la page (voir `layers.rs`).
            .with_transparent(layers::SUPPORTED)
            .with_devtools(cfg!(debug_assertions))
            .with_ipc_handler(move |request| {
                let _ = chrome_proxy.send_event(UserEvent::Chrome(request.into_body()));
            }),
        &window,
    )?;

    update::clean_leftovers();
    let settings = Settings::load();
    let check_update = settings.auto_update_check;
    let start_url = settings.start_url();
    let restore = settings.startup == Startup::Restore;
    let mut library = Library::load();
    library.prune(settings.history_days);
    let mut fulltext = FullText::load();
    fulltext.prune(settings.history_days);

    // Une seconde : progression des téléchargements, poids et veille des onglets.
    let tick_proxy = proxy.clone();
    std::thread::spawn(move || {
        while tick_proxy.send_event(UserEvent::Tick).is_ok() {
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    let mut app = App {
        window,
        surface,
        chrome,
        tabs: TabManager::default(),
        settings: Arc::new(RwLock::new(settings)),
        proxy,
        expanded: false,
        layers: layers::Layers::default(),
        strip: false,
        update: UpdateState::Idle,
        restart: None,
        library: Arc::new(RwLock::new(library)),
        sites: Arc::new(RwLock::new(SiteData::load())),
        fulltext,
        notes: Notes::load(),
        downloads: Vec::new(),
        next_download_id: 1,
        persist_pending: Cell::new(false),
        saved_session: RefCell::new(None),
        page_opens: VecDeque::new(),
        permission_requests: HashMap::new(),
        next_permission_request: 1,
        sampler: Sampler::default(),
        ticks: 0,
    };
    let session = library::load_session();
    // Adresses données au lancement (« Ouvrir avec Feather », banc de mesure de la
    // mémoire `outils/banc-memoire`) : une par onglet, la première affichée.
    let launch = network::launch_urls(std::env::args().skip(1));
    if let Some((first, others)) = launch.split_first() {
        app.open_tab(first, false);
        for url in others {
            app.open_tab_in_background(url);
        }
    } else if restore && !session.urls.is_empty() {
        // Les onglets restaurés restent en veille : seul celui affiché est chargé.
        for url in &session.urls {
            app.tabs.add_asleep(url, "", false);
        }
        app.tabs.activate_index(session.active, true);
        app.wake_active();
        app.push_state();
    } else {
        app.open_tab(&start_url, false);
    }
    if check_update {
        app.start_check(false);
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                app.persist();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. },
                ..
            } => app.relayout(),
            Event::UserEvent(event) => app.handle(event),
            _ => {}
        }
    });
}

/// Icône de la fenêtre : image 64×64 en RGBA brut (voir packaging/).
fn window_icon() -> Option<Icon> {
    const RGBA: &[u8] = include_bytes!("../packaging/feather-64.rgba");
    Icon::from_rgba(RGBA.to_vec(), 64, 64).ok()
}

impl App {
    fn handle(&mut self, event: UserEvent) {
        match event {
            UserEvent::Chrome(raw) => match serde_json::from_str::<ChromeMessage>(&raw) {
                Ok(message) => self.handle_chrome(message),
                Err(err) => eprintln!("message de l'interface ignoré : {err}"),
            },
            UserEvent::Page(id, raw) => {
                if let Ok(message) = serde_json::from_str::<PageMessage>(&raw) {
                    self.handle_page(id, message);
                }
            }
            UserEvent::Title(id, title) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    // Le titre d'une page d'erreur ne remplace pas celui de la page dans l'historique.
                    if !tab.private && tab.failed.is_none() {
                        if let Ok(mut lib) = self.library.write() {
                            lib.set_title(tab.url(), &title);
                        }
                        self.fulltext.set_title(tab.url(), &title);
                    }
                    tab.title = title;
                }
                self.push_state();
            }
            UserEvent::PageLoad(id, loading, url) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    // Page d'erreur demandée par Feather : l'onglet garde l'adresse en
                    // échec (barre, « Réessayer », session). Une page d'erreur ouverte
                    // autrement (lien d'un site) garde la sienne : rien à usurper.
                    let url = match errorpage::parse(&url) {
                        Some(failed) if tab.failed.as_ref() == Some(&failed) => failed.1,
                        _ => {
                            tab.failed = None;
                            url
                        }
                    };
                    // Autre site : on efface l'icône de l'ancien en attendant celle du nouveau.
                    if loading && network::host_of(tab.url()) != network::host_of(&url) {
                        tab.icon = None;
                        tab.site_name = None;
                    }
                    tab.loading = loading;
                    tab.set_url(url);
                    // Un onglet privé ne laisse aucune trace dans l'historique.
                    if !loading
                        && !tab.private
                        && tab.failed.is_none()
                        && let Ok(mut lib) = self.library.write()
                    {
                        lib.record_visit(tab.url(), &tab.title);
                    }
                    if loading {
                        tab.audible = false;
                        // La page qui posait une question s'en va : la question aussi.
                        self.permission_requests.retain(|_, p| p.tab != id);
                    } else if tab.muted {
                        // Nouvelle page : on garde l'onglet en sourdine.
                        tab.apply_mute();
                    }
                }
                self.apply_zoom(id);
                self.apply_economy(id);
                self.push_state();
            }
            UserEvent::LoadFailed { id, url, kind } => self.load_failed(id, url, kind),
            // window.open : seules les pages web sont acceptées. Avant, une page
            // pouvait ouvrir ainsi un onglet sur file:// (fichiers de l'ordinateur).
            UserEvent::OpenInNewTab { url, private } => {
                if network::is_web(&url) {
                    self.open_tab(&url, private);
                }
            }
            UserEvent::Update(event) => self.handle_update(event),
            UserEvent::Reap => self.tabs.reap(),
            UserEvent::DownloadStarted { url, path, private } => self.download_started(url, path, private),
            UserEvent::DownloadDone { url, path, success } => self.download_done(&url, path, success),
            UserEvent::DownloadSize { id, total } => {
                if let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) {
                    d.total = Some(total);
                }
                self.push_state();
            }
            UserEvent::Persist => {
                self.persist_pending.set(false);
                self.persist();
            }
            UserEvent::Tick => self.tick(),
            UserEvent::TimerDone { seconds, label } => self.timer_done(seconds, &label),
        }
        // L'onglet affiché a pu changer : s'il était en veille, on recharge sa page.
        self.wake_active();
        if !self.permission_requests.is_empty() {
            self.drop_stale_permissions();
        }
    }

    fn handle_chrome(&mut self, message: ChromeMessage) {
        match message {
            ChromeMessage::Ready => {}
            ChromeMessage::CheckUpdate => self.start_check(true),
            ChromeMessage::InstallUpdate => self.start_install(),
            ChromeMessage::RestartUpdate => {
                // On garde `restart` en cas d'échec : le bouton reste utilisable.
                if self.restart.is_some() {
                    self.persist();
                }
                if let Some(restart) = &self.restart {
                    match restart.launch() {
                        Ok(()) => std::process::exit(0),
                        Err(err) => eprintln!("redémarrage impossible : {err}"),
                    }
                }
            }
            ChromeMessage::OpenReleasePage => {
                if let UpdateState::Available { info } | UpdateState::Ready { info } = &self.update {
                    let page = info.page.clone();
                    self.open_tab(&page, false);
                }
            }
            ChromeMessage::Navigate { input } => {
                let engine = self.read_settings().search_engine;
                if let Some(url) = network::resolve_input(&input, engine) {
                    self.load_in_active(&url);
                }
            }
            ChromeMessage::NewTab => self.open_new_tab(),
            ChromeMessage::ToggleMute { id } => self.handle_shortcut(Some(id), Shortcut::ToggleMute),
            ChromeMessage::Shortcut { shortcut } => self.handle_shortcut(self.tabs.active_id(), shortcut),
            ChromeMessage::MoveTab { id, index } => self.tabs.move_tab(id, index),
            ChromeMessage::CloseTab { id } => {
                if let Some(id) = id.or(self.tabs.active_id()) {
                    self.close_tab(id);
                }
            }
            ChromeMessage::ActivateTab { id } => self.tabs.activate(id, self.page_visible()),
            // Mêmes actions que les raccourcis des pages : un seul endroit pour les deux.
            ChromeMessage::Back => self.handle_shortcut(None, Shortcut::Back),
            ChromeMessage::Forward => self.handle_shortcut(None, Shortcut::Forward),
            ChromeMessage::Reload => self.handle_shortcut(None, Shortcut::Reload),
            ChromeMessage::Home => {
                let url = self.read_settings().start_url();
                self.load_in_active(&url);
            }
            ChromeMessage::Expand { open } => {
                self.expanded = open;
                if layers::SUPPORTED {
                    self.layers.set_on_top(&self.window, &self.chrome, open);
                } else {
                    self.tabs.set_active_visible(!open);
                }
            }
            // Messages fréquents (à chaque image) : rien à redessiner dans l'interface.
            ChromeMessage::Shape { rects } => {
                self.layers.set_shape(&self.window, &self.chrome, rects);
                return;
            }
            ChromeMessage::SidebarWidth { width, done } => {
                self.set_sidebar_width(width, done);
                if !done {
                    return;
                }
            }
            ChromeMessage::FocusPage => self.focus_page(),
            ChromeMessage::FocusBar => self.focus_bar(),
            ChromeMessage::SaveSettings { settings } => self.save_settings(settings),
            ChromeMessage::ToggleBookmark { id } => {
                if let Some(id) = id.or(self.tabs.active_id()) {
                    self.toggle_bookmark(id);
                }
            }
            ChromeMessage::Library { kind, query } => self.send_library(kind, &query),
            ChromeMessage::OpenUrl { url, new_tab, private } => {
                if network::is_web(&url) {
                    if private {
                        self.open_tab(&url, true);
                    } else if new_tab {
                        self.open_tab_in_background(&url);
                    } else {
                        self.load_in_active(&url);
                    }
                }
            }
            ChromeMessage::RemoveBookmark { url } => {
                if let Ok(mut lib) = self.library.write() {
                    lib.remove_bookmark(&url);
                }
                // La liste n'est plus renvoyée ici sans filtre : l'interface la redemande
                // elle-même avec la recherche en cours.
                self.reload_home_tabs();
            }
            ChromeMessage::RemoveVisit { url } => {
                if let Ok(mut lib) = self.library.write() {
                    lib.remove_visits(&url);
                }
                self.fulltext.remove(&url);
            }
            ChromeMessage::ClearHistory => {
                if let Ok(mut lib) = self.library.write() {
                    lib.clear_history();
                }
                self.fulltext.clear();
                self.send_library(LibraryKind::History, "");
            }
            ChromeMessage::RevealDownload { id } => {
                if let Some(d) = self.downloads.iter().find(|d| d.id == id) {
                    downloads::reveal(&d.path);
                }
            }
            ChromeMessage::OpenDownload { id } => {
                if let Some(d) = self.downloads.iter().find(|d| d.id == id && d.state == downloads::State::Done) {
                    downloads::open(&d.path);
                }
            }
            ChromeMessage::ClearDownloads => {
                self.downloads.retain(|d| d.state == downloads::State::Progress);
            }
            ChromeMessage::CancelDownload { id } => self.cancel_download(id),
            ChromeMessage::Strip { open } => {
                self.strip = open;
                self.relayout();
            }
            ChromeMessage::Find { text, backwards } => self.find(&text, backwards),
            ChromeMessage::FindClose => {
                self.find("", false);
                self.focus_page();
            }
            ChromeMessage::Suggest { query } => self.send_suggestions(&query),
            ChromeMessage::Command { input, run } => self.run_command(&input, run),
            ChromeMessage::PermissionAnswer { request, allow, remember } => {
                self.answer_permission(request, allow, remember)
            }
            ChromeMessage::ForgetPermission { host, permission } => {
                if let Ok(mut sites) = self.sites.write() {
                    sites.forget_permission(&host, permission);
                }
            }
            ChromeMessage::SleepTab { id } => {
                if self.tabs.sleep(id) {
                    self.schedule_reap();
                }
            }
            ChromeMessage::SleepHidden => {
                if self.tabs.sleep_hidden() {
                    self.schedule_reap();
                }
            }
            ChromeMessage::SetPermission { host, permission, allow } => {
                if let Ok(mut sites) = self.sites.write() {
                    sites.set_permission(&host, permission, allow);
                }
                self.schedule_persist();
            }
            ChromeMessage::ClearSiteData => self.clear_site_data(),
        }
        self.push_state();
    }

    fn handle_page(&mut self, id: TabId, message: PageMessage) {
        if let Some(shortcut) = message.shortcut {
            self.handle_shortcut(Some(id), shortcut);
            self.push_state();
        }
        if let Some(site) = message.site {
            if let Some(tab) = self.tabs.get_mut(id)
                && !network::is_home(tab.url())
                && tab.failed.is_none()
            {
                let (icon, name) = ui::clean_site(site);
                tab.icon = icon;
                tab.site_name = name;
            }
            self.push_state();
        }
        if let Some(url) = message.open_tab
            && network::is_web(&url)
            && self.allow_page_tab()
        {
            self.open_tab_in_background(&url);
        }
        if let Some(link) = message.link
            && network::is_web(&link.url)
            && self.allow_page_tab()
        {
            match link.how {
                LinkHow::Tab => self.open_tab(&link.url, false),
                LinkHow::Background => self.open_tab_in_background(&link.url),
                LinkHow::Private => self.open_tab(&link.url, true),
            }
        }
        if let Some(audible) = message.audible {
            if let Some(tab) = self.tabs.get_mut(id) {
                tab.audible = audible;
            }
            self.push_state();
        }
        if let Some(result) = message.find_result
            && Some(id) == self.tabs.active_id()
        {
            self.chrome_call(&format!("navigateur.setFind({})", json!(result)));
        }
        if let Some(metrics) = message.metrics {
            self.page_metrics(id, metrics);
        }
        if let Some(text) = message.text {
            self.index_page(id, &text);
        }
        // Clic dans la page pendant qu'un panneau est ouvert par-dessus : il se ferme.
        if message.pressed && self.expanded {
            self.chrome_call("navigateur.closePanel()");
        }
        if let Some(request) = message.permission {
            self.ask_permission(id, request);
        }
        // Seule la page d'accueil intégrée a le droit d'utiliser sa barre de recherche.
        let Some(tab) = self.tabs.get(id) else { return };
        if !network::is_home(tab.url()) {
            return;
        }
        if let Some(input) = message.navigate {
            let engine = self.read_settings().search_engine;
            if let Some(url) = network::resolve_input(&input, engine)
                && let Some(w) = tab.webview()
            {
                let _ = w.load_url(&url);
            }
        }
        if let Some(text) = message.open_bar {
            let text = serde_json::to_string(&text).unwrap_or_default();
            self.focus_chrome(&format!("navigateur.openBar({text})"));
        }
    }

    /// Action d'un raccourci, venu d'une page ou de l'interface. `id` : l'onglet
    /// concerné (celui de la page, ou l'onglet actif). L'appelant redessine ensuite.
    fn handle_shortcut(&mut self, id: Option<TabId>, shortcut: Shortcut) {
        match shortcut {
            Shortcut::FocusBar => self.focus_bar(),
            Shortcut::Back => self.go_back(),
            Shortcut::Forward => self.with_active(|w| w.go_forward()),
            Shortcut::Reload => self.reload(),
            Shortcut::Settings => self.focus_chrome("navigateur.openSettings()"),
            Shortcut::NewTab => self.open_new_tab(),
            Shortcut::CloseTab => {
                if let Some(id) = id {
                    self.close_tab(id);
                }
            }
            Shortcut::ToggleMute => {
                if let Some(tab) = id.and_then(|id| self.tabs.get_mut(id)) {
                    tab.muted = !tab.muted;
                    tab.apply_mute();
                }
            }
            Shortcut::ToggleBookmark => {
                if let Some(id) = id {
                    self.toggle_bookmark(id);
                }
            }
            Shortcut::NextTab => self.tabs.cycle(1, self.page_visible()),
            Shortcut::PreviousTab => self.tabs.cycle(-1, self.page_visible()),
            Shortcut::Find => self.focus_chrome("navigateur.openFind()"),
            Shortcut::ZoomIn => self.zoom(1),
            Shortcut::ZoomOut => self.zoom(-1),
            Shortcut::ZoomReset => self.zoom(0),
            Shortcut::ReopenTab => {
                if let Some(url) = self.tabs.pop_closed() {
                    self.open_tab(&url, false);
                }
            }
            Shortcut::Print => self.with_active(|w| w.print()),
            Shortcut::NewPrivateTab => self.open_tab(network::HOME_URL, true),
            Shortcut::TaskManager => self.focus_chrome("navigateur.openMemory()"),
        }
    }

    /// Une page a-t-elle encore le droit d'ouvrir un onglet ? (voir `PAGE_TAB_LIMIT`)
    fn allow_page_tab(&mut self) -> bool {
        let now = Instant::now();
        while self
            .page_opens
            .front()
            .is_some_and(|t| now.duration_since(*t) > PAGE_TAB_WINDOW)
        {
            self.page_opens.pop_front();
        }
        if self.page_opens.len() >= PAGE_TAB_LIMIT {
            return false;
        }
        self.page_opens.push_back(now);
        true
    }

    /// Un nouvel onglet s'ouvre toujours sur l'accueil Feather (heure et recherche).
    fn open_new_tab(&mut self) {
        self.open_tab(network::HOME_URL, false);
    }

    /// Zone des pages selon la position et la largeur de la barre, et le bandeau du bas.
    fn page_bounds(&self) -> wry::Rect {
        let settings = self.read_settings();
        ui::page_bounds(&self.window, settings.bar_position, settings.sidebar_width, self.strip)
    }

    /// La page active reste affichée : toujours quand les panneaux passent
    /// par-dessus, sinon seulement si aucun panneau n'est ouvert.
    fn page_visible(&self) -> bool {
        !self.expanded || layers::SUPPORTED
    }

    /// La barre latérale change de largeur ; on enregistre quand le bouton est relâché.
    fn set_sidebar_width(&mut self, width: u32, save: bool) {
        let width = width.clamp(settings::SIDEBAR_MIN, settings::SIDEBAR_MAX);
        if let Ok(mut s) = self.settings.write() {
            s.sidebar_width = width;
            if save && let Err(err) = s.save() {
                eprintln!("paramètres non enregistrés : {err}");
            }
        }
        self.tabs.set_bounds(self.page_bounds());
    }

    fn open_tab(&mut self, url: &str, private: bool) {
        let ctx = PageContext {
            window: &self.window,
            surface: &self.surface,
            proxy: &self.proxy,
            settings: &self.settings,
            library: &self.library,
            sites: &self.sites,
            bounds: self.page_bounds(),
        };
        match self.tabs.open(url, &ctx, self.page_visible(), private) {
            Ok(_) => self.focus_page(),
            Err(err) => eprintln!("impossible d'ouvrir l'onglet : {err}"),
        }
        // La nouvelle page est créée au-dessus de tout, panneau ouvert compris.
        self.layers.restack(&self.window, &self.chrome);
        self.push_state();
    }

    /// Recharge la page de l'onglet affiché s'il était en veille.
    fn wake_active(&mut self) {
        let Some(id) = self.tabs.active().filter(|t| t.is_asleep()).map(|t| t.id) else {
            return;
        };
        let ctx = PageContext {
            window: &self.window,
            surface: &self.surface,
            proxy: &self.proxy,
            settings: &self.settings,
            library: &self.library,
            sites: &self.sites,
            bounds: self.page_bounds(),
        };
        if let Err(err) = self.tabs.wake(id, &ctx, self.page_visible()) {
            eprintln!("impossible de réveiller l'onglet : {err}");
        }
        self.layers.restack(&self.window, &self.chrome);
        self.focus_page();
        self.push_state();
    }

    /// Ouvre un onglet sans le montrer : on reste sur la page en cours (clic de molette).
    fn open_tab_in_background(&mut self, url: &str) {
        let previous = self.tabs.active().map(|t| t.id);
        self.open_tab(url, false);
        if let Some(id) = previous {
            self.tabs.activate(id, self.page_visible());
            // `open_tab` a donné le focus clavier au nouvel onglet, caché : on le
            // rend à la page visible, sinon les touches partaient dans l'onglet caché.
            self.focus_page();
            self.push_state();
        }
    }

    /// Ferme un onglet ; on garde toujours au moins un onglet ouvert.
    fn close_tab(&mut self, id: TabId) {
        self.tabs.close(id, self.page_visible());
        self.schedule_reap();
        if self.tabs.is_empty() {
            self.open_new_tab();
        }
    }

    /// Les pages fermées ou mises en veille sont détruites trois secondes plus tard.
    fn schedule_reap(&self) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(3100));
            let _ = proxy.send_event(UserEvent::Reap);
        });
    }

    fn load_in_active(&mut self, url: &str) {
        match self.tabs.active().and_then(|t| t.webview()) {
            Some(w) => {
                let _ = w.load_url(url);
                self.focus_page();
            }
            None => self.open_tab(url, false),
        }
    }

    fn with_active(&self, action: impl FnOnce(&WebView) -> wry::Result<()>) {
        if let Some(w) = self.tabs.active().and_then(|t| t.webview()) {
            let _ = action(w);
        }
    }

    fn focus_page(&self) {
        if !self.expanded {
            self.with_active(|w| w.focus());
        }
    }

    /// Sur l'accueil, la barre est au milieu de la page ; ailleurs, elle
    /// s'ouvre par-dessus la page.
    fn focus_bar(&self) {
        match self.tabs.active() {
            Some(tab) if network::is_home(tab.url()) && !self.expanded && tab.webview().is_some() => {
                self.focus_page();
                tab.eval("window.focusSearch && focusSearch()");
            }
            _ => self.focus_chrome("navigateur.openBar('')"),
        }
    }

    fn focus_chrome(&self, script: &str) {
        let _ = self.chrome.focus();
        self.chrome_call(script);
    }

    /// Appelle une fonction de l'interface (`navigateur.…`), si elle est prête.
    fn chrome_call(&self, script: &str) {
        let _ = self
            .chrome
            .evaluate_script(&format!("window.navigateur && {script}"));
    }

    fn toggle_bookmark(&mut self, id: TabId) {
        let Some(tab) = self.tabs.get(id) else { return };
        if let Ok(mut lib) = self.library.write() {
            lib.toggle_bookmark(tab.url(), &tab.title, tab.icon.as_deref());
        }
        self.reload_home_tabs();
        self.push_state();
    }

    /// Les pages d'accueil ouvertes se redessinent (nouveaux paramètres ou favoris).
    fn reload_home_tabs(&self) {
        for w in self.tabs.home_tabs().filter_map(|t| t.webview()) {
            let _ = w.reload();
        }
    }

    fn save_settings(&mut self, settings: Settings) {
        let settings = settings.cleaned();
        if let Err(err) = settings.save() {
            eprintln!("paramètres non enregistrés : {err}");
        }
        let before = self.read_settings();
        if let Ok(mut lib) = self.library.write() {
            lib.prune(settings.history_days);
        }
        self.fulltext.prune(settings.history_days);
        // Désactiver la recherche dans le texte des pages efface ce qui était gardé.
        if before.index_pages && !settings.index_pages {
            self.fulltext.clear();
        }
        if before.blocks_trackers() != settings.blocks_trackers() {
            for w in self.tabs.iter().filter_map(|t| t.webview()) {
                trackers::set_enabled(w, settings.blocks_trackers());
            }
        }
        let economy_changed = before.economy != settings.economy;
        if let Ok(mut current) = self.settings.write() {
            *current = settings;
        }
        if economy_changed {
            let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
            for id in ids {
                self.apply_economy(id);
            }
        }
        self.relayout();
        // Les pages d'accueil ouvertes reprennent le nouveau prénom et thème.
        self.reload_home_tabs();
    }

    /// Envoie au panneau Bibliothèque la liste demandée.
    fn send_library(&self, kind: LibraryKind, query: &str) {
        let items = match kind {
            LibraryKind::Downloads => json!(self.downloads),
            LibraryKind::Bookmarks => match self.library.read() {
                Ok(lib) => json!(lib.search_bookmarks(query)),
                Err(_) => json!([]),
            },
            LibraryKind::History => self.history_items(query),
        };
        let kind = match kind {
            LibraryKind::Bookmarks => "bookmarks",
            LibraryKind::History => "history",
            LibraryKind::Downloads => "downloads",
        };
        let data = json!({ "kind": kind, "items": items });
        self.chrome_call(&format!("navigateur.setLibrary({data})"));
    }

    /// Prévoit un enregistrement dans deux secondes, pour regrouper les changements.
    fn schedule_persist(&self) {
        if self.persist_pending.replace(true) {
            return;
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            let _ = proxy.send_event(UserEvent::Persist);
        });
    }

    /// Enregistre tout de suite l'historique, les favoris, les réglages par site,
    /// les notes, le texte des pages et les onglets ouverts (seul ce qui a changé).
    fn persist(&mut self) {
        if let Ok(mut lib) = self.library.write()
            && let Err(err) = lib.save()
        {
            eprintln!("historique ou favoris non enregistrés : {err}");
        }
        if let Ok(mut sites) = self.sites.write()
            && let Err(err) = sites.save()
        {
            eprintln!("réglages des sites non enregistrés : {err}");
        }
        if let Err(err) = self.notes.save() {
            eprintln!("notes non enregistrées : {err}");
        }
        if let Err(err) = self.fulltext.save() {
            eprintln!("texte des pages non enregistré : {err}");
        }
        if self.tabs.is_empty() {
            return;
        }
        // `push_state` prévoit un enregistrement à chaque changement (son, icône…) :
        // on n'écrit la session que si les onglets ont vraiment changé.
        let session = self.tabs.session();
        if self.saved_session.borrow().as_ref() == Some(&session) {
            return;
        }
        match library::save_session(&session) {
            Ok(()) => *self.saved_session.borrow_mut() = Some(session),
            Err(err) => eprintln!("session non enregistrée : {err}"),
        }
    }

    fn read_settings(&self) -> Settings {
        self.settings.read().map(|s| s.clone()).unwrap_or_default()
    }

    fn relayout(&self) {
        let _ = self.chrome.set_bounds(ui::chrome_bounds(&self.window));
        self.tabs.set_bounds(self.page_bounds());
    }

    fn start_check(&mut self, manual: bool) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready { .. }) {
            return;
        }
        self.update = UpdateState::Checking;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let found = update::check();
            let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Checked { found, manual }));
        });
    }

    fn start_install(&mut self) {
        let UpdateState::Available { info } = &self.update else { return };
        let info = info.clone();
        self.update = UpdateState::Downloading { info: info.clone() };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = update::install(&info);
            let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Installed(result)));
        });
    }

    fn handle_update(&mut self, event: UpdateEvent) {
        match event {
            UpdateEvent::Checked { found, manual } => {
                self.update = match found {
                    Ok(Some(info)) => UpdateState::Available { info },
                    Ok(None) => UpdateState::UpToDate,
                    // Une vérification automatique qui échoue (hors ligne) reste silencieuse.
                    Err(_) if !manual => UpdateState::Idle,
                    Err(message) => UpdateState::Error { message },
                };
            }
            UpdateEvent::Installed(result) => {
                let info = match &self.update {
                    UpdateState::Downloading { info } => Some(info.clone()),
                    _ => None,
                };
                self.update = match (result, info) {
                    (Ok(restart), Some(info)) => {
                        self.restart = Some(restart);
                        UpdateState::Ready { info }
                    }
                    (Err(message), _) => UpdateState::Error { message },
                    _ => UpdateState::Idle,
                };
            }
        }
        self.push_state();
    }

    /// Envoie à l'interface l'état complet (onglets et paramètres) à redessiner.
    fn push_state(&self) {
        let active = self.tabs.active().and_then(|t| t.webview());
        let lib = self.library.read().ok();
        let permissions = self.sites.read().map(|s| s.decisions()).unwrap_or_default();
        let commands: Vec<_> = commands::liste()
            .into_iter()
            .map(|(name, desc)| json!({ "name": format!("/{name}"), "desc": desc }))
            .collect();
        let state = json!({
            "tabs": self.tabs.snapshot(|url| lib.as_ref().is_some_and(|l| l.is_bookmarked(url))),
            "downloads": self.downloads,
            "can_back": active.and_then(|w| w.can_go_back().ok()).unwrap_or(false),
            "can_forward": active.and_then(|w| w.can_go_forward().ok()).unwrap_or(false),
            "settings": self.read_settings(),
            "version": update::CURRENT,
            "update": self.update,
            "permissions": permissions,
            "permission_prompt": self.permission_prompt(),
            "native_commands": commands,
            "overlay": layers::SUPPORTED,
            "site": self.site_info(),
        });
        drop(lib);
        self.chrome_call(&format!("navigateur.setState({state})"));
        self.schedule_persist();
    }
}
