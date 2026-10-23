//! Windows : WebView2 fait tourner les extensions Chrome (et Edge),
//! décompressées, dans le profil commun à toutes les pages.

use std::cell::RefCell;
use std::path::PathBuf;

use tao::event_loop::EventLoopProxy;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2_13, ICoreWebView2BrowserExtension, ICoreWebView2Profile7,
};
use webview2_com::{
    BrowserExtensionEnableCompletedHandler, BrowserExtensionRemoveCompletedHandler,
    ProfileAddBrowserExtensionCompletedHandler, ProfileGetBrowserExtensionsCompletedHandler, take_pwstr,
};
use windows::core::{BOOL, HSTRING, Interface, PWSTR};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

use super::Extension;
use crate::UserEvent;

pub const PLATFORM: &str = "windows";

thread_local! {
    /// Profil WebView2 commun aux pages non privées (celui de l'interface).
    static PROFILE: RefCell<Option<ICoreWebView2Profile7>> = const { RefCell::new(None) };
}

pub fn support() -> Result<(), &'static str> {
    Ok(())
}

/// « Choisir un fichier » : la fenêtre « Ouvrir » de Windows, limitée aux
/// extensions, devant la fenêtre de Feather qui attend la réponse. tao sait
/// recevoir les événements de Windows pendant ce temps (boucle imbriquée).
pub fn choose_archive(chrome: &WebView, chosen: impl Fn(PathBuf) + 'static) {
    if let Some(path) = open_dialog(chrome) {
        chosen(path);
    }
}

fn open_dialog(chrome: &WebView) -> Option<PathBuf> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor};
    use windows::core::w;

    let types = [COMDLG_FILTERSPEC { pszName: w!("Extensions (.crx, .xpi, .zip)"), pszSpec: w!("*.crx;*.xpi;*.zip") }];
    // SAFETY : fil de l'interface, où COM est déjà prêt (WebView2 en dépend) ;
    // le chemin rendu par Windows est copié, puis libéré.
    unsafe {
        let owner = GetAncestor(chrome.hwnd(), GA_ROOT);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        dialog.SetFileTypes(&types).ok()?;
        dialog.SetOptions(dialog.GetOptions().ok()? | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM).ok()?;
        // Fenêtre fermée sans choix : une erreur, donc rien.
        dialog.Show(Some(owner)).ok()?;
        let text = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        path.map(PathBuf::from)
    }
}

pub fn prepare_builder<'a>(builder: WebViewBuilder<'a>, _attach: bool, _url: &str) -> WebViewBuilder<'a> {
    builder.with_browser_extensions_enabled(true)
}

fn profile() -> Option<ICoreWebView2Profile7> {
    PROFILE.with_borrow(|p| p.clone())
}

/// Les extensions que le profil connaît : (identifiant WebView2, objet).
fn installed(done: impl FnOnce(Vec<(String, ICoreWebView2BrowserExtension)>) + 'static) {
    let Some(profile) = profile() else { return };
    let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |result, list| {
        let mut found = Vec::new();
        if let (Ok(()), Some(list)) = (result, list) {
            // SAFETY : liste valide donnée par WebView2, lue sur son fil.
            unsafe {
                let mut count = 0u32;
                let _ = list.Count(&mut count);
                for i in 0..count {
                    let Ok(ext) = list.GetValueAtIndex(i) else { continue };
                    let mut id = PWSTR::null();
                    if ext.Id(&mut id).is_ok() {
                        found.push((take_pwstr(id), ext));
                    }
                }
            }
        }
        done(found);
        Ok(())
    }));
    // SAFETY : profil valide, rappel conservé par WebView2 jusqu'à son appel.
    unsafe {
        let _ = profile.GetBrowserExtensions(&handler);
    }
}

/// Au démarrage : retrouve le profil, puis confie à WebView2 les extensions
/// qu'il ne connaît pas (il garde les autres d'une fois sur l'autre).
pub fn start(chrome: &WebView, list: &[Extension], proxy: &EventLoopProxy<UserEvent>) {
    let core = chrome.webview();
    // SAFETY : appels COM sur le fil de la webview.
    let profile = unsafe { core.cast::<ICoreWebView2_13>().and_then(|c| c.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile7>()) };
    let Ok(profile) = profile else {
        eprintln!("extensions indisponibles : WebView2 trop ancien");
        return;
    };
    PROFILE.with_borrow_mut(|p| *p = Some(profile));
    let list = list.to_vec();
    let proxy = proxy.clone();
    installed(move |found| {
        for ext in list {
            match found.iter().find(|(id, _)| Some(id) == ext.engine_id.as_ref()) {
                Some((id, engine)) => {
                    enable(engine, ext.enabled);
                    let _ = proxy.send_event(UserEvent::ExtensionLoaded { id: ext.id.clone(), result: Ok(Some(id.clone())) });
                }
                None => add(&ext, &proxy),
            }
        }
    });
}

fn enable(engine: &ICoreWebView2BrowserExtension, on: bool) {
    // SAFETY : extension valide, rappel conservé par WebView2.
    unsafe {
        let mut current = BOOL::default();
        if engine.IsEnabled(&mut current).is_ok() && current.as_bool() == on {
            return;
        }
        let _ = engine.Enable(on, &BrowserExtensionEnableCompletedHandler::create(Box::new(|_| Ok(()))));
    }
}

pub fn add(ext: &Extension, proxy: &EventLoopProxy<UserEvent>) {
    let (Some(profile), Some(path)) = (profile(), ext.path()) else { return };
    let id = ext.id.clone();
    let enabled = ext.enabled;
    let done = proxy.clone();
    let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |result, engine| {
        let result = match (result, engine) {
            (Ok(()), Some(engine)) => {
                if !enabled {
                    enable(&engine, false);
                }
                let mut eid = PWSTR::null();
                // SAFETY : extension valide donnée par WebView2.
                unsafe { engine.Id(&mut eid) }.map(|()| Some(take_pwstr(eid))).map_err(|e| e.message())
            }
            (Err(e), _) => Err(e.message()),
            _ => Err("refusée par WebView2".into()),
        };
        let _ = done.send_event(UserEvent::ExtensionLoaded { id, result });
        Ok(())
    }));
    // SAFETY : profil valide, chemin en UTF-16, rappel conservé par WebView2.
    unsafe {
        if let Err(err) = profile.AddBrowserExtension(&HSTRING::from(path.as_path()), &handler) {
            let _ = proxy.send_event(UserEvent::ExtensionLoaded { id: ext.id.clone(), result: Err(err.message()) });
        }
    }
}

pub fn set_enabled(ext: &Extension, on: bool) {
    let Some(engine_id) = ext.engine_id.clone() else { return };
    installed(move |found| {
        if let Some((_, engine)) = found.iter().find(|(id, _)| *id == engine_id) {
            enable(engine, on);
        }
    });
}

pub fn remove(ext: &Extension) {
    remove_then(ext, || {});
}

/// Retire l'extension du profil, puis fait `then` (même si elle n'y était pas).
fn remove_then(ext: &Extension, then: impl FnOnce() + 'static) {
    let Some(engine_id) = ext.engine_id.clone() else {
        then();
        return;
    };
    installed(move |found| match found.iter().find(|(id, _)| *id == engine_id) {
        Some((_, engine)) => {
            let then = std::cell::Cell::new(Some(then));
            let done = BrowserExtensionRemoveCompletedHandler::create(Box::new(move |_| {
                if let Some(then) = then.take() {
                    then();
                }
                Ok(())
            }));
            // SAFETY : extension valide, rappel conservé par WebView2.
            unsafe {
                let _ = engine.Remove(&done);
            }
        }
        None => then(),
    });
}

/// Nouvelle version : la nouvelle n'est ajoutée qu'une fois l'ancienne retirée
/// (même identifiant : sinon le retrait tardif emportait la nouvelle).
pub fn replace(old: &Extension, new: &Extension, proxy: &EventLoopProxy<UserEvent>) {
    let new = new.clone();
    let proxy = proxy.clone();
    remove_then(old, move || add(&new, &proxy));
}

/// WebView2 ne tient pas de liste des erreurs des extensions.
pub fn errors() -> Vec<(String, Vec<String>)> {
    Vec::new()
}

/// WebView2 ne confie pas les demandes d'autorisation à Feather.
pub fn answer(_number: u32, _allow: bool) {}

/// Adresse d'une page de l'extension (`popup.html`…), une fois chargée.
pub fn page_url(ext: &Extension, page: &str) -> Option<String> {
    Some(format!("chrome-extension://{}/{page}", ext.engine_id.as_ref()?))
}

pub fn options_url(ext: &Extension) -> Option<String> {
    page_url(ext, ext.options.as_ref()?)
}

/// Adresse de la petite fenêtre : la webview est créée par `App` (voir main.rs).
pub fn open_popup(
    ext: &Extension,
    _button: (f64, f64, f64, f64),
    _frame: (f64, f64, f64, f64),
    _bar: &str,
    _chrome: &WebView,
) -> Result<Option<String>, String> {
    let page = ext.popup.as_ref().ok_or("Cette extension n'a pas de fenêtre à ouvrir.")?;
    page_url(ext, page).map(Some).ok_or_else(|| "L'extension est encore en cours de chargement.".into())
}

pub fn close_popup() {}

pub fn popup_shown() -> bool {
    true
}
