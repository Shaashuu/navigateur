//! Les onglets : une webview par onglet, toutes possédées par le `TabManager`.
//!
//! Fermer un onglet le retire du `Vec` : la webview est alors détruite (`drop`)
//! et sa mémoire libérée. Aucun `Rc`/`RefCell` : les rappels des webviews ne
//! connaissent que l'identifiant de leur onglet et envoient des messages à la
//! boucle d'événements.
//!
//! Un onglet peut aussi être « en veille » : sa webview est détruite mais on
//! garde son adresse, son titre et son icône ; elle est recréée quand on y revient.

use std::borrow::Cow;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use serde::Serialize;
use tao::event_loop::EventLoopProxy;
use tao::window::Window;
use wry::http::{Response, header::CONTENT_TYPE};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder};

use crate::UserEvent;
use crate::downloads;
use crate::library::{Library, Session};
use crate::network;
use crate::settings::Settings;
use crate::sitedata::SiteData;
use crate::trackers;
use crate::ui::{self, Surface};
use crate::weight::Weight;

pub type TabId = u32;

/// Nombre d'onglets fermés que `Ctrl+Maj+T` peut rouvrir.
const CLOSED_MEMORY: usize = 10;

pub struct Tab {
    pub id: TabId,
    pub title: String,
    url: String,
    pub loading: bool,
    /// La page joue du son (vidéo ou musique).
    pub audible: bool,
    /// L'utilisateur a coupé le son de cet onglet.
    pub muted: bool,
    /// Adresse de l'icône du site et nom du site, donnés par la page.
    pub icon: Option<String>,
    pub site_name: Option<String>,
    /// Onglet privé : ni historique, ni session, ni cookies gardés.
    pub private: bool,
    /// Dernière fois que l'onglet était au premier plan (pour la mise en veille).
    pub last_active: Instant,
    /// Mémoire et processeur de la page (voir `weight.rs`).
    pub weight: Weight,
    /// Hôte de la page, partagé avec les rappels de la webview (autorisations, traqueurs).
    host: Arc<RwLock<String>>,
    /// `None` : l'onglet est en veille.
    webview: Option<WebView>,
}

impl Tab {
    /// La webview de l'onglet, sauf s'il est en veille.
    pub fn webview(&self) -> Option<&WebView> {
        self.webview.as_ref()
    }

    pub fn is_asleep(&self) -> bool {
        self.webview.is_none()
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Change l'adresse connue de l'onglet (et l'hôte vu par ses rappels).
    pub fn set_url(&mut self, url: String) {
        if let Ok(mut host) = self.host.write() {
            *host = network::host_of(&url).unwrap_or_default();
        }
        self.url = url;
    }

    pub fn host(&self) -> String {
        self.host.read().map(|h| h.clone()).unwrap_or_default()
    }

    /// Exécute un script dans la page (rien si l'onglet est en veille).
    pub fn eval(&self, script: &str) {
        if let Some(w) = &self.webview {
            let _ = w.evaluate_script(script);
        }
    }

    /// Coupe ou remet le son des vidéos et musiques de la page (voir assets/page.js).
    pub fn apply_mute(&self) {
        self.eval(&format!("window.__featherSetMuted && __featherSetMuted({})", self.muted));
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
    pub bookmarked: bool,
    pub private: bool,
    pub asleep: bool,
    /// Connexion chiffrée (https) ou page intégrée.
    pub secure: bool,
    pub weight: Weight,
}

/// Tout ce qu'il faut pour créer la webview d'un nouvel onglet.
pub struct PageContext<'a> {
    pub window: &'a Window,
    pub surface: &'a Surface,
    pub proxy: &'a EventLoopProxy<UserEvent>,
    pub settings: &'a Arc<RwLock<Settings>>,
    pub library: &'a Arc<RwLock<Library>>,
    pub sites: &'a Arc<RwLock<SiteData>>,
    pub bounds: Rect,
}

#[derive(Default)]
pub struct TabManager {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: TabId,
    /// Onglets fermés, gardés quelques secondes le temps que leur page s'arrête vraiment.
    closing: Vec<(Instant, WebView)>,
    /// Adresses des derniers onglets fermés (les plus récents à la fin).
    closed: Vec<String>,
}

impl TabManager {
    /// Ouvre un onglet, le rend actif et renvoie son identifiant.
    pub fn open(&mut self, url: &str, ctx: &PageContext, visible: bool, private: bool) -> wry::Result<TabId> {
        let id = self.add_asleep(url, "", private);
        self.wake(id, ctx, false)?;
        self.activate(id, visible);
        Ok(id)
    }

    /// Ajoute un onglet en veille (sans page), par exemple à la restauration de
    /// la session : seul l'onglet affiché sera chargé.
    pub fn add_asleep(&mut self, url: &str, title: &str, private: bool) -> TabId {
        self.next_id += 1;
        let id = self.next_id;
        let mut tab = Tab {
            id,
            title: title.to_string(),
            url: String::new(),
            loading: false,
            audible: false,
            muted: false,
            icon: None,
            site_name: None,
            private,
            last_active: Instant::now(),
            weight: Weight::default(),
            host: Arc::default(),
            webview: None,
        };
        tab.set_url(url.to_string());
        self.tabs.push(tab);
        id
    }

    /// Recrée la page d'un onglet en veille. `visible` : la montrer tout de suite.
    pub fn wake(&mut self, id: TabId, ctx: &PageContext, visible: bool) -> wry::Result<()> {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return Ok(());
        };
        if tab.webview.is_some() {
            return Ok(());
        }
        let webview = build_page(id, &tab.url, ctx, tab.private, Arc::clone(&tab.host))?;
        let _ = webview.set_visible(visible);
        tab.webview = Some(webview);
        tab.loading = true;
        tab.weight = Weight::default();
        Ok(())
    }

    /// Met un onglet en veille : sa page est détruite, son adresse gardée.
    /// L'onglet actif, un onglet qui joue du son ou un onglet privé (qui
    /// perdrait sa session) ne sont pas mis en veille.
    pub fn sleep(&mut self, id: TabId) -> bool {
        let active = self.active;
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        if active == Some(id) || tab.audible || tab.private {
            return false;
        }
        let Some(webview) = tab.webview.take() else {
            return false;
        };
        tab.loading = false;
        tab.weight = Weight::default();
        stop_page(&webview);
        self.closing.push((Instant::now(), webview));
        true
    }

    /// Ferme un onglet. Si c'était l'onglet actif, son voisin devient actif.
    pub fn close(&mut self, id: TabId, visible: bool) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        let mut closed = self.tabs.remove(index);
        // On garde l'adresse pour « rouvrir l'onglet fermé », sauf pour un onglet privé.
        if !closed.private && !network::is_home(&closed.url) {
            self.closed.retain(|u| *u != closed.url);
            self.closed.push(closed.url.clone());
            if self.closed.len() > CLOSED_MEMORY {
                self.closed.remove(0);
            }
        }
        // On arrête le son avant de détruire la page, et on lui laisse quelques secondes
        // pour s'arrêter (voir `reap`) : détruite tout de suite, sa vidéo pouvait continuer.
        if let Some(webview) = closed.webview.take() {
            stop_page(&webview);
            self.closing.push((Instant::now(), webview));
        }
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

    /// Adresse du dernier onglet fermé, retirée de la liste.
    pub fn pop_closed(&mut self) -> Option<String> {
        self.closed.pop()
    }

    /// Détruit les pages fermées depuis un moment.
    pub fn reap(&mut self) {
        self.closing
            .retain(|(since, _)| since.elapsed() < std::time::Duration::from_secs(3));
    }

    /// Rend un onglet actif. S'il est en veille, l'appelant le réveille ensuite
    /// (voir `App::wake_active`).
    pub fn activate(&mut self, id: TabId, visible: bool) {
        if !self.tabs.iter().any(|t| t.id == id) {
            return;
        }
        let now = Instant::now();
        for tab in &mut self.tabs {
            // L'onglet qu'on quitte compte comme vu à l'instant.
            if Some(tab.id) == self.active || tab.id == id {
                tab.last_active = now;
            }
            if let Some(w) = &tab.webview {
                let _ = w.set_visible(visible && tab.id == id);
            }
        }
        self.active = Some(id);
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

    pub fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.tabs.iter_mut()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Montre ou cache l'onglet actif (caché quand un panneau de l'interface est ouvert).
    pub fn set_active_visible(&self, visible: bool) {
        if let Some(w) = self.active().and_then(Tab::webview) {
            let _ = w.set_visible(visible);
        }
    }

    pub fn set_bounds(&self, bounds: Rect) {
        for w in self.tabs.iter().filter_map(Tab::webview) {
            let _ = w.set_bounds(bounds);
        }
    }

    pub fn home_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter().filter(|t| network::is_home(&t.url))
    }

    /// Les onglets ouverts, pour les rouvrir au prochain démarrage (sans les onglets privés).
    pub fn session(&self) -> Session {
        let public: Vec<&Tab> = self.tabs.iter().filter(|t| !t.private).collect();
        Session {
            urls: public.iter().map(|t| t.url.clone()).collect(),
            active: public.iter().position(|t| Some(t.id) == self.active).unwrap_or(0),
        }
    }

    /// Active l'onglet à cette position (restauration de la session).
    pub fn activate_index(&mut self, index: usize, visible: bool) {
        if let Some(id) = self.tabs.get(index).map(|t| t.id) {
            self.activate(id, visible);
        }
    }

    pub fn snapshot(&self, is_bookmarked: impl Fn(&str) -> bool) -> Vec<TabInfo<'_>> {
        self.tabs
            .iter()
            .map(|t| {
                let home = network::is_home(&t.url);
                TabInfo {
                    id: t.id,
                    title: &t.title,
                    url: &t.url,
                    active: Some(t.id) == self.active,
                    loading: t.loading,
                    home,
                    audible: t.audible,
                    muted: t.muted,
                    icon: t.icon.as_deref(),
                    site_name: t.site_name.as_deref(),
                    bookmarked: is_bookmarked(&t.url),
                    private: t.private,
                    asleep: t.webview.is_none(),
                    secure: home || !t.url.starts_with("http://"),
                    weight: t.weight,
                }
            })
            .collect()
    }

    fn active_index(&self) -> Option<usize> {
        self.active
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
    }
}

/// Coupe le son et vide une page avant de la détruire.
fn stop_page(webview: &WebView) {
    let _ = webview.set_visible(false);
    let _ = webview.evaluate_script(
        "document.querySelectorAll('video, audio').forEach((m) => { m.pause(); m.removeAttribute('src'); m.load(); })",
    );
    let _ = webview.load_url("about:blank");
}

fn build_page(
    id: TabId,
    url: &str,
    ctx: &PageContext,
    private: bool,
    host: Arc<RwLock<String>>,
) -> wry::Result<WebView> {
    let settings = Arc::clone(ctx.settings);
    let library = Arc::clone(ctx.library);
    let download_settings = Arc::clone(ctx.settings);
    let sites = Arc::clone(ctx.sites);
    let permission_host = Arc::clone(&host);
    let on_download = ctx.proxy.clone();
    let on_downloaded = ctx.proxy.clone();
    let on_ipc = ctx.proxy.clone();
    let on_title = ctx.proxy.clone();
    let on_load = ctx.proxy.clone();
    let on_new_window = ctx.proxy.clone();
    let block_trackers = ctx.settings.read().map(|s| s.block_trackers).unwrap_or(true);

    let builder = WebViewBuilder::new()
        .with_bounds(ctx.bounds)
        .with_url(url)
        .with_devtools(cfg!(debug_assertions))
        // Onglet privé : stockage à part, effacé à la fermeture.
        .with_incognito(private)
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
            let settings = settings.read().map(|s| s.clone()).unwrap_or_default();
            let html = match library.read() {
                Ok(lib) if settings.bookmarks_on_home => ui::home_page(&settings, lib.bookmarks()),
                _ => ui::home_page(&settings, &[]),
            };
            Response::builder()
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Cow::Owned(html.into_bytes()))
                .expect("réponse HTTP valide")
        })
        // Autorisations : la décision retenue pour ce site (la question est posée par
        // le script injecté avant que la page n'appelle le moteur).
        .with_permission_handler(move |kind| {
            let host = permission_host.read().map(|h| h.clone()).unwrap_or_default();
            sites
                .read()
                .map(|s| s.answer(&host, kind))
                .unwrap_or(wry::PermissionResponse::Default)
        })
        // Les pages envoient des raccourcis clavier (voir assets/page.js) ; l'accueil, sa barre de recherche.
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
        // Téléchargements : rangés dans le dossier choisi, suivis dans la bibliothèque.
        .with_download_started_handler(move |url, path| {
            let dir = download_settings.read().map(|s| s.download_dir.clone()).unwrap_or_default();
            *path = downloads::destination(path, &dir);
            let _ = on_download.send_event(UserEvent::DownloadStarted { url, path: path.clone(), private });
            true
        })
        .with_download_completed_handler(move |url, path, success| {
            let _ = on_downloaded.send_event(UserEvent::DownloadDone { url, path, success });
        })
        // Les liens « nouvelle fenêtre » s'ouvrent dans un nouvel onglet (privé si la page l'est).
        .with_new_window_req_handler(move |url, _features| {
            let _ = on_new_window.send_event(UserEvent::OpenInNewTab { url, private });
            NewWindowResponse::Deny
        });

    let webview = ctx.surface.attach(builder, ctx.window)?;
    trackers::install(&webview, host, block_trackers);
    Ok(webview)
}
