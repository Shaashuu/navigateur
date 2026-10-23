//! Capture de la page visible (`/capture`, Ctrl+Maj+S) : une image PNG rangée
//! dans le dossier des téléchargements, ou copiée dans le presse-papiers
//! (`/capture copier`).
//!
//! Le moteur fait l'image lui-même : `CapturePreview` sous Windows,
//! `takeSnapshot` sous macOS. Rien sous Linux (Feather n'y est pas distribué).
//! Le résultat revient à la boucle d'événements par `UserEvent::Captured`.

use std::path::PathBuf;

use tao::event_loop::EventLoopProxy;
use wry::WebView;

use crate::UserEvent;

/// Où va l'image.
pub enum Target {
    File(PathBuf),
    Clipboard,
}

/// Vrai si le système permet la capture.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

/// Décalage de l'heure locale sur l'heure UTC, en secondes, à l'instant
/// `epoch_secs` (heure d'été comprise). Le nom des captures porte ainsi l'heure
/// de l'horloge de l'ordinateur : avant, une capture de 16 h à Paris était
/// nommée 14 h (UTC).
#[cfg(target_os = "macos")]
pub fn local_offset_secs(epoch_secs: u64) -> i64 {
    let time = epoch_secs as libc::time_t;
    // SAFETY : `tm` est une structure C ordinaire, entièrement remplie par localtime_r.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY : pointeurs valides pendant l'appel ; localtime_r est réentrant.
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff
}

#[cfg(windows)]
pub fn local_offset_secs(_epoch_secs: u64) -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut zone = TIME_ZONE_INFORMATION::default();
    // SAFETY : structure valide, remplie par le système.
    let state = unsafe { GetTimeZoneInformation(&mut zone) };
    // Biais en minutes (heure UTC = heure locale + biais) ; 1 : heure d'hiver, 2 : heure d'été.
    let bias = zone.Bias
        + match state {
            1 => zone.StandardBias,
            2 => zone.DaylightBias,
            _ => 0,
        };
    -i64::from(bias) * 60
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn local_offset_secs(_epoch_secs: u64) -> i64 {
    0
}

/// Nom du fichier : « wikipedia.org-2026-10-20-142530.png ». `epoch_secs` est
/// déjà à l'heure locale (voir `local_offset_secs`).
pub fn file_name(host: &str, epoch_secs: u64) -> String {
    let host: String = host
        .trim_start_matches("www.")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' })
        .collect();
    let host = if host.is_empty() { "page".to_string() } else { host };
    let (y, m, d) = civil_date(epoch_secs / 86_400);
    let t = epoch_secs % 86_400;
    format!("{host}-{y:04}-{m:02}-{d:02}-{:02}{:02}{:02}.png", t / 3600, t / 60 % 60, t % 60)
}

/// Jour depuis 1970 → (année, mois, jour), calendrier grégorien (H. Hinnant).
fn civil_date(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Enregistre l'image PNG, ou la confie à `copy` (presse-papiers). `Some(chemin)` :
/// le fichier écrit ; `None` : copiée.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn deliver(png: &[u8], target: Target, copy: impl FnOnce(&[u8]) -> Result<(), String>) -> Result<Option<PathBuf>, String> {
    if png.is_empty() {
        return Err("Capture impossible : image vide".into());
    }
    match target {
        Target::File(path) => {
            std::fs::write(&path, png).map_err(|e| format!("Capture non enregistrée : {e}"))?;
            Ok(Some(path))
        }
        Target::Clipboard => {
            copy(png)?;
            Ok(None)
        }
    }
}

/// Capture la partie visible de la page ; le résultat arrive plus tard.
pub fn capture(webview: &WebView, target: Target, proxy: EventLoopProxy<UserEvent>) {
    platform::capture(webview, target, proxy);
}

#[cfg(windows)]
mod platform {
    use tao::event_loop::EventLoopProxy;
    use webview2_com::CapturePreviewCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
    use windows::Win32::System::Com::{IStream, STREAM_SEEK_CUR};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::core::w;
    use wry::{WebView, WebViewExtWindows};

    use super::{Target, deliver};
    use crate::UserEvent;

    /// Octets écrits dans un flux en mémoire.
    unsafe fn stream_bytes(stream: &IStream) -> windows::core::Result<Vec<u8>> {
        // SAFETY : le flux est valide ; on lit au plus la longueur écrite.
        unsafe {
            let mut len = 0u64;
            stream.Seek(0, STREAM_SEEK_CUR, Some(&mut len))?;
            let global = GetHGlobalFromStream(stream)?;
            let ptr = GlobalLock(global) as *const u8;
            if ptr.is_null() {
                return Ok(Vec::new());
            }
            let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            let _ = GlobalUnlock(global);
            Ok(bytes)
        }
    }

    pub fn capture(webview: &WebView, target: Target, proxy: EventLoopProxy<UserEvent>) {
        let core = webview.webview();
        // Le presse-papiers demande une fenêtre propriétaire : celle de la page.
        let owner = webview.hwnd().0 as isize;
        // SAFETY : appels COM sur le fil de la webview, objets valides.
        let started = unsafe {
            CreateStreamOnHGlobal(HGLOBAL::default(), true).and_then(|stream| {
                let written = stream.clone();
                let done = CapturePreviewCompletedHandler::create(Box::new(move |result| {
                    let outcome = result
                        .and_then(|()| stream_bytes(&written))
                        .map_err(|e| format!("Capture impossible : {e}"))
                        .and_then(|png| deliver(&png, target, |png| copy_png(png, HWND(owner as *mut _))));
                    let _ = proxy.send_event(UserEvent::Captured(outcome));
                    Ok(())
                }));
                core.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &done)
            })
        };
        if let Err(e) = started {
            eprintln!("capture impossible : {e}");
        }
    }

    /// Presse-papiers au format « PNG » (reconnu par Word, les navigateurs,
    /// les messageries…).
    fn copy_png(png: &[u8], owner: HWND) -> Result<(), String> {
        // SAFETY : presse-papiers ouvert puis refermé ; la mémoire confiée à
        // SetClipboardData appartient ensuite au système.
        unsafe {
            OpenClipboard(Some(owner)).map_err(|e| e.to_string())?;
            let result = (|| {
                EmptyClipboard()?;
                let format = RegisterClipboardFormatW(w!("PNG"));
                let global = GlobalAlloc(GMEM_MOVEABLE, png.len())?;
                let ptr = GlobalLock(global) as *mut u8;
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(png.as_ptr(), ptr, png.len());
                    let _ = GlobalUnlock(global);
                }
                SetClipboardData(format, Some(HANDLE(global.0)))?;
                Ok::<(), windows::core::Error>(())
            })();
            let _ = CloseClipboard();
            result.map_err(|e| e.to_string())
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSPasteboard, NSPasteboardTypePNG};
    use objc2_foundation::{NSData, NSDictionary, NSError};
    use tao::event_loop::EventLoopProxy;
    use wry::{WebView, WebViewExtMacOS};

    use super::{Target, deliver};
    use crate::UserEvent;

    fn png_of(image: &NSImage) -> Option<Vec<u8>> {
        // SAFETY : objets AppKit valides, appelés sur le fil principal.
        unsafe {
            let tiff = image.TIFFRepresentation()?;
            let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
            let png = rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())?;
            Some(png.to_vec())
        }
    }

    pub fn capture(webview: &WebView, target: Target, proxy: EventLoopProxy<UserEvent>) {
        let wv = webview.webview();
        // Le bloc doit pouvoir être appelé plusieurs fois : la cible n'est prise qu'une fois.
        let target = std::cell::Cell::new(Some(target));
        let done = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
            let Some(target) = target.take() else { return };
            // SAFETY : WebKit passe une image valide (ou nulle en cas d'échec).
            let image: Option<Retained<NSImage>> = unsafe { Retained::retain(image) };
            let outcome = image
                .as_deref()
                .and_then(png_of)
                .ok_or_else(|| "Capture impossible".to_string())
                .and_then(|png| deliver(&png, target, copy_png));
            let _ = proxy.send_event(UserEvent::Captured(outcome));
        });
        // SAFETY : appelé sur le fil principal ; le bloc est gardé par WebKit jusqu'à son appel.
        unsafe { wv.takeSnapshotWithConfiguration_completionHandler(None, &done) };
    }

    fn copy_png(png: &[u8]) -> Result<(), String> {
        // SAFETY : presse-papiers général, sur le fil principal.
        unsafe {
            let board = NSPasteboard::generalPasteboard();
            board.clearContents();
            let data = NSData::with_bytes(png);
            if board.setData_forType(Some(&data), NSPasteboardTypePNG) {
                Ok(())
            } else {
                Err("Presse-papiers indisponible".to_string())
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use tao::event_loop::EventLoopProxy;
    use wry::WebView;

    use super::Target;
    use crate::UserEvent;

    pub fn capture(_webview: &WebView, _target: Target, proxy: EventLoopProxy<UserEvent>) {
        let _ = proxy.send_event(UserEvent::Captured(Err("La capture n'est pas encore possible sous Linux".into())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nom_du_fichier() {
        // 2026-10-20 14:25:30 UTC
        assert_eq!(file_name("www.fr.wikipedia.org", 1_792_506_330), "fr.wikipedia.org-2026-10-20-142530.png");
        assert_eq!(file_name("", 0), "page-1970-01-01-000000.png");
        assert_eq!(file_name("a/b:c", 951_782_400), "a-b-c-2000-02-29-000000.png");
    }

    #[test]
    fn decalage_horaire_plausible() {
        // Entre UTC-12 et UTC+14, en quarts d'heure (fuseaux de l'Inde, du Népal…).
        let offset = local_offset_secs(1_792_506_330);
        assert!((-12 * 3600..=14 * 3600).contains(&offset), "{offset}");
        assert_eq!(offset % 900, 0);
    }
}
