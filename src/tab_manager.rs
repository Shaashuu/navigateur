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
    /// La page joue du son (vidéo ou musique).
    pub audible: bool,
    /// L'utilisateur a coupé le son de cet onglet.
    pub muted: bool,
    /// Adresse de l'icône du site et nom du site, donnés par la page.
    pub icon: Option<String>,
    pub site_name: Option<String>,
    webview: WebView,
}

impl Tab {
    pub fn webview(&self) -> &WebView {
        &self.webview
    }

    /// Coupe ou remet le son des vidéos et musiques de la page (voir ui::PAGE_SCRIPT).
    pub fn apply_mute(&self) {
        let _ = self.webview.evaluate_script(&format!(
            "window.__featherSetMuted && __featherSetMuted({})",
            self.muted
        ));
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
    pub audible: bool,
    pub muted: bool,
    pub icon: Option<&'a str>,
    pub site_name: Option<&'a str>,
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
    /// Onglets fermés, gardés quelques secondes le temps que leur page s'arrête vraiment.
    closing: Vec<(std::time::Instant, Tab)>,
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
            audible: false,
            muted: false,
            icon: None,
            site_name: None,
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
        // On arrête le son avant de détruire la page, et on lui laisse quelques secondes
        // pour s'arrêter (voir `reap`) : détruite tout de suite, sa vidéo pouvait continuer.
        let tab = &self.tabs[index];
        let _ = tab.webview.set_visible(false);
        let _ = tab.webview.evaluate_script(
            "document.querySelectorAll('video, audio').forEach((m) => { m.pause(); m.removeAttribute('src'); m.load(); })",
        );
        let _ = tab.webview.load_url("about:blank");
        let closed = self.tabs.remove(index);
        self.closing.push((std::time::Instant::now(), closed));
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

    /// Détruit les onglets fermés depuis un moment.
    pub fn reap(&mut self) {
        self.closing
            .retain(|(since, _)| since.elapsed() < std::time::Duration::from_secs(3));
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

    /// Déplace un onglet à une nouvelle position (glisser-déposer dans la colonne).
    pub fn move_tab(&mut self, id: TabId, index: usize) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        let tab = self.tabs.remove(from);
        let index = index.min(self.tabs.len());
        self.tabs.insert(index, tab);
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
                audible: t.audible,
                muted: t.muted,
                icon: t.icon.as_deref(),
                site_name: t.site_name.as_deref(),
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
        // macOS : un clic sur une page peu après l'avoir quittée ne doit pas servir qu'à la réactiver.
        .with_accept_first_mouse(true)
        .with_initialization_script(if std::env::var_os("FEATHER_DIAG").is_some() {
            ui::DIAG_SCRIPT
        } else {
            ""
        })
        // FEATHER_SANS_SCRIPT=1 : pas de script injecté (pour chercher l'origine d'un défaut).
        .with_initialization_script(if std::env::var_os("FEATHER_SANS_SCRIPT").is_some() {
            ""
        } else {
            ui::PAGE_SCRIPT
        })
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
        // Les pages envoient des raccourcis clavier (voir ui::PAGE_SCRIPT) ; l'accueil, sa barre de recherche.
        .with_ipc_handler(move |request| {
            let _ = on_ipc.send_event(UserEvent::Page(id, request.into_body()));
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

    // Sur macOS, WebKit s'annonce sans « Safari » : YouTube et d'autres sites se comportent
    // alors comme dans une vue intégrée (miniatures qui ne réagissent pas au clic).
    #[cfg(target_os = "macos")]
    let builder = builder.with_user_agent(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15",
    );

    ctx.surface.attach(builder, ctx.window)
}
