//! L'interface du navigateur (la « chrome ») : une webview HTML/CSS placée
//! sous les pages, qui dessine la colonne de gauche (paramètres, pastille de
//! recherche, onglets) et la barre de recherche qui s'ouvre par-dessus la page.
//! Ce module définit aussi les messages échangés avec elle.

use serde::Deserialize;
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::window::Window;
use wry::{Rect, WebView, WebViewBuilder};

use crate::settings::{Settings, Theme};
use crate::tab_manager::TabId;

/// Largeur de la colonne des pastilles, à gauche (pixels logiques).
pub const SIDEBAR_WIDTH: f64 = 56.0;

pub const CHROME_HTML: &str = include_str!("../assets/chrome.html");
const HOME_HTML: &str = include_str!("../assets/home.html");

/// Injecté dans chaque page : relaie quelques raccourcis clavier vers Rust,
/// car une page qui a le focus reçoit les touches avant la fenêtre.
pub const PAGE_SCRIPT: &str = r#"
(() => {
  const post = (msg) => window.ipc.postMessage(JSON.stringify(msg));
  const send = (shortcut) => post({ shortcut });
  window.addEventListener('keydown', (e) => {
    const k = e.key.toLowerCase();
    const ctrl = (e.ctrlKey || e.metaKey) && !e.altKey;
    let a = null;
    if (e.altKey && !e.ctrlKey && !e.metaKey && k === 'arrowleft') a = 'back';
    else if (e.altKey && !e.ctrlKey && !e.metaKey && k === 'arrowright') a = 'forward';
    else if (k === 'f5' || (ctrl && k === 'r')) a = 'reload';
    else if (ctrl && (k === 'l' || k === 'k')) a = 'focus_bar';
    else if (ctrl && k === 't') a = 'new_tab';
    else if (ctrl && k === 'w') a = 'close_tab';
    else if (ctrl && k === 'tab') a = e.shiftKey ? 'previous_tab' : 'next_tab';
    else if (ctrl && k === ',') a = 'settings';
    if (a) { e.preventDefault(); e.stopPropagation(); send(a); }
  }, true);

  // Son : prévient l'interface quand une vidéo ou une musique joue, et
  // permet de couper le son de l'onglet.
  let muted = false;
  let last = false;
  const media = () => Array.from(document.querySelectorAll('video, audio'));
  const audible = (m) => !m.paused && !m.ended && (m.muted ? m.__featherMuted === true : m.volume > 0);
  const report = () => {
    const now = media().some(audible);
    if (now !== last) { last = now; post({ audible: now }); }
  };
  const silence = (m) => { if (!m.muted) { m.muted = true; m.__featherMuted = true; } };
  for (const type of ['play', 'playing', 'pause', 'ended', 'volumechange', 'emptied']) {
    document.addEventListener(type, (e) => {
      if (muted && type === 'play' && e.target instanceof HTMLMediaElement) silence(e.target);
      report();
    }, true);
  }
  window.__featherSetMuted = (on) => {
    muted = on;
    for (const m of media()) {
      if (on) silence(m);
      else if (m.__featherMuted) { m.muted = false; m.__featherMuted = false; }
    }
    report();
  };
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
    Back,
    Forward,
    Reload,
    Home,
    /// L'interface ouvre (ou ferme) un panneau : il faut cacher la page.
    Expand {
        open: bool,
    },
    /// L'utilisateur a quitté la barre : rendre le focus à la page.
    FocusPage,
    /// Ctrl+L : barre de l'accueil si l'onglet est sur l'accueil, sinon barre par-dessus la page.
    FocusBar,
    SaveSettings {
        settings: Settings,
    },
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
    NextTab,
    PreviousTab,
    Settings,
}

/// Zone occupée par les pages : tout sauf la colonne de gauche.
pub fn page_bounds(window: &Window) -> Rect {
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    Rect {
        position: LogicalPosition::new(SIDEBAR_WIDTH, 0.0).into(),
        size: LogicalSize::new((size.width - SIDEBAR_WIDTH).max(1.0), size.height.max(1.0)).into(),
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
pub fn home_page(settings: &Settings) -> String {
    let theme = match settings.theme {
        Theme::Auto => "auto",
        Theme::Light => "light",
        Theme::Dark => "dark",
    };
    HOME_HTML
        .replace("{{THEME}}", theme)
        .replace("{{NAME}}", &escape_html(&settings.name))
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
    }

    #[test]
    fn accueil_echappe_le_nom() {
        let s = Settings {
            name: "<b>Léa</b>".into(),
            ..Default::default()
        };
        let html = home_page(&s);
        assert!(html.contains("&lt;b&gt;Léa&lt;/b&gt;"));
        assert!(!html.contains("{{NAME}}"));
    }
}
