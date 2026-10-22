//! Ce que le moteur sait du son et de la caméra d'une page, cadres compris.
//!
//! `assets/page.js` ne voit que les balises `video` et `audio` de la page
//! elle-même : un lecteur YouTube intégré à un blog, Spotify ou SoundCloud en
//! cadre, ou un son fait en WebAudio, passaient inaperçus, et la veille
//! détruisait l'onglet en pleine écoute. Le moteur, lui, sait :
//!
//! - Windows : `IsDocumentPlayingAudio` (toute la page, cadres et WebAudio
//!   compris), et `IsMuted` pour couper le son de tout l'onglet ;
//! - macOS 12 ou plus : l'état de lecture des médias de la page (sans savoir
//!   s'ils ont du son) et l'état de la caméra et du micro.
//!
//! Ailleurs, rien : on garde ce que dit la page.

use tao::event_loop::EventLoopProxy;
use wry::WebView;

use crate::UserEvent;
use crate::tab_manager::TabId;

/// État lu dans le moteur, envoyé par `UserEvent::Media`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaState {
    /// Windows : la page fait du son. macOS : un média de la page joue.
    pub playing: bool,
    /// Caméra ou micro en cours d'utilisation (macOS).
    pub capturing: bool,
}

/// Demande au moteur l'état de la page ; la réponse arrive par `UserEvent::Media`.
pub fn probe(webview: &WebView, id: TabId, proxy: &EventLoopProxy<UserEvent>) {
    imp::probe(webview, id, proxy);
}

/// Coupe ou remet le son de tout l'onglet, cadres compris (Windows). Ailleurs,
/// seul `assets/page.js` coupe les vidéos et musiques de la page.
pub fn set_muted(webview: &WebView, muted: bool) {
    imp::set_muted(webview, muted);
}

#[cfg(windows)]
mod imp {
    use tao::event_loop::EventLoopProxy;
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_8;
    use windows::core::{BOOL, Interface};
    use wry::{WebView, WebViewExtWindows};

    use super::MediaState;
    use crate::UserEvent;
    use crate::tab_manager::TabId;

    pub fn probe(webview: &WebView, id: TabId, proxy: &EventLoopProxy<UserEvent>) {
        let mut playing = BOOL::default();
        // SAFETY : appel COM sur le fil de la webview, objet valide.
        let ok = unsafe {
            webview
                .webview()
                .cast::<ICoreWebView2_8>()
                .and_then(|core| core.IsDocumentPlayingAudio(&mut playing))
                .is_ok()
        };
        if ok {
            let state = MediaState { playing: playing.as_bool(), capturing: false };
            let _ = proxy.send_event(UserEvent::Media(id, state));
        }
    }

    pub fn set_muted(webview: &WebView, muted: bool) {
        // SAFETY : appel COM sur le fil de la webview, objet valide.
        unsafe {
            if let Ok(core) = webview.webview().cast::<ICoreWebView2_8>() {
                let _ = core.SetIsMuted(muted);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use block2::RcBlock;
    use objc2_web_kit::{WKMediaCaptureState, WKMediaPlaybackState};
    use tao::event_loop::EventLoopProxy;
    use wry::{WebView, WebViewExtMacOS};

    use super::MediaState;
    use crate::UserEvent;
    use crate::tab_manager::TabId;

    pub fn probe(webview: &WebView, id: TabId, proxy: &EventLoopProxy<UserEvent>) {
        if !objc2::available!(macos = 12.0) {
            return;
        }
        let wv = webview.webview();
        // SAFETY : fil principal, webview valide ; propriétés de macOS 12.
        let capturing = unsafe {
            wv.cameraCaptureState() == WKMediaCaptureState::Active
                || wv.microphoneCaptureState() == WKMediaCaptureState::Active
        };
        let proxy = proxy.clone();
        let done = RcBlock::new(move |state: WKMediaPlaybackState| {
            let state = MediaState { playing: state == WKMediaPlaybackState::Playing, capturing };
            let _ = proxy.send_event(UserEvent::Media(id, state));
        });
        // SAFETY : fil principal ; le bloc est gardé par WebKit jusqu'à son appel.
        unsafe { wv.requestMediaPlaybackStateWithCompletionHandler(&done) };
    }

    pub fn set_muted(_webview: &WebView, _muted: bool) {}
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use tao::event_loop::EventLoopProxy;
    use wry::WebView;

    use crate::UserEvent;
    use crate::tab_manager::TabId;

    pub fn probe(_webview: &WebView, _id: TabId, _proxy: &EventLoopProxy<UserEvent>) {}

    pub fn set_muted(_webview: &WebView, _muted: bool) {}
}
