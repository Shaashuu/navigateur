// En mode release sur Windows, pas de console noire derrière la fenêtre.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Feather 0.1 : une fenêtre tao, une webview pour l'interface, et une
//! webview par onglet. Tout tourne sur le thread principal ; les webviews
//! communiquent avec le code Rust par des messages (`UserEvent`).

mod downloads;
mod library;
#[cfg(target_os = "macos")]
mod mac_mouse;
mod network;
mod settings;
mod tab_manager;
mod ui;
mod update;

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::Serialize;
use serde_json::json;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Icon, Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

use downloads::Download;
use library::Library;
use settings::{Settings, Startup};
use tab_manager::{PageContext, TabId, TabManager};
use ui::{ChromeMessage, LibraryKind, PageMessage, Shortcut, Surface};

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
    OpenInNewTab(String),
    Update(UpdateEvent),
    /// Les onglets fermés peuvent être détruits.
    Reap,
    /// Un téléchargement commence, rangé à cet endroit.
    DownloadStarted { url: String, path: PathBuf },
    DownloadDone { url: String, path: Option<PathBuf>, success: bool },
    /// Moment d'enregistrer l'historique, les favoris et la session.
    Persist,
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
    update: UpdateState,
    /// Programme à relancer quand la mise à jour est installée.
    restart: Option<update::Restart>,
    library: Arc<RwLock<Library>>,
    /// Téléchargements de cette session.
    downloads: Vec<Download>,
    /// Un enregistrement est déjà prévu (voir `schedule_persist`).
    persist_pending: Cell<bool>,
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

    let mut app = App {
        window,
        surface,
        chrome,
        tabs: TabManager::default(),
        settings: Arc::new(RwLock::new(settings)),
        proxy,
        expanded: false,
        update: UpdateState::Idle,
        restart: None,
        library: Arc::new(RwLock::new(library)),
        downloads: Vec::new(),
        persist_pending: Cell::new(false),
    };
    let session = library::load_session();
    if restore && !session.urls.is_empty() {
        for url in &session.urls {
            app.open_tab(url);
        }
        app.tabs.activate_index(session.active, true);
        app.push_state();
    } else {
        app.open_tab(&start_url);
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

/// Le site d'une adresse (pour savoir si on change de site).
fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_string)
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
                    if let Ok(mut lib) = self.library.write() {
                        lib.set_title(&tab.url, &title);
                    }
                    tab.title = title;
                }
                self.push_state();
            }
            UserEvent::PageLoad(id, loading, url) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    // Autre site : on efface l'icône de l'ancien en attendant celle du nouveau.
                    if loading && host_of(&tab.url) != host_of(&url) {
                        tab.icon = None;
                        tab.site_name = None;
                    }
                    tab.loading = loading;
                    tab.url = url;
                    if !loading && let Ok(mut lib) = self.library.write() {
                        lib.record_visit(&tab.url, &tab.title);
                    }
                    if loading {
                        tab.audible = false;
                    } else if tab.muted {
                        // Nouvelle page : on garde l'onglet en sourdine.
                        tab.apply_mute();
                    }
                }
                self.push_state();
            }
            UserEvent::OpenInNewTab(url) => self.open_tab(&url),
            UserEvent::Update(event) => self.handle_update(event),
            UserEvent::Reap => self.tabs.reap(),
            UserEvent::DownloadStarted { url, path } => {
                let id = self.downloads.iter().map(|d| d.id).max().unwrap_or(0) + 1;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| url.clone());
                self.downloads.push(Download { id, url, name, path, state: downloads::State::Progress });
                self.push_state();
            }
            UserEvent::DownloadDone { url, path, success } => {
                let found = self
                    .downloads
                    .iter_mut()
                    .rev()
                    .find(|d| d.url == url && d.state == downloads::State::Progress);
                if let Some(d) = found {
                    d.state = if success { downloads::State::Done } else { downloads::State::Failed };
                    if let Some(path) = path {
                        d.path = path;
                    }
                }
                self.push_state();
            }
            UserEvent::Persist => {
                self.persist_pending.set(false);
                self.persist();
            }
        }
    }

    fn handle_chrome(&mut self, message: ChromeMessage) {
        match message {
            ChromeMessage::Ready => {}
            ChromeMessage::CheckUpdate => self.start_check(true),
            ChromeMessage::InstallUpdate => self.start_install(),
            ChromeMessage::RestartUpdate => {
                if let Some(restart) = self.restart.take() {
                    self.persist();
                    match restart.launch() {
                        Ok(()) => std::process::exit(0),
                        Err(err) => {
                            self.update = UpdateState::Error {
                                message: format!("redémarrage impossible : {err}"),
                            }
                        }
                    }
                }
            }
            ChromeMessage::OpenReleasePage => {
                if let UpdateState::Available { info } | UpdateState::Ready { info } = &self.update {
                    let page = info.page.clone();
                    self.open_tab(&page);
                }
            }
            ChromeMessage::Navigate { input } => {
                let engine = self.read_settings().search_engine;
                if let Some(url) = network::resolve_input(&input, engine) {
                    self.load_in_active(&url);
                }
            }
            ChromeMessage::NewTab => self.open_new_tab(),
            ChromeMessage::ToggleMute { id } => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.muted = !tab.muted;
                    tab.apply_mute();
                }
            }
            ChromeMessage::MoveTab { id, index } => self.tabs.move_tab(id, index),
            ChromeMessage::CloseTab { id } => {
                if let Some(id) = id.or(self.tabs.active_id()) {
                    self.close_tab(id);
                }
            }
            ChromeMessage::ActivateTab { id } => self.tabs.activate(id, !self.expanded),
            ChromeMessage::Back => self.with_active(|w| w.go_back()),
            ChromeMessage::Forward => self.with_active(|w| w.go_forward()),
            ChromeMessage::Reload => self.with_active(|w| w.reload()),
            ChromeMessage::Home => {
                let url = self.read_settings().start_url();
                self.load_in_active(&url);
            }
            ChromeMessage::Expand { open } => {
                self.expanded = open;
                self.tabs.set_active_visible(!open);
            }
            ChromeMessage::FocusPage => self.focus_page(),
            ChromeMessage::FocusBar => self.focus_bar(),
            ChromeMessage::SaveSettings { settings } => {
                let settings = settings.cleaned();
                if let Err(err) = settings.save() {
                    eprintln!("paramètres non enregistrés : {err}");
                }
                if let Ok(mut lib) = self.library.write() {
                    lib.prune(settings.history_days);
                }
                if let Ok(mut current) = self.settings.write() {
                    *current = settings;
                }
                self.relayout();
                // Les pages d'accueil ouvertes reprennent le nouveau prénom et thème.
                self.reload_home_tabs();
            }
            ChromeMessage::ToggleBookmark { id } => {
                if let Some(id) = id.or(self.tabs.active_id()) {
                    self.toggle_bookmark(id);
                }
            }
            ChromeMessage::Library { kind, query } => self.send_library(kind, &query),
            ChromeMessage::OpenUrl { url, new_tab } => {
                if library::is_web(&url) {
                    if new_tab {
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
                self.reload_home_tabs();
                self.send_library(LibraryKind::Bookmarks, "");
            }
            ChromeMessage::RemoveVisit { url } => {
                if let Ok(mut lib) = self.library.write() {
                    lib.remove_visits(&url);
                }
            }
            ChromeMessage::ClearHistory => {
                if let Ok(mut lib) = self.library.write() {
                    lib.clear_history();
                }
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
        }
        self.push_state();
    }

    fn handle_page(&mut self, id: TabId, message: PageMessage) {
        if let Some(shortcut) = message.shortcut {
            self.handle_shortcut(id, shortcut);
        }
        if let Some(site) = message.site {
            if let Some(tab) = self.tabs.get_mut(id)
                && !network::is_home(&tab.url)
            {
                let (icon, name) = ui::clean_site(site);
                tab.icon = icon;
                tab.site_name = name;
            }
            self.push_state();
        }
        if let Some(url) = message.open_tab
            && (url.starts_with("https://") || url.starts_with("http://"))
        {
            self.open_tab_in_background(&url);
        }
        if let Some(audible) = message.audible {
            if let Some(tab) = self.tabs.get_mut(id) {
                tab.audible = audible;
            }
            self.push_state();
        }
        // Seule la page d'accueil intégrée a le droit d'utiliser sa barre de recherche.
        let Some(tab) = self.tabs.get(id) else { return };
        if !network::is_home(&tab.url) {
            return;
        }
        if let Some(input) = message.navigate {
            let engine = self.read_settings().search_engine;
            if let Some(url) = network::resolve_input(&input, engine) {
                let _ = tab.webview().load_url(&url);
            }
        }
        if let Some(text) = message.open_bar {
            let text = serde_json::to_string(&text).unwrap_or_default();
            self.focus_chrome(&format!("navigateur.openBar({text})"));
        }
    }

    fn handle_shortcut(&mut self, id: TabId, shortcut: Shortcut) {
        match shortcut {
            Shortcut::FocusBar => self.focus_bar(),
            Shortcut::Back => self.with_active(|w| w.go_back()),
            Shortcut::Forward => self.with_active(|w| w.go_forward()),
            Shortcut::Reload => self.with_active(|w| w.reload()),
            Shortcut::Settings => self.focus_chrome("navigateur.openSettings()"),
            Shortcut::NewTab => self.open_new_tab(),
            Shortcut::CloseTab => self.close_tab(id),
            Shortcut::ToggleMute => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.muted = !tab.muted;
                    tab.apply_mute();
                }
            }
            Shortcut::ToggleBookmark => self.toggle_bookmark(id),
            Shortcut::NextTab => self.tabs.cycle(1, !self.expanded),
            Shortcut::PreviousTab => self.tabs.cycle(-1, !self.expanded),
        }
        self.push_state();
    }

    /// Un nouvel onglet s'ouvre toujours sur l'accueil Feather (heure et recherche).
    fn open_new_tab(&mut self) {
        self.open_tab(network::HOME_URL);
    }

    fn open_tab(&mut self, url: &str) {
        let ctx = PageContext {
            window: &self.window,
            surface: &self.surface,
            proxy: &self.proxy,
            settings: &self.settings,
            library: &self.library,
            bounds: ui::page_bounds(&self.window, self.read_settings().bar_position),
        };
        match self.tabs.open(url, &ctx, !self.expanded) {
            Ok(_) => self.focus_page(),
            Err(err) => eprintln!("impossible d'ouvrir l'onglet : {err}"),
        }
        self.push_state();
    }

    /// Ouvre un onglet sans le montrer : on reste sur la page en cours (clic de molette).
    fn open_tab_in_background(&mut self, url: &str) {
        let previous = self.tabs.active().map(|t| t.id);
        self.open_tab(url);
        if let Some(id) = previous {
            self.tabs.activate(id, !self.expanded);
            self.push_state();
        }
    }

    /// Ferme un onglet ; on garde toujours au moins un onglet ouvert.
    fn close_tab(&mut self, id: TabId) {
        self.tabs.close(id, !self.expanded);
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(3100));
            let _ = proxy.send_event(UserEvent::Reap);
        });
        if self.tabs.is_empty() {
            self.open_new_tab();
        }
    }

    fn load_in_active(&mut self, url: &str) {
        match self.tabs.active() {
            Some(tab) => {
                let _ = tab.webview().load_url(url);
                self.focus_page();
            }
            None => self.open_tab(url),
        }
    }

    fn with_active(&self, action: impl FnOnce(&WebView) -> wry::Result<()>) {
        if let Some(tab) = self.tabs.active() {
            let _ = action(tab.webview());
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
            Some(tab) if network::is_home(&tab.url) && !self.expanded => {
                let _ = tab.webview().focus();
                let _ = tab
                    .webview()
                    .evaluate_script("window.focusSearch && focusSearch()");
            }
            _ => self.focus_chrome("navigateur.openBar('')"),
        }
    }

    fn focus_chrome(&self, script: &str) {
        let _ = self.chrome.focus();
        let _ = self
            .chrome
            .evaluate_script(&format!("window.navigateur && {script}"));
    }

    fn toggle_bookmark(&mut self, id: TabId) {
        let Some(tab) = self.tabs.get(id) else { return };
        if let Ok(mut lib) = self.library.write() {
            lib.toggle_bookmark(&tab.url, &tab.title, tab.icon.as_deref());
        }
        self.reload_home_tabs();
        self.push_state();
    }

    /// Les pages d'accueil ouvertes se redessinent (nouveaux paramètres ou favoris).
    fn reload_home_tabs(&self) {
        for tab in self.tabs.home_tabs() {
            let _ = tab.webview().reload();
        }
    }

    /// Envoie au panneau Bibliothèque la liste demandée.
    fn send_library(&self, kind: LibraryKind, query: &str) {
        let items = match kind {
            LibraryKind::Downloads => json!(self.downloads),
            _ => match self.library.read() {
                Ok(lib) if kind == LibraryKind::Bookmarks => json!(lib.search_bookmarks(query)),
                Ok(lib) => json!(lib.search_history(query, 300)),
                Err(_) => json!([]),
            },
        };
        let kind = match kind {
            LibraryKind::Bookmarks => "bookmarks",
            LibraryKind::History => "history",
            LibraryKind::Downloads => "downloads",
        };
        let data = json!({ "kind": kind, "items": items });
        let _ = self
            .chrome
            .evaluate_script(&format!("window.navigateur && navigateur.setLibrary({data})"));
    }

    /// Prévoit un enregistrement dans deux secondes, pour regrouper les changements.
    fn schedule_persist(&self) {
        if self.persist_pending.replace(true) {
            return;
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = proxy.send_event(UserEvent::Persist);
        });
    }

    /// Enregistre tout de suite l'historique, les favoris et les onglets ouverts.
    fn persist(&self) {
        if let Ok(mut lib) = self.library.write()
            && let Err(err) = lib.save()
        {
            eprintln!("historique ou favoris non enregistrés : {err}");
        }
        if !self.tabs.is_empty()
            && let Err(err) = library::save_session(&self.tabs.session())
        {
            eprintln!("session non enregistrée : {err}");
        }
    }

    fn read_settings(&self) -> Settings {
        self.settings.read().map(|s| s.clone()).unwrap_or_default()
    }

    fn relayout(&self) {
        let _ = self.chrome.set_bounds(ui::chrome_bounds(&self.window));
        self.tabs.set_bounds(ui::page_bounds(&self.window, self.read_settings().bar_position));
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
        let active = self.tabs.active().map(|t| t.webview());
        let lib = self.library.read().ok();
        let state = json!({
            "tabs": self.tabs.snapshot(|url| lib.as_ref().is_some_and(|l| l.is_bookmarked(url))),
            "downloads": self.downloads,
            "can_back": active.and_then(|w| w.can_go_back().ok()).unwrap_or(false),
            "can_forward": active.and_then(|w| w.can_go_forward().ok()).unwrap_or(false),
            "settings": self.read_settings(),
            "version": update::CURRENT,
            "update": self.update,
        });
        drop(lib);
        let _ = self.chrome.evaluate_script(&format!(
            "window.navigateur && navigateur.setState({state})"
        ));
        self.schedule_persist();
    }
}
