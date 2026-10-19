// En mode release sur Windows, pas de console noire derrière la fenêtre.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Feather 0.1 : une fenêtre tao, une webview pour l'interface, et une
//! webview par onglet. Tout tourne sur le thread principal ; les webviews
//! communiquent avec le code Rust par des messages (`UserEvent`).

mod network;
mod settings;
mod tab_manager;
mod ui;

use std::sync::{Arc, RwLock};

use serde_json::json;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Icon, Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

use settings::Settings;
use tab_manager::{PageContext, TabId, TabManager};
use ui::{ChromeMessage, PageMessage, Shortcut, Surface};

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

    let settings = Settings::load();
    let start_url = settings.start_url();

    let mut app = App {
        window,
        surface,
        chrome,
        tabs: TabManager::default(),
        settings: Arc::new(RwLock::new(settings)),
        proxy,
        expanded: false,
    };
    app.open_tab(&start_url);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
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
                    tab.title = title;
                }
                self.push_state();
            }
            UserEvent::PageLoad(id, loading, url) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.loading = loading;
                    tab.url = url;
                }
                self.push_state();
            }
            UserEvent::OpenInNewTab(url) => self.open_tab(&url),
        }
    }

    fn handle_chrome(&mut self, message: ChromeMessage) {
        match message {
            ChromeMessage::Ready => {}
            ChromeMessage::Navigate { input } => {
                let engine = self.read_settings().search_engine;
                if let Some(url) = network::resolve_input(&input, engine) {
                    self.load_in_active(&url);
                }
            }
            ChromeMessage::NewTab => self.open_start_tab(),
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
                if let Ok(mut current) = self.settings.write() {
                    *current = settings;
                }
                // Les pages d'accueil ouvertes reprennent le nouveau prénom et thème.
                for tab in self.tabs.home_tabs() {
                    let _ = tab.webview().reload();
                }
            }
        }
        self.push_state();
    }

    fn handle_page(&mut self, id: TabId, message: PageMessage) {
        if let Some(shortcut) = message.shortcut {
            self.handle_shortcut(id, shortcut);
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
            Shortcut::NewTab => self.open_start_tab(),
            Shortcut::CloseTab => self.close_tab(id),
            Shortcut::NextTab => self.tabs.cycle(1, !self.expanded),
            Shortcut::PreviousTab => self.tabs.cycle(-1, !self.expanded),
        }
        self.push_state();
    }

    fn open_start_tab(&mut self) {
        let url = self.read_settings().start_url();
        self.open_tab(&url);
    }

    fn open_tab(&mut self, url: &str) {
        let ctx = PageContext {
            window: &self.window,
            surface: &self.surface,
            proxy: &self.proxy,
            settings: &self.settings,
            bounds: ui::page_bounds(&self.window),
        };
        match self.tabs.open(url, &ctx, !self.expanded) {
            Ok(_) => self.focus_page(),
            Err(err) => eprintln!("impossible d'ouvrir l'onglet : {err}"),
        }
        self.push_state();
    }

    /// Ferme un onglet ; on garde toujours au moins un onglet ouvert.
    fn close_tab(&mut self, id: TabId) {
        self.tabs.close(id, !self.expanded);
        if self.tabs.is_empty() {
            self.open_start_tab();
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

    fn read_settings(&self) -> Settings {
        self.settings.read().map(|s| s.clone()).unwrap_or_default()
    }

    fn relayout(&self) {
        let _ = self.chrome.set_bounds(ui::chrome_bounds(&self.window));
        self.tabs.set_bounds(ui::page_bounds(&self.window));
    }

    /// Envoie à l'interface l'état complet (onglets et paramètres) à redessiner.
    fn push_state(&self) {
        let state = json!({
            "tabs": self.tabs.snapshot(),
            "settings": self.read_settings(),
        });
        let _ = self.chrome.evaluate_script(&format!(
            "window.navigateur && navigateur.setState({state})"
        ));
    }
}
