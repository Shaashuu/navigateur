//! Les onglets : une webview par onglet, toutes possédées par le `TabManager`.
//!
//! Fermer un onglet le retire du `Vec` : la webview est alors détruite (`drop`)
//! et sa mémoire libérée. Aucun `Rc`/`RefCell` : les rappels des webviews ne
//! connaissent que l'identifiant de leur onglet et envoient des messages à la
//! boucle d'événements.

use std::borrow::Cow;
use std::sync::{Arc, RwLock};

use serde::Serialize;
use tao::event_loop::EventLoopProxy;
use tao::window::Window;
use wry::http::{Response, header::CONTENT_TYPE};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder};

use crate::UserEvent;
use crate::network;
use crate::settings::Settings;
use crate::ui::{self, Surface};

pub type TabId = u32;

pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub url: String,
    pub loading: bool,
    webview: WebView,
}

impl Tab {
    pub fn webview(&self) -> &WebView {
        &self.webview
    }
}

/// Ce que l'interface reçoit pour dessiner les pastilles.
#[derive(Serialize)]
pub struct TabInfo<'a> {
    pub id: TabId,
    pub title: &'a str,
    pub url: &'a str,
    pub active: bool,
    pub loading: bool,
    pub home: bool,
}

/// Tout ce qu'il faut pour créer la webview d'un nouvel onglet.
pub struct PageContext<'a> {
    pub window: &'a Window,
    pub surface: &'a Surface,
    pub proxy: &'a EventLoopProxy<UserEvent>,
    pub settings: &'a Arc<RwLock<Settings>>,
    pub bounds: Rect,
}

#[derive(Default)]
pub struct TabManager {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: TabId,
}

impl TabManager {
    /// Ouvre un onglet, le rend actif et renvoie son identifiant.
    pub fn open(&mut self, url: &str, ctx: &PageContext, visible: bool) -> wry::Result<TabId> {
        self.next_id += 1;
        let id = self.next_id;
        let webview = build_page(id, url, ctx)?;
        self.tabs.push(Tab {
            id,
            title: String::new(),
            url: url.to_string(),
            loading: true,
            webview,
        });
        self.activate(id, visible);
        Ok(id)
    }

    /// Ferme un onglet. Si c'était l'onglet actif, son voisin devient actif.
    pub fn close(&mut self, id: TabId, visible: bool) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        // `remove` rend l'onglet, qui est détruit à la fin de cette instruction.
        self.tabs.remove(index);
        if self.active == Some(id) {
            self.active = None;
            let neighbour = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|t| t.id);
            if let Some(next) = neighbour {
                self.activate(next, visible);
            }
        }
    }

    pub fn activate(&mut self, id: TabId, visible: bool) {
        if !self.tabs.iter().any(|t| t.id == id) {
            return;
        }
        self.active = Some(id);
        for tab in &self.tabs {
            let _ = tab.webview.set_visible(visible && tab.id == id);
        }
    }

    /// Onglet suivant (`step = 1`) ou précédent (`step = -1`), en boucle.
    pub fn cycle(&mut self, step: isize, visible: bool) {
        let Some(current) = self.active_index() else {
            return;
        };
        let len = self.tabs.len() as isize;
        let next = (current as isize + step).rem_euclid(len) as usize;
        let id = self.tabs[next].id;
        self.activate(id, visible);
    }

    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Montre ou cache l'onglet actif (caché quand un panneau de l'interface est ouvert).
    pub fn set_active_visible(&self, visible: bool) {
        if let Some(tab) = self.active() {
            let _ = tab.webview.set_visible(visible);
        }
    }

    pub fn set_bounds(&self, bounds: Rect) {
        for tab in &self.tabs {
            let _ = tab.webview.set_bounds(bounds);
        }
    }

    pub fn home_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter().filter(|t| network::is_home(&t.url))
    }

    pub fn snapshot(&self) -> Vec<TabInfo<'_>> {
        self.tabs
            .iter()
            .map(|t| TabInfo {
                id: t.id,
                title: &t.title,
                url: &t.url,
                active: Some(t.id) == self.active,
                loading: t.loading,
                home: network::is_home(&t.url),
            })
            .collect()
    }

    fn active_index(&self) -> Option<usize> {
        self.active
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
    }
}

fn build_page(id: TabId, url: &str, ctx: &PageContext) -> wry::Result<WebView> {
    let settings = Arc::clone(ctx.settings);
    let on_ipc = ctx.proxy.clone();
    let on_title = ctx.proxy.clone();
    let on_load = ctx.proxy.clone();
    let on_new_window = ctx.proxy.clone();

    let builder = WebViewBuilder::new()
        .with_bounds(ctx.bounds)
        .with_url(url)
        .with_devtools(cfg!(debug_assertions))
        .with_initialization_script(ui::PAGE_SCRIPT)
        // La page d'accueil intégrée est générée à chaque visite avec les paramètres actuels.
        .with_custom_protocol("navigateur".into(), move |_id, _request| {
            let html = match settings.read() {
                Ok(s) => ui::home_page(&s),
                Err(_) => ui::home_page(&Settings::default()),
            };
            Response::builder()
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Cow::Owned(html.into_bytes()))
                .expect("réponse HTTP valide")
        })
        // Les pages ne peuvent envoyer que des raccourcis clavier (voir ui::PAGE_SCRIPT).
        .with_ipc_handler(move |request| {
            let _ = on_ipc.send_event(UserEvent::Shortcut(id, request.into_body()));
        })
        .with_document_title_changed_handler(move |title| {
            let _ = on_title.send_event(UserEvent::Title(id, title));
        })
        .with_on_page_load_handler(move |event, url| {
            let loading = matches!(event, PageLoadEvent::Started);
            let _ = on_load.send_event(UserEvent::PageLoad(id, loading, url));
        })
        // Les liens « nouvelle fenêtre » s'ouvrent dans un nouvel onglet.
        .with_new_window_req_handler(move |url, _features| {
            let _ = on_new_window.send_event(UserEvent::OpenInNewTab(url));
            NewWindowResponse::Deny
        });

    ctx.surface.attach(builder, ctx.window)
}
