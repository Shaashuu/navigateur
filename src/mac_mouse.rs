//! macOS : WebKit ne transmet pas les boutons latéraux de la souris aux pages.
//! On les intercepte donc au niveau de l'application.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};
use tao::event_loop::EventLoopProxy;

use crate::UserEvent;

/// À appeler depuis le fil principal. Le bouton 3 (précédent) et le bouton 4
/// (suivant) déclenchent les mêmes messages que les flèches de l'interface.
pub fn install(proxy: EventLoopProxy<UserEvent>) {
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let diag = std::env::var_os("FEATHER_DIAG").is_some();
        let (kind_id, button) = unsafe {
            let e = event.as_ref();
            (e.r#type(), e.buttonNumber())
        };
        if diag {
            eprintln!("[mac_mouse] type={kind_id:?} bouton={button}");
        }
        let kind = match button {
            3 => "back",
            4 => "forward",
            _ => return event.as_ptr(),
        };
        if kind_id != NSEventType::OtherMouseUp {
            return event.as_ptr();
        }
        let _ = proxy.send_event(UserEvent::Chrome(format!(r#"{{"type":"{kind}"}}"#)));
        std::ptr::null_mut()
    });
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::OtherMouseUp | NSEventMask::OtherMouseDown, &handler)
    };
    // Le moniteur doit vivre aussi longtemps que l'application.
    std::mem::forget(monitor);
    std::mem::forget(handler);
}
