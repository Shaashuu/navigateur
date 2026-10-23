//! macOS : menu Fichier (Enregistrer la page, Exporter en PDF, Ouvrir un
//! fichier, Partager).
//!
//! WebKit fait lui-même l'archive de la page (`.webarchive`, celle de Safari)
//! et le PDF de toute la page. Les fichiers sont rangés dans le dossier des
//! téléchargements, comme les captures, et le résultat revient à la boucle
//! d'événements par `UserEvent::Exported`.

use std::path::PathBuf;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSModalResponseOK, NSOpenPanel, NSSharingServicePicker};
use objc2_foundation::{NSArray, NSData, NSError, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL};
use tao::event_loop::EventLoopProxy;
use wry::{WebView, WebViewExtMacOS};

use crate::UserEvent;

thread_local! {
    /// Le menu de partage ouvert, gardé en vie tant qu'il est affiché.
    static PICKER: std::cell::RefCell<Option<Retained<NSSharingServicePicker>>> = const { std::cell::RefCell::new(None) };
}

/// Ce que WebKit fabrique à partir de la page.
#[derive(Clone, Copy)]
pub enum Export {
    /// Archive Safari (`.webarchive`) : la page et ses images, lisible hors ligne.
    Archive,
    /// PDF de toute la page.
    Pdf,
}

impl Export {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Archive => "webarchive",
            Self::Pdf => "pdf",
        }
    }
}

/// Écrit l'archive ou le PDF de la page dans `path` ; le résultat arrive plus tard.
pub fn export(webview: &WebView, kind: Export, path: PathBuf, proxy: EventLoopProxy<UserEvent>) {
    let wv = webview.webview();
    // Le bloc doit pouvoir être appelé plusieurs fois : le chemin n'est pris qu'une fois.
    let path = std::cell::Cell::new(Some(path));
    let done = RcBlock::new(move |data: *mut NSData, _error: *mut NSError| {
        let Some(path) = path.take() else { return };
        // SAFETY : WebKit passe des données valides (ou nulles en cas d'échec).
        let data: Option<Retained<NSData>> = unsafe { Retained::retain(data) };
        let outcome = match data.map(|d| d.to_vec()) {
            Some(bytes) if !bytes.is_empty() => std::fs::write(&path, bytes)
                .map(|()| path)
                .map_err(|e| format!("Page non enregistrée : {e}")),
            _ => Err("WebKit n'a pas pu enregistrer cette page".to_string()),
        };
        let _ = proxy.send_event(UserEvent::Exported(outcome));
    });
    // SAFETY : appelé sur le fil principal ; le bloc est gardé par WebKit jusqu'à son appel.
    unsafe {
        match kind {
            Export::Archive => wv.createWebArchiveDataWithCompletionHandler(&done),
            Export::Pdf => wv.createPDFWithConfiguration_completionHandler(None, &done),
        }
    }
}

/// Fenêtre « Ouvrir » de macOS : l'adresse `file://` du fichier choisi.
pub fn choose_file() -> Option<String> {
    let mtm = MainThreadMarker::new()?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    let url = panel.URLs().firstObject()?;
    url.absoluteString().map(|s| s.to_string())
}

/// Charge le fichier choisi dans la page. WebKit ne lit un fichier de
/// l'ordinateur que par `loadFileURL` (un simple chargement d'adresse
/// `file://` peut rester blanc) ; le dossier du fichier est lisible aussi,
/// pour ses images et ses feuilles de style.
pub fn load_file(webview: &WebView, url: &str) {
    let Some(file) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
    let Some(dir) = file.URLByDeletingLastPathComponent() else { return };
    // SAFETY : appelé sur le fil principal, adresses `file://` valides.
    unsafe {
        let _ = webview.webview().loadFileURL_allowingReadAccessToURL(&file, &dir);
    }
}

/// Menu de partage de macOS (Mail, Messages, AirDrop, Notes…) pour cette
/// adresse, ouvert en haut de la page.
pub fn share(webview: &WebView, url: &str) {
    // AppKit : seulement sur le fil principal.
    if MainThreadMarker::new().is_none() {
        return;
    }
    let Some(link) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
    let wv = webview.webview();
    let items: Retained<NSArray<AnyObject>> = NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(link))]);
    // SAFETY : un tableau d'objets NSURL, ce que le menu de partage attend.
    let picker = unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
    let bounds = wv.bounds();
    // Un point au milieu du haut de la page ; le menu s'ouvre en dessous. Le
    // haut est y = 0 dans une vue retournée (WKWebView), y = hauteur sinon.
    let flipped = wv.isFlipped();
    let top = if flipped { bounds.origin.y } else { bounds.origin.y + bounds.size.height - 1.0 };
    let anchor = NSRect::new(NSPoint::new(bounds.size.width / 2.0, top), NSSize::new(1.0, 1.0));
    let edge = if flipped { NSRectEdge::MaxY } else { NSRectEdge::MinY };
    picker.showRelativeToRect_ofView_preferredEdge(anchor, &wv, edge);
    PICKER.with_borrow_mut(|p| *p = Some(picker));
}
