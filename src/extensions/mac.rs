//! macOS 15.4+ : `WKWebExtensionController`. WebKit ne connaît ni les onglets
//! ni la fenêtre de Feather : on les lui décrit (`ExtTab`, `ExtWindow`), et le
//! délégué (`ExtDelegate`) répond à ses demandes (fenêtre de l'extension,
//! nouvel onglet, autorisations). Sans cela, les scripts des extensions ne
//! savaient pas dans quel onglet ils tournaient (Proton Pass ne remplissait
//! rien) et leur fenêtre s'affichait vide, avant d'être chargée.
//!
//! Règle : jamais d'appel à WebKit pendant un emprunt de `STATE`, car WebKit
//! rappelle aussitôt nos objets, qui le relisent.
use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::{self, NonNull};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSPopover, NSPopoverBehavior, NSView};
use objc2_foundation::{
    NSArray, NSDate, NSError, NSNotification, NSNotificationCenter, NSPoint, NSRect, NSRectEdge, NSSet, NSSize, NSString,
    NSUInteger, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKWebExtension, WKWebExtensionAction, WKWebExtensionContext, WKWebExtensionContextErrorsDidUpdateNotification,
    WKWebExtensionContextPermissionStatus, WKWebExtensionController, WKWebExtensionControllerDelegate,
    WKWebExtensionMatchPattern, WKWebExtensionTab, WKWebExtensionTabChangedProperties, WKWebExtensionTabConfiguration,
    WKWebExtensionWindow,
    WKUserContentController, WKWebExtensionWindowState, WKWebExtensionWindowType, WKWebView, WKWebViewConfiguration,
    WKWebsiteDataStore,
};
use tao::event_loop::EventLoopProxy;
use wry::{WebView, WebViewBuilder, WebViewBuilderExtMacos, WebViewExtMacOS};

use super::{Extension, TabView};
use crate::UserEvent;

pub const PLATFORM: &str = "macos";

type NewTabDone = RcBlock<dyn Fn(*mut ProtocolObject<dyn WKWebExtensionTab>, *mut NSError)>;
type Reply<T> = RcBlock<dyn Fn(NonNull<NSSet<T>>, *mut NSDate)>;

/// Demande d'autorisation d'une extension : ce qu'elle demande, et la
/// réponse que WebKit attend (voir `ask` et `answer`).
enum Asked {
    Permissions(Retained<NSSet<NSString>>, Reply<NSString>),
    Urls(Retained<NSSet<NSURL>>, Reply<NSURL>),
    Patterns(Retained<NSSet<WKWebExtensionMatchPattern>>, Reply<WKWebExtensionMatchPattern>),
}

/// Un onglet de Feather tel que les extensions le voient (recopié par `sync_tabs`).
struct Mirror {
    id: u32,
    tab: Retained<ExtTab>,
    title: String,
    url: String,
    loading: bool,
    pinned: bool,
    audible: bool,
    muted: bool,
    webview: Option<Retained<WKWebView>>,
}

#[derive(Default)]
struct State {
    /// Une extension au moins est installée : les onglets sont décrits à WebKit.
    wanted: bool,
    controller: Option<Retained<WKWebExtensionController>>,
    delegate: Option<Retained<ExtDelegate>>,
    contexts: HashMap<String, Retained<WKWebExtensionContext>>,
    popover: Option<Retained<NSPopover>>,
    proxy: Option<EventLoopProxy<UserEvent>>,
    /// La vue de l'interface, contre laquelle la fenêtre de l'extension s'ouvre.
    chrome: Option<Retained<NSView>>,
    /// Le bouton cliqué (x, y, largeur, hauteur) et la place de la barre.
    anchor: Option<((f64, f64, f64, f64), String)>,
    window: Option<Retained<ExtWindow>>,
    tabs: Vec<Mirror>,
    active: Option<u32>,
    /// Onglets demandés par une extension, en attente de leur création.
    new_tabs: Vec<NewTabDone>,
    /// Abonnement aux erreurs notées par WebKit (voir `errors`), et une
    /// lecture déjà demandée à la boucle d'évènements.
    errors_observer: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    errors_due: bool,
    /// Demandes d'autorisation en attente de la réponse de l'utilisateur.
    asks: Vec<(u32, Asked)>,
    next_ask: u32,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn send(event: UserEvent) {
    if let Some(proxy) = STATE.with_borrow(|s| s.proxy.clone()) {
        let _ = proxy.send_event(event);
    }
}

fn tab_object(id: u32) -> Option<Retained<ProtocolObject<dyn WKWebExtensionTab>>> {
    STATE.with_borrow(|s| s.tabs.iter().find(|m| m.id == id).map(|m| ProtocolObject::from_retained(m.tab.clone())))
}

fn active_tab() -> Option<Retained<ProtocolObject<dyn WKWebExtensionTab>>> {
    tab_object(STATE.with_borrow(|s| s.active)?)
}

fn window_object() -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
    STATE.with_borrow(|s| s.window.clone()).map(ProtocolObject::from_retained)
}

fn with_tab<R>(id: u32, f: impl FnOnce(&Mirror) -> R) -> Option<R> {
    STATE.with_borrow(|s| s.tabs.iter().find(|m| m.id == id).map(f))
}

struct TabIvars {
    id: u32,
}

define_class!(
    // SAFETY : NSObject n'impose rien à ses sous-classes ; pas de `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FeatherExtensionTab"]
    #[ivars = TabIvars]
    struct ExtTab;

    unsafe impl NSObjectProtocol for ExtTab {}

    unsafe impl WKWebExtensionTab for ExtTab {
        #[unsafe(method_id(windowForWebExtensionContext:))]
        fn window_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
            window_object()
        }

        #[unsafe(method(indexInWindowForWebExtensionContext:))]
        fn index_for(&self, _context: &WKWebExtensionContext) -> NSUInteger {
            let id = self.ivars().id;
            STATE.with_borrow(|s| s.tabs.iter().position(|m| m.id == id)).unwrap_or(NSUInteger::MAX)
        }

        #[unsafe(method_id(webViewForWebExtensionContext:))]
        fn webview_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<WKWebView>> {
            with_tab(self.ivars().id, |m| m.webview.clone()).flatten()
        }

        #[unsafe(method_id(titleForWebExtensionContext:))]
        fn title_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<NSString>> {
            with_tab(self.ivars().id, |m| NSString::from_str(&m.title))
        }

        #[unsafe(method_id(urlForWebExtensionContext:))]
        fn url_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<NSURL>> {
            with_tab(self.ivars().id, |m| NSURL::URLWithString(&NSString::from_str(&m.url))).flatten()
        }

        #[unsafe(method(isLoadingCompleteForWebExtensionContext:))]
        fn loaded_for(&self, _context: &WKWebExtensionContext) -> bool {
            with_tab(self.ivars().id, |m| !m.loading).unwrap_or(true)
        }

        #[unsafe(method(isPinnedForWebExtensionContext:))]
        fn pinned_for(&self, _context: &WKWebExtensionContext) -> bool {
            with_tab(self.ivars().id, |m| m.pinned).unwrap_or(false)
        }

        #[unsafe(method(isPlayingAudioForWebExtensionContext:))]
        fn audible_for(&self, _context: &WKWebExtensionContext) -> bool {
            with_tab(self.ivars().id, |m| m.audible).unwrap_or(false)
        }

        #[unsafe(method(isMutedForWebExtensionContext:))]
        fn muted_for(&self, _context: &WKWebExtensionContext) -> bool {
            with_tab(self.ivars().id, |m| m.muted).unwrap_or(false)
        }

        #[unsafe(method(isSelectedForWebExtensionContext:))]
        fn selected_for(&self, _context: &WKWebExtensionContext) -> bool {
            STATE.with_borrow(|s| s.active) == Some(self.ivars().id)
        }

        // Clic sur le bouton d'une extension : elle a accès à l'onglet affiché
        // (autorisation « activeTab »), comme dans Chrome.
        #[unsafe(method(shouldGrantPermissionsOnUserGestureForWebExtensionContext:))]
        fn grant_on_gesture(&self, _context: &WKWebExtensionContext) -> bool {
            true
        }

        #[unsafe(method(loadURL:forWebExtensionContext:completionHandler:))]
        fn load_url(&self, url: &NSURL, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
            if let Some(webview) = with_tab(self.ivars().id, |m| m.webview.clone()).flatten() {
                // SAFETY : fil principal, vue et requête valides.
                unsafe { webview.loadRequest(&NSURLRequest::requestWithURL(url)) };
            }
            done.call((ptr::null_mut(),));
        }

        #[unsafe(method(reloadFromOrigin:forWebExtensionContext:completionHandler:))]
        fn reload(&self, _origin: bool, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
            if let Some(webview) = with_tab(self.ivars().id, |m| m.webview.clone()).flatten() {
                // SAFETY : fil principal, vue valide.
                unsafe { webview.reload() };
            }
            done.call((ptr::null_mut(),));
        }

        #[unsafe(method(activateForWebExtensionContext:completionHandler:))]
        fn activate(&self, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
            send(UserEvent::Chrome(format!(r#"{{"type":"activate_tab","id":{}}}"#, self.ivars().id)));
            done.call((ptr::null_mut(),));
        }

        #[unsafe(method(closeForWebExtensionContext:completionHandler:))]
        fn close(&self, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
            send(UserEvent::Chrome(format!(r#"{{"type":"close_tab","id":{}}}"#, self.ivars().id)));
            done.call((ptr::null_mut(),));
        }
    }
);

impl ExtTab {
    fn new(mtm: MainThreadMarker, id: u32) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TabIvars { id });
        // SAFETY : `init` de NSObject.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY : NSObject n'impose rien à ses sous-classes ; pas de `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FeatherExtensionWindow"]
    struct ExtWindow;

    unsafe impl NSObjectProtocol for ExtWindow {}

    unsafe impl WKWebExtensionWindow for ExtWindow {
        #[unsafe(method_id(tabsForWebExtensionContext:))]
        fn tabs_for(&self, _context: &WKWebExtensionContext) -> Retained<NSArray<ProtocolObject<dyn WKWebExtensionTab>>> {
            let tabs: Vec<_> = STATE.with_borrow(|s| s.tabs.iter().map(|m| ProtocolObject::from_retained(m.tab.clone())).collect());
            NSArray::from_retained_slice(&tabs)
        }

        #[unsafe(method_id(activeTabForWebExtensionContext:))]
        fn active_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<ProtocolObject<dyn WKWebExtensionTab>>> {
            active_tab()
        }

        #[unsafe(method(windowTypeForWebExtensionContext:))]
        fn type_for(&self, _context: &WKWebExtensionContext) -> WKWebExtensionWindowType {
            WKWebExtensionWindowType::Normal
        }

        #[unsafe(method(windowStateForWebExtensionContext:))]
        fn state_for(&self, _context: &WKWebExtensionContext) -> WKWebExtensionWindowState {
            WKWebExtensionWindowState::Normal
        }

        #[unsafe(method(isPrivateForWebExtensionContext:))]
        fn private_for(&self, _context: &WKWebExtensionContext) -> bool {
            false
        }

        #[unsafe(method(focusForWebExtensionContext:completionHandler:))]
        fn focus(&self, _context: &WKWebExtensionContext, done: &DynBlock<dyn Fn(*mut NSError)>) {
            done.call((ptr::null_mut(),));
        }
    }
);

impl ExtWindow {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY : `init` de NSObject.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY : NSObject n'impose rien à ses sous-classes ; pas de `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FeatherExtensionDelegate"]
    struct ExtDelegate;

    unsafe impl NSObjectProtocol for ExtDelegate {}

    unsafe impl WKWebExtensionControllerDelegate for ExtDelegate {
        #[unsafe(method_id(webExtensionController:openWindowsForExtensionContext:))]
        fn open_windows(
            &self,
            _controller: &WKWebExtensionController,
            _context: &WKWebExtensionContext,
        ) -> Retained<NSArray<ProtocolObject<dyn WKWebExtensionWindow>>> {
            let windows: Vec<_> = window_object().into_iter().collect();
            NSArray::from_retained_slice(&windows)
        }

        #[unsafe(method_id(webExtensionController:focusedWindowForExtensionContext:))]
        fn focused_window(
            &self,
            _controller: &WKWebExtensionController,
            _context: &WKWebExtensionContext,
        ) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
            window_object()
        }

        // `tabs.create` : un onglet de Feather, rendu à l'extension dès qu'il existe.
        #[unsafe(method(webExtensionController:openNewTabUsingConfiguration:forExtensionContext:completionHandler:))]
        fn open_new_tab(
            &self,
            _controller: &WKWebExtensionController,
            configuration: &WKWebExtensionTabConfiguration,
            _context: &WKWebExtensionContext,
            done: &DynBlock<dyn Fn(*mut ProtocolObject<dyn WKWebExtensionTab>, *mut NSError)>,
        ) {
            // SAFETY : fil principal, configuration valide.
            let url = unsafe { configuration.url() }.and_then(|u| u.absoluteString()).map(|s| s.to_string());
            match url.filter(|u| super::opens_in_tab(u)) {
                Some(url) => {
                    STATE.with_borrow_mut(|s| s.new_tabs.push(done.copy()));
                    send(UserEvent::ExtensionOpen(url));
                }
                None => done.call((ptr::null_mut(), ptr::null_mut())),
            }
        }

        #[unsafe(method(webExtensionController:openOptionsPageForExtensionContext:completionHandler:))]
        fn open_options(
            &self,
            _controller: &WKWebExtensionController,
            context: &WKWebExtensionContext,
            done: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            // SAFETY : fil principal, contexte valide.
            if let Some(url) = unsafe { context.optionsPageURL() }.and_then(|u| u.absoluteString()) {
                send(UserEvent::ExtensionOpen(url.to_string()));
            }
            done.call((ptr::null_mut(),));
        }

        // Autorisations demandées plus tard par l'extension (WebKit n'accepte
        // que celles que son manifeste annonce comme facultatives) : la
        // réponse attend l'utilisateur (voir `ask`). Avant, c'était toujours oui.
        #[unsafe(method(webExtensionController:promptForPermissions:inTab:forExtensionContext:completionHandler:))]
        fn prompt_permissions(
            &self,
            _controller: &WKWebExtensionController,
            permissions: &NSSet<NSString>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            context: &WKWebExtensionContext,
            done: &DynBlock<dyn Fn(NonNull<NSSet<NSString>>, *mut NSDate)>,
        ) {
            let items = permissions.iter().map(|p| p.to_string()).collect();
            ask(context, items, Asked::Permissions(permissions.retain(), done.copy()));
        }

        #[unsafe(method(webExtensionController:promptForPermissionToAccessURLs:inTab:forExtensionContext:completionHandler:))]
        fn prompt_urls(
            &self,
            _controller: &WKWebExtensionController,
            urls: &NSSet<NSURL>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            context: &WKWebExtensionContext,
            done: &DynBlock<dyn Fn(NonNull<NSSet<NSURL>>, *mut NSDate)>,
        ) {
            let items = urls.iter().map(|url| site_of(&url)).collect();
            ask(context, items, Asked::Urls(urls.retain(), done.copy()));
        }

        #[unsafe(method(webExtensionController:promptForPermissionMatchPatterns:inTab:forExtensionContext:completionHandler:))]
        fn prompt_patterns(
            &self,
            _controller: &WKWebExtensionController,
            patterns: &NSSet<WKWebExtensionMatchPattern>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            context: &WKWebExtensionContext,
            done: &DynBlock<dyn Fn(NonNull<NSSet<WKWebExtensionMatchPattern>>, *mut NSDate)>,
        ) {
            // SAFETY : motifs valides donnés par WebKit.
            let items = patterns.iter().map(|p| unsafe { p.string() }.to_string()).collect();
            ask(context, items, Asked::Patterns(patterns.retain(), done.copy()));
        }

        // WebKit appelle ceci une fois la page de la fenêtre chargée : avant,
        // la bulle était montrée tout de suite, vide.
        #[unsafe(method(webExtensionController:presentPopupForAction:forExtensionContext:completionHandler:))]
        fn present_popup(
            &self,
            _controller: &WKWebExtensionController,
            action: &WKWebExtensionAction,
            _context: &WKWebExtensionContext,
            done: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            show_popover(action);
            done.call((ptr::null_mut(),));
        }
    }
);

impl ExtDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY : `init` de NSObject.
        unsafe { msg_send![super(this), init] }
    }
}

/// Le site d'une adresse, écrit comme un motif (« https://exemple.fr/* »).
fn site_of(url: &NSURL) -> String {
    let text = url.absoluteString().map(|s| s.to_string()).unwrap_or_default();
    match url::Url::parse(&text) {
        Ok(parsed) if parsed.host_str().is_some() => {
            format!("{}://{}/*", parsed.scheme(), parsed.host_str().unwrap_or_default())
        }
        _ => text,
    }
}

/// Demande d'une extension : gardée avec la réponse que WebKit attend, puis
/// confiée à Feather (`App::extension_asks`), qui répond par `answer`.
fn ask(context: &WKWebExtensionContext, items: Vec<String>, asked: Asked) {
    // SAFETY : fil principal, contexte valide.
    let id = unsafe { context.uniqueIdentifier() }.to_string();
    let number = STATE.with_borrow_mut(|s| {
        s.next_ask += 1;
        s.asks.push((s.next_ask, asked));
        s.next_ask
    });
    send(UserEvent::ExtensionAsks { number, id, items });
}

/// Réponse à une demande (voir `ask`) : tout ce qui était demandé, ou rien.
pub fn answer(number: u32, allow: bool) {
    let asked = STATE.with_borrow_mut(|s| {
        let at = s.asks.iter().position(|(n, _)| *n == number)?;
        Some(s.asks.remove(at).1)
    });
    // Hors de `STATE` : WebKit peut rappeler Feather pendant la réponse.
    match asked {
        Some(Asked::Permissions(set, done)) => reply(set, &done, allow),
        Some(Asked::Urls(set, done)) => reply(set, &done, allow),
        Some(Asked::Patterns(set, done)) => reply(set, &done, allow),
        None => {}
    }
}

fn reply<T: Message>(set: Retained<NSSet<T>>, done: &Reply<T>, allow: bool) {
    let set = if allow { set } else { NSSet::new() };
    done.call((NonNull::from(&*set), ptr::null_mut()));
}

pub fn support() -> Result<(), &'static str> {
    if objc2::available!(macos = 15.4) {
        Ok(())
    } else {
        Err("Les extensions demandent macOS 15.4 ou plus récent.")
    }
}

fn controller(mtm: MainThreadMarker) -> Option<Retained<WKWebExtensionController>> {
    support().ok()?;
    if let Some(controller) = STATE.with_borrow(|s| s.controller.clone()) {
        return Some(controller);
    }
    // SAFETY : fil principal, classe présente depuis macOS 15.4 (vérifié ci-dessus).
    let controller = unsafe { WKWebExtensionController::new(mtm) };
    let delegate = ExtDelegate::new(mtm);
    // SAFETY : fil principal ; le contrôleur ne garde qu'une référence faible
    // au délégué, gardé dans `STATE`.
    unsafe { controller.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
    // Erreurs notées par WebKit pendant que les extensions tournent : une
    // seule lecture demandée pour une rafale de notifications.
    let block = RcBlock::new(|_: NonNull<NSNotification>| {
        let proxy = STATE.with_borrow_mut(|s| {
            let proxy = if s.errors_due { None } else { s.proxy.clone() };
            s.errors_due |= proxy.is_some();
            proxy
        });
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(UserEvent::ExtensionErrors);
        }
    });
    // SAFETY : nom publié par WebKit depuis macOS 15.4 (vérifié par `support`) ;
    // sans file d'attente, le bloc est appelé sur le fil qui publie, le
    // principal pour WebKit. L'abonnement est gardé dans `STATE`.
    let observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(WKWebExtensionContextErrorsDidUpdateNotification),
            None,
            None,
            &block,
        )
    };
    STATE.with_borrow_mut(|s| {
        s.controller = Some(controller.clone());
        s.delegate = Some(delegate);
        s.errors_observer = Some(observer);
    });
    Some(controller)
}

/// Les erreurs notées par WebKit, par extension : manifeste, arrière-plan
/// refusé, erreur de script (selon la version de macOS). Toutes à chaque
/// fois ; WebKit ne garde qu'un exemplaire de chacune.
pub fn errors() -> Vec<(String, Vec<String>)> {
    let contexts: Vec<_> = STATE.with_borrow_mut(|s| {
        s.errors_due = false;
        s.contexts.iter().map(|(id, c)| (id.clone(), c.clone())).collect()
    });
    contexts
        .into_iter()
        .map(|(id, context)| {
            // SAFETY : fil principal, contexte valide.
            let errors = unsafe { context.errors() };
            (id, errors.iter().map(|e| e.localizedDescription().to_string()).collect())
        })
        .collect()
}

/// Toute page non privée reçoit le contrôleur des extensions. Avant, seulement
/// quand une extension était déjà installée : les onglets ouverts avant
/// l'installation n'avaient jamais les extensions (WebKit ne permet pas de
/// l'ajouter à une page déjà créée).
pub fn prepare_builder<'a>(builder: WebViewBuilder<'a>, attach: bool, url: &str) -> WebViewBuilder<'a> {
    let Some(mtm) = MainThreadMarker::new() else { return builder };
    if !attach {
        return builder;
    }
    // Page d'une extension (options, page ouverte par l'extension) : WebKit ne
    // la charge que dans la configuration de son contexte. Avec celle des
    // onglets, la page restait blanche (vérifié par outils/sonde-extensions.m).
    if let Some(config) = page_configuration(url) {
        return builder.with_webview_configuration(config);
    }
    let Some(controller) = controller(mtm) else { return builder };
    // SAFETY : fil principal ; configuration neuve, magasin par défaut
    // (celui que wry aurait choisi pour une page non privée).
    let config = unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        config.setWebsiteDataStore(&WKWebsiteDataStore::defaultDataStore(mtm));
        config.setWebExtensionController(Some(&controller));
        config
    };
    builder.with_webview_configuration(config)
}

pub fn start(chrome: &WebView, list: &[Extension], proxy: &EventLoopProxy<UserEvent>) {
    if support().is_err() {
        return;
    }
    let view: Retained<NSView> = Retained::into_super(Retained::into_super(chrome.webview()));
    STATE.with_borrow_mut(|s| {
        s.proxy = Some(proxy.clone());
        s.chrome = Some(view);
    });
    for ext in list {
        add(ext, proxy);
    }
}

/// Décrit à WebKit les onglets de Feather (ouverts, fermés, déplacés, affiché,
/// titre, adresse…), après chaque changement d'état.
pub fn sync_tabs<'a>(tabs: impl Iterator<Item = TabView<'a>>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if !STATE.with_borrow(|s| s.wanted) {
        return;
    }
    let Some(controller) = controller(mtm) else { return };
    // La fenêtre, une fois pour toutes.
    let new_window = STATE.with_borrow(|s| s.window.is_none());
    if new_window {
        let window = ExtWindow::new(mtm);
        STATE.with_borrow_mut(|s| s.window = Some(window.clone()));
        let window = ProtocolObject::from_retained(window);
        // SAFETY : fil principal, objets valides.
        unsafe {
            controller.didOpenWindow(&window);
            controller.didFocusWindow(Some(&window));
        }
    }
    let views: Vec<TabView> = tabs.collect();
    let active = views.iter().find(|v| v.active).map(|v| v.id);
    let mut opened = Vec::new();
    let mut moved = Vec::new();
    let mut changed = Vec::new();
    // Les différences sont calculées d'abord, WebKit prévenu ensuite (voir la règle en tête).
    let (closed, previous, done) = STATE.with_borrow_mut(|s| {
        let mut old = std::mem::take(&mut s.tabs);
        let previous = s.active.and_then(|id| old.iter().find(|m| m.id == id)).map(|m| m.tab.clone());
        let old_index: HashMap<u32, usize> = old.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
        for (index, view) in views.iter().enumerate() {
            let webview: Option<Retained<WKWebView>> = view.webview.map(|w| Retained::into_super(w.webview()));
            let mirror = match old.iter().position(|m| m.id == view.id) {
                Some(i) => {
                    let m = old.swap_remove(i);
                    let mut props = WKWebExtensionTabChangedProperties::empty();
                    if m.title != view.title {
                        props |= WKWebExtensionTabChangedProperties::Title;
                    }
                    let same_view = match (&m.webview, &webview) {
                        (Some(a), Some(b)) => ptr::eq(&**a, &**b),
                        (None, None) => true,
                        _ => false,
                    };
                    if m.url != view.url || !same_view {
                        props |= WKWebExtensionTabChangedProperties::URL;
                    }
                    if m.loading != view.loading || !same_view {
                        props |= WKWebExtensionTabChangedProperties::Loading;
                    }
                    if m.pinned != view.pinned {
                        props |= WKWebExtensionTabChangedProperties::Pinned;
                    }
                    if m.audible != view.audible {
                        props |= WKWebExtensionTabChangedProperties::PlayingAudio;
                    }
                    if m.muted != view.muted {
                        props |= WKWebExtensionTabChangedProperties::Muted;
                    }
                    if !props.is_empty() {
                        changed.push((m.tab.clone(), props));
                    }
                    if old_index.get(&view.id).is_some_and(|&i| i != index) {
                        moved.push((m.tab.clone(), old_index[&view.id]));
                    }
                    m.tab
                }
                None => {
                    let tab = ExtTab::new(mtm, view.id);
                    opened.push(tab.clone());
                    tab
                }
            };
            s.tabs.push(Mirror {
                id: view.id,
                tab: mirror,
                title: view.title.to_string(),
                url: view.url.to_string(),
                loading: view.loading,
                pinned: view.pinned,
                audible: view.audible,
                muted: view.muted,
                webview,
            });
        }
        // L'onglet affiché avant, sauf s'il vient d'être fermé.
        let previous = previous.filter(|p| !old.iter().any(|m| ptr::eq(&*m.tab, &**p)));
        let previous = if s.active == active { None } else { Some(previous) };
        s.active = active;
        // Onglets demandés par une extension : le dernier ouvert leur est rendu.
        let done = if opened.is_empty() { Vec::new() } else { std::mem::take(&mut s.new_tabs) };
        (old, previous, done)
    });
    let active_tab = active.and_then(tab_object);
    // SAFETY : fil principal, objets valides ; aucun emprunt de `STATE` en cours.
    unsafe {
        for m in closed {
            controller.didCloseTab_windowIsClosing(&ProtocolObject::from_retained(m.tab), false);
        }
        for tab in &opened {
            controller.didOpenTab(ProtocolObject::from_ref(&**tab));
        }
        for (tab, from) in moved {
            controller.didMoveTab_fromIndex_inWindow(ProtocolObject::from_ref(&*tab), from, window_object().as_deref());
        }
        for (tab, props) in changed {
            controller.didChangeTabProperties_forTab(props, ProtocolObject::from_ref(&*tab));
        }
        if let (Some(previous), Some(tab)) = (previous, &active_tab) {
            let previous = previous.map(ProtocolObject::from_retained);
            controller.didActivateTab_previousActiveTab(tab, previous.as_deref());
        }
    }
    if let Some(tab) = opened.last() {
        let tab: &ProtocolObject<dyn WKWebExtensionTab> = ProtocolObject::from_ref(&**tab);
        let tab = tab as *const ProtocolObject<dyn WKWebExtensionTab> as *mut _;
        for block in done {
            block.call((tab, ptr::null_mut()));
        }
    }
}

pub fn add(ext: &Extension, proxy: &EventLoopProxy<UserEvent>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let (Some(controller), Some(path)) = (controller(mtm), ext.path()) else { return };
    STATE.with_borrow_mut(|s| s.wanted = true);
    let id = ext.id.clone();
    let enabled = ext.enabled;
    let granted = ext.granted.clone();
    let proxy = proxy.clone();
    let block = RcBlock::new(move |extension: *mut WKWebExtension, error: *mut NSError| {
        // SAFETY : WebKit passe une extension valide, ou nulle avec une erreur.
        let Some(extension) = (unsafe { Retained::retain(extension) }) else {
            let error = unsafe { Retained::retain(error) };
            let message = error.map(|e| e.localizedDescription().to_string()).unwrap_or_else(|| "extension refusée par WebKit".into());
            let _ = proxy.send_event(UserEvent::ExtensionLoaded { id: id.clone(), result: Err(message) });
            return;
        };
        // SAFETY : fil principal (WebKit rappelle sur la file principale), objets valides.
        let result = unsafe {
            let context = WKWebExtensionContext::contextForExtension(&extension);
            context.setUniqueIdentifier(&NSString::from_str(&id));
            context.setInspectable(cfg!(debug_assertions));
            // Ce que l'extension demande lui est accordé à l'installation,
            // comme Chrome (la liste des sites est affichée dans le panneau).
            for permission in extension.requestedPermissions().iter() {
                context.setPermissionStatus_forPermission(WKWebExtensionContextPermissionStatus::GrantedExplicitly, &permission);
            }
            for pattern in extension.allRequestedMatchPatterns().iter() {
                context.setPermissionStatus_forMatchPattern(WKWebExtensionContextPermissionStatus::GrantedExplicitly, &pattern);
            }
            // Et ce que l'utilisateur lui a accordé ensuite (voir `answer`).
            for item in &granted {
                let text = NSString::from_str(item);
                if super::is_site_pattern(item) {
                    if let Some(pattern) = WKWebExtensionMatchPattern::matchPatternWithString(&text, mtm) {
                        context.setPermissionStatus_forMatchPattern(WKWebExtensionContextPermissionStatus::GrantedExplicitly, &pattern);
                    }
                } else {
                    context.setPermissionStatus_forPermission(WKWebExtensionContextPermissionStatus::GrantedExplicitly, &text);
                }
            }
            let loaded = if enabled { controller.loadExtensionContext_error(&context) } else { Ok(()) };
            STATE.with_borrow_mut(|s| s.contexts.insert(id.clone(), context));
            loaded.map(|()| None).map_err(|e| e.localizedDescription().to_string())
        };
        let _ = proxy.send_event(UserEvent::ExtensionLoaded { id: id.clone(), result });
    });
    // Les évènements de Chrome absents de WebKit, avant l'arrière-plan.
    if let Err(err) = super::compat::add_compat(&path) {
        eprintln!("compléments de l'extension non ajoutés : {err}");
    }
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&path.to_string_lossy()), true);
    // SAFETY : fil principal, adresse de fichier valide, rappel conservé par WebKit.
    unsafe { WKWebExtension::extensionWithResourceBaseURL_completionHandler(&url, &block, mtm) };
}

fn context(id: &str) -> Option<Retained<WKWebExtensionContext>> {
    STATE.with_borrow(|s| s.contexts.get(id).cloned())
}

/// Configuration du contexte dont l'adresse de base commence `url`
/// (webkit-extension://<identifiant>/…).
///
/// WebKit rend une copie de la configuration, mais qui garde le même
/// `WKUserContentController` pour toutes les pages de l'extension : wry y
/// ajoutait à chaque page son gestionnaire « ipc » (WebKit refuse un nom déjà
/// pris) et ses scripts, jamais retirés. Chaque page reçoit donc le sien.
fn page_configuration(url: &str) -> Option<Retained<WKWebViewConfiguration>> {
    if !url.starts_with("webkit-extension://") {
        return None;
    }
    let mtm = MainThreadMarker::new()?;
    let contexts: Vec<_> = STATE.with_borrow(|s| s.contexts.values().cloned().collect());
    contexts.into_iter().find_map(|context| {
        // SAFETY : fil principal, contexte valide ; contrôleur neuf.
        unsafe {
            let base = context.baseURL().absoluteString()?.to_string();
            if !url.starts_with(&base) {
                return None;
            }
            let config = context.webViewConfiguration()?;
            config.setUserContentController(&WKUserContentController::new(mtm));
            Some(config)
        }
    })
}

pub fn set_enabled(ext: &Extension, on: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let (Some(controller), Some(context)) = (controller(mtm), context(&ext.id)) else { return };
    // SAFETY : fil principal, objets valides.
    unsafe {
        if on && !context.isLoaded() {
            let _ = controller.loadExtensionContext_error(&context);
        } else if !on && context.isLoaded() {
            let _ = controller.unloadExtensionContext_error(&context);
        }
    }
}

pub fn remove(ext: &Extension) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(context) = STATE.with_borrow_mut(|s| s.contexts.remove(&ext.id)) else { return };
    if let Some(controller) = controller(mtm) {
        // SAFETY : fil principal, objets valides.
        unsafe {
            if context.isLoaded() {
                let _ = controller.unloadExtensionContext_error(&context);
            }
        }
    }
}

pub fn replace(old: &Extension, new: &Extension, proxy: &EventLoopProxy<UserEvent>) {
    remove(old);
    add(new, proxy);
}

/// Adresse d'une page de l'extension (`popup.html`…), une fois chargée.
pub fn page_url(ext: &Extension, page: &str) -> Option<String> {
    let context = context(&ext.id)?;
    // SAFETY : fil principal, contexte valide.
    let base = unsafe { context.baseURL().absoluteString() }?.to_string();
    Some(format!("{}/{page}", base.trim_end_matches('/')))
}

pub fn options_url(ext: &Extension) -> Option<String> {
    let context = context(&ext.id)?;
    // SAFETY : fil principal, contexte valide.
    let url = unsafe { context.optionsPageURL() }?;
    url.absoluteString().map(|s| s.to_string())
}

/// Clic sur le bouton de l'extension (`button`, dans l'interface) : WebKit
/// charge sa fenêtre, puis demande de l'afficher (`present_popup`).
/// Sans fenêtre, le clic est transmis à l'extension.
pub fn open_popup(
    ext: &Extension,
    button: (f64, f64, f64, f64),
    _frame: (f64, f64, f64, f64),
    bar: &str,
    _chrome: &WebView,
) -> Result<Option<String>, String> {
    let context = context(&ext.id).ok_or("L'extension est encore en cours de chargement.")?;
    let tab = active_tab();
    // SAFETY : fil principal, objets valides ; aucun emprunt de `STATE` en cours.
    unsafe {
        let action = context.actionForTab(tab.as_deref()).ok_or("Cette extension n'a pas de bouton.")?;
        if action.presentsPopup() {
            STATE.with_borrow_mut(|s| s.anchor = Some((button, bar.to_string())));
        }
        context.performActionForTab(tab.as_deref());
    }
    Ok(None)
}

/// La bulle de la fenêtre, la pointe sur le bouton de l'extension, du côté de la page.
fn show_popover(action: &WKWebExtensionAction) {
    // SAFETY : fil principal, action valide.
    let Some(popover) = (unsafe { action.popupPopover() }) else { return };
    let (anchor, view) = STATE.with_borrow_mut(|s| (s.anchor.take(), s.chrome.clone()));
    let Some(view) = view else { return };
    let ((x, y, w, h), bar) = anchor.unwrap_or(((0.0, 0.0, 1.0, 1.0), String::new()));
    let flipped = view.isFlipped();
    let height = view.frame().size.height;
    let y = if flipped { y } else { height - y - h };
    let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(1.0), h.max(1.0)));
    let edge = match bar.as_str() {
        "right" => NSRectEdge::MinX,
        "top" if flipped => NSRectEdge::MaxY,
        "top" => NSRectEdge::MinY,
        _ => NSRectEdge::MaxX,
    };
    popover.setBehavior(NSPopoverBehavior::Transient);
    popover.showRelativeToRect_ofView_preferredEdge(rect, &view, edge);
    STATE.with_borrow_mut(|s| s.popover = Some(popover));
}

pub fn close_popup() {
    let popover = STATE.with_borrow_mut(|s| {
        s.anchor = None;
        s.popover.take()
    });
    if let Some(popover) = popover {
        // SAFETY : fil principal, bulle valide.
        unsafe { popover.performClose(None) };
    }
}

/// La bulle se ferme seule au clic ailleurs : est-elle encore affichée ?
/// (Juste après le clic, la fenêtre se charge : elle compte comme affichée.)
pub fn popup_shown() -> bool {
    STATE.with_borrow(|s| s.anchor.is_some() || s.popover.as_ref().is_some_and(|p| p.isShown()))
}
