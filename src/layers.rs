//! Panneaux par-dessus la page (macOS et Windows).
//!
//! L'interface est une webview dessinée sous les pages. Quand un panneau
//! s'ouvre (recherche, paramètres, favoris…), on la passe au-dessus : la page
//! reste visible derrière le panneau au lieu d'être cachée.
//!
//! - Windows : une fenêtre enfant ne laisse pas voir ses voisines à travers
//!   ses zones transparentes. On découpe donc l'interface à la forme de la
//!   barre et des panneaux (`SetWindowRgn`) : ailleurs, on voit et on clique
//!   la page.
//! - macOS : les zones transparentes de l'interface laissent voir la page.
//! - Linux : pas d'ordre réglable entre les webviews GTK ; la page est cachée
//!   pendant qu'un panneau est ouvert, comme avant.
//!
//! Pour un nouveau panneau, rien à faire ici : il suffit que l'interface
//! l'envoie dans sa forme (voir `reportShape` dans chrome.html).

use serde::Deserialize;
use tao::window::Window;
use wry::WebView;

/// Les panneaux passent par-dessus la page sur ce système.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));

/// Rayon des coins de la page, posée en carte à côté de la barre : le même
/// arrondi que les pastilles des onglets (11 px dans chrome.html).
const PAGE_RADIUS: f64 = 11.0;

/// Arrondit les coins d'une page (macOS et Windows ; Linux : coins droits).
/// À rappeler après chaque changement de taille (Windows découpe à la taille du moment).
pub fn round_page(page: &WebView) {
    platform::round(page, PAGE_RADIUS);
}

/// Au plus ce nombre de rectangles (la barre, le bandeau, quelques panneaux).
const MAX_RECTS: usize = 16;

/// Un rectangle de l'interface, en pixels CSS depuis le coin haut gauche.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct ShapeRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Rayon des coins arrondis.
    #[serde(default)]
    pub r: f64,
}

impl ShapeRect {
    fn valid(&self) -> bool {
        [self.x, self.y, self.w, self.h, self.r].iter().all(|v| v.is_finite())
            && self.w > 0.0
            && self.h > 0.0
            && self.r >= 0.0
    }
}

/// Garde seulement les rectangles utilisables (l'interface peut se tromper).
pub fn clean(mut rects: Vec<ShapeRect>) -> Vec<ShapeRect> {
    rects.retain(ShapeRect::valid);
    rects.truncate(MAX_RECTS);
    rects
}

/// État des couches : l'interface est-elle au-dessus, et avec quelle forme.
#[derive(Default)]
pub struct Layers {
    on_top: bool,
    shape: Vec<ShapeRect>,
}

impl Layers {
    /// Passe l'interface au-dessus des pages (`true`) ou en dessous.
    pub fn set_on_top(&mut self, window: &Window, chrome: &WebView, top: bool) {
        if !SUPPORTED {
            return;
        }
        self.on_top = top;
        platform::raise(chrome, top);
        self.apply(window, chrome);
    }

    /// Une page vient d'être créée au-dessus de tout : on remet l'interface devant.
    pub fn restack(&mut self, window: &Window, chrome: &WebView) {
        if self.on_top {
            platform::raise(chrome, true);
            self.apply(window, chrome);
        }
    }

    /// Nouvelle forme de l'interface (la barre et les panneaux ouverts).
    pub fn set_shape(&mut self, window: &Window, chrome: &WebView, rects: Vec<ShapeRect>) {
        let rects = clean(rects);
        if rects == self.shape {
            return;
        }
        self.shape = rects;
        self.apply(window, chrome);
    }

    /// Applique la forme : découpe de l'interface (Windows seulement).
    #[cfg_attr(target_os = "macos", allow(unused_variables))]
    fn apply(&mut self, window: &Window, chrome: &WebView) {
        let shape: &[ShapeRect] = if self.on_top { &self.shape } else { &[] };
        #[cfg(not(target_os = "macos"))]
        platform::clip(chrome, shape, window.scale_factor());
    }
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, RGN_OR, SetWindowRgn};
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, HWND_BOTTOM, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };
    use wry::{WebView, WebViewExtWindows};

    use super::ShapeRect;

    /// Découpe la page en rectangle arrondi, à sa taille actuelle.
    pub fn round(page: &WebView, radius: f64) {
        let hwnd = page.hwnd();
        unsafe {
            let mut rect = RECT::default();
            if GetClientRect(hwnd, &mut rect).is_err() || rect.right <= 0 || rect.bottom <= 0 {
                return;
            }
            let scale = f64::from(GetDpiForWindow(hwnd).max(96)) / 96.0;
            let d = (radius * 2.0 * scale).round() as i32;
            // Les coordonnées de fin sont exclues : +1 pour ne pas rogner le bord.
            let region = CreateRoundRectRgn(0, 0, rect.right + 1, rect.bottom + 1, d, d);
            // Windows garde la région : on ne la libère pas nous-mêmes.
            SetWindowRgn(hwnd, Some(region), true);
        }
    }

    pub fn raise(chrome: &WebView, top: bool) {
        let hwnd = chrome.hwnd();
        let after = if top { HWND_TOP } else { HWND_BOTTOM };
        unsafe {
            let _ = SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    /// Découpe l'interface : seuls ces rectangles restent visibles et cliquables.
    /// Sans rectangle (interface sous les pages), elle redevient entière.
    pub fn clip(chrome: &WebView, shape: &[ShapeRect], scale: f64) {
        let hwnd = chrome.hwnd();
        unsafe {
            if shape.is_empty() {
                SetWindowRgn(hwnd, None, true);
                return;
            }
            let px = |v: f64| (v * scale).round() as i32;
            let region = CreateRectRgn(0, 0, 0, 0);
            for r in shape {
                let (x, y) = (px(r.x), px(r.y));
                let (x2, y2) = (px(r.x + r.w), px(r.y + r.h));
                let d = px(r.r * 2.0);
                let part = if d > 0 {
                    // Les coordonnées de fin sont exclues : +1 pour ne pas rogner le bord.
                    CreateRoundRectRgn(x, y, x2 + 1, y2 + 1, d, d)
                } else {
                    CreateRectRgn(x, y, x2, y2)
                };
                CombineRgn(Some(region), Some(region), Some(part), RGN_OR);
                let _ = DeleteObject(part.into());
            }
            // Windows garde la région : on ne la libère pas nous-mêmes.
            SetWindowRgn(hwnd, Some(region), true);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSView;
    use objc2_foundation::NSArray;
    use wry::{WebView, WebViewExtMacOS};

    /// Coins arrondis par le calque de la vue : ils suivent les changements de taille.
    pub fn round(page: &WebView, radius: f64) {
        let webview = page.webview();
        let view: &NSView = &webview;
        view.setWantsLayer(true);
        let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
        if layer.is_null() {
            return;
        }
        unsafe {
            let _: () = msg_send![layer, setCornerRadius: radius];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }

    /// `setSubviews` réordonne les vues sans les retirer de la fenêtre.
    /// Avant, `addSubview:positioned:` retirait l'interface puis la remettait :
    /// WebKit la redessinait de zéro et l'écran « sautait » à chaque panneau,
    /// et la souris n'était plus suivie (la barre au survol restait ouverte).
    pub fn raise(chrome: &WebView, top: bool) {
        let webview = chrome.webview();
        let view: &NSView = &webview;
        let Some(parent) = (unsafe { view.superview() }) else { return };
        let mut views: Vec<Retained<NSView>> = parent.subviews().to_vec();
        let Some(i) = views.iter().position(|v| std::ptr::eq(&**v, view)) else { return };
        let me = views.remove(i);
        if top {
            views.push(me);
        } else {
            views.insert(0, me);
        }
        parent.setSubviews(&NSArray::from_retained_slice(&views));
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use wry::WebView;

    use super::ShapeRect;

    pub fn raise(_chrome: &WebView, _top: bool) {}

    pub fn round(_page: &WebView, _radius: f64) {}

    pub fn clip(_chrome: &WebView, _shape: &[ShapeRect], _scale: f64) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangles_invalides_ignores() {
        let ok = ShapeRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0, r: 4.0 };
        let rects = vec![
            ok,
            ShapeRect { w: 0.0, ..ok },
            ShapeRect { x: f64::NAN, ..ok },
            ShapeRect { r: -1.0, ..ok },
        ];
        assert_eq!(clean(rects), vec![ok]);
        assert_eq!(clean(vec![ok; 40]).len(), MAX_RECTS);
        let parsed: Vec<ShapeRect> = serde_json::from_str(r#"[{"x":1,"y":2,"w":3,"h":4}]"#).unwrap();
        assert_eq!(parsed[0].r, 0.0);
    }
}
