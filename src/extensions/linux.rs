//! Linux : WebKitGTK ne sait pas faire tourner d'extensions ; rien à faire.

use tao::event_loop::EventLoopProxy;
use wry::{WebView, WebViewBuilder};

use super::Extension;
use crate::UserEvent;

pub const PLATFORM: &str = "linux";

pub fn support() -> Result<(), &'static str> {
    Err("WebKitGTK, le moteur de Feather sous Linux, ne sait pas faire tourner d'extensions.")
}

pub fn prepare_builder<'a>(builder: WebViewBuilder<'a>, _attach: bool, _url: &str) -> WebViewBuilder<'a> {
    builder
}

pub fn start(_chrome: &WebView, _list: &[Extension], _proxy: &EventLoopProxy<UserEvent>) {}


pub fn add(_ext: &Extension, _proxy: &EventLoopProxy<UserEvent>) {}

pub fn set_enabled(_ext: &Extension, _on: bool) {}

pub fn remove(_ext: &Extension) {}

pub fn replace(_old: &Extension, _new: &Extension, _proxy: &EventLoopProxy<UserEvent>) {}

pub fn errors() -> Vec<(String, Vec<String>)> {
    Vec::new()
}

pub fn answer(_number: u32, _allow: bool) {}

pub fn page_url(_ext: &Extension, _page: &str) -> Option<String> {
    None
}

pub fn options_url(_ext: &Extension) -> Option<String> {
    None
}

pub fn open_popup(
    _ext: &Extension,
    _button: (f64, f64, f64, f64),
    _frame: (f64, f64, f64, f64),
    _bar: &str,
    _chrome: &WebView,
) -> Result<Option<String>, String> {
    Err(support().unwrap_err().into())
}

pub fn close_popup() {}

pub fn popup_shown() -> bool {
    false
}
