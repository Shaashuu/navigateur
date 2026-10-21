// En mode release sur Windows, pas de console noire derrière la fenêtre.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Feather : une fenêtre tao, une webview pour l'interface, et une
//! webview par onglet. Tout tourne sur le thread principal ; les webviews
//! communiquent avec le code Rust par des messages (`UserEvent`).
//!
//! Ce fichier contient le cœur (onglets, navigation, état de l'interface) ;
//! `features.rs` ajoute la recherche dans la page, le zoom, les autorisations,
//! les commandes natives, la mise en veille et le suivi des téléchargements.

mod adblock;
mod capture;
mod commands;
mod downloads;
mod errorpage;
mod extensions;
mod features;
mod fulltext;
mod images;
mod import;
mod layers;
mod library;
#[cfg(target_os = "macos")]
mod mac_mouse;
mod network;
mod reader;
mod settings;
mod siteclear;
mod sitedata;
mod softsleep;
mod storage;
mod tab_manager;
mod trackers;
mod ui;
mod update;
mod weight;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Icon, Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

use commands::Notes;
use downloads::Download;
use features::PendingPermission;
use fulltext::FullText;
use library::{Library, Session};
use settings::{Settings, Startup};
use sitedata::SiteData;
use tab_manager::{PageContext, TabId, TabManager};
use ui::{ChromeMessage, LibraryKind, LinkHow, PageMessage, Shortcut, Surface};
use weight::Sampler;

/// Une page peut ouvrir au plus ce nombre d'onglets (clic de molette)…
const PAGE_TAB_LIMIT: usize = 10;
/// … sur cette durée : au-delà, une page malveillante pourrait en ouvrir sans fin.
const PAGE_TAB_WINDOW: Duration = Duration::from_secs(10);
/// Au plus un envoi par seconde pour des titres : certains sites font défiler
/// le leur (« (3) Nouveau message… ») plusieurs fois par seconde.
const TITLE_PAUSE: Duration = Duration::from_secs(1);
/// Lecture de la position de la souris pendant qu'un panneau est ouvert
/// (voir `App::watch_pointer`) : assez souvent pour que la barre au survol se
/// referme sans attendre, sans réveiller le programme pour rien.
const POINTER_POLL: Duration = Duration::from_millis(60);

/// Événements envoyés par les webviews à la boucle principale.
#[derive(Debug)]
pub enum UserEvent {
    /// Message JSON venant de l'interface.
    Chrome(String),
    /// Message JSON venant d'une page (raccourci clavier, barre de l'accueil).
    Page(TabId, String),
    Title(TabId, String),
    /// Des changements de titre retenus (voir `TITLE_PAUSE`) partent vers l'interface.
    TitlesDue,
    /// Début (`true`) ou fin (`false`) de chargement, avec l'adresse.
    PageLoad(TabId, bool, String),
    /// Le chargement de cette adresse a échoué : on affiche une page d'erreur Feather.
    LoadFailed { id: TabId, url: String, kind: errorpage::ErrorKind },
    /// `window.open` ; `private` : la page qui l'a demandé est privée.
    OpenInNewTab { url: String, private: bool },
    Update(UpdateEvent),
    /// Les onglets fermés peuvent être détruits.
    Reap,
    /// Un téléchargement commence, rangé à cet endroit.
    DownloadStarted { url: String, path: PathBuf, private: bool },
    DownloadDone { url: String, path: Option<PathBuf>, success: bool },
    /// Taille annoncée par le serveur pour un téléchargement.
    DownloadSize { id: u32, total: u64 },
    /// Image ajoutée à Photos (ou la raison de l'échec).
    ImageSaved(Result<(), String>),
    /// Moment d'enregistrer l'historique, les favoris et la session.
    Persist,
    /// Chaque seconde : progression des téléchargements, poids et veille des onglets.
    Tick,
    /// Un minuteur (`/minuteur`) est arrivé à son terme.
    TimerDone { seconds: u64, label: String },
    /// Fin d'une capture de page : le fichier écrit (`None` : copiée), ou l'erreur.
    Captured(Result<Option<PathBuf>, String>),
    /// Listes du bloqueur lues (ou leur état seul : téléchargement, erreur).
    AdFilters(Option<Box<adblock::Filters>>, adblock::Status),
    /// macOS : les listes compilées par WebKit sont prêtes à être posées sur les pages.
    AdRulesReady,
    /// Une extension est téléchargée et décompressée (ou la raison de l'échec).
    ExtensionPrepared(Result<Box<extensions::Extension>, String>),
    /// Le moteur a chargé une extension (avec son identifiant WebView2), ou l'a refusée.
    ExtensionLoaded { id: String, result: Result<Option<String>, String> },
    /// Message de la petite fenêtre d'une extension (Windows).
    ExtensionPopup(String),
    /// Lien ouvert depuis la fenêtre d'une extension (Windows), ou onglet
    /// demandé par une extension (macOS).
    ExtensionOpen(String),
}

/// Résultat d'un travail de mise à jour fait en arrière-plan.
#[derive(Debug)]
pub enum UpdateEvent {
    Checked { found: Result<Option<update::Available>, String>, manual: bool },
    Installed(Result<update::Restart, String>),
}

/// Où en est la mise à jour (affiché dans les paramètres).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available { info: update::Available },
    /// `received` : octets déjà téléchargés (mesurés chaque seconde), `speed` : par seconde.
    Downloading { info: update::Available, received: u64, speed: u64 },
    Ready { info: update::Available },
    Error { message: String },
}

struct App {
    window: Window,
    surface: Surface,
    chrome: WebView,
    tabs: TabManager,
    settings: Arc<RwLock<Settings>>,
    proxy: EventLoopProxy<UserEvent>,
    /// Un panneau de l'interface (commandes, paramètres) recouvre la page.
    expanded: bool,
    /// Dernière position de la souris envoyée à l'interface (en pixels CSS
    /// arrondis ; `None` : hors de la fenêtre) et prochaine lecture prévue,
    /// tant qu'un panneau est ouvert par-dessus la page ou que l'interface le
    /// demande (`pointer_wanted`, voir `watch_pointer`).
    pointer_wanted: bool,
    pointer_sent: Option<Option<(i32, i32)>>,
    pointer_due: Option<Instant>,
    /// Mode concentration (Ctrl+Maj+F) : la barre se réduit à un rail.
    focus: bool,
    /// L'interface passe au-dessus des pages quand un panneau est ouvert.
    layers: layers::Layers,
    /// Le bandeau du bas (recherche, question, minuteur) est ouvert : la page rétrécit.
    strip: bool,
    update: UpdateState,
    /// Programme à relancer quand la mise à jour est installée.
    restart: Option<update::Restart>,
    library: Arc<RwLock<Library>>,
    /// Zoom et autorisations retenus par site.
    sites: Arc<RwLock<SiteData>>,
    /// Texte des pages visitées (recherche dans l'historique).
    fulltext: FullText,
    /// Notes de la commande `/note`.
    notes: Notes,
    /// Téléchargements de cette session.
    downloads: Vec<Download>,
    /// Prochain numéro de téléchargement (jamais réutilisé, même après « Vider la liste »).
    next_download_id: u32,
    /// Un enregistrement est déjà prévu (voir `schedule_persist`).
    persist_pending: Cell<bool>,
    /// L'état a changé : il partira vers l'interface une fois les événements en
    /// attente traités (voir `push_state`).
    state_dirty: Cell<bool>,
    /// Dernier envoi dû à un titre, et envoi de titres déjà prévu (voir `TITLE_PAUSE`).
    last_title_push: Cell<Instant>,
    titles_waiting: Cell<bool>,
    /// Le dernier état envoyé, pour n'envoyer ensuite que ce qui a changé.
    sent_state: RefCell<serde_json::Map<String, serde_json::Value>>,
    /// Le prochain envoi sera complet : après un message de l'interface, qui a
    /// pu modifier sa copie de l'état en attendant la réponse (réglages…).
    full_state: Cell<bool>,
    /// Dernière session écrite sur le disque, pour ne pas la réécrire à l'identique.
    saved_session: RefCell<Option<Session>>,
    /// Heures des derniers onglets ouverts à la demande d'une page (voir `PAGE_TAB_LIMIT`).
    page_opens: VecDeque<Instant>,
    /// Demandes d'autorisation en attente d'une réponse de l'utilisateur.
    permission_requests: HashMap<u32, PendingPermission>,
    next_permission_request: u32,
    /// Mesure du processus de chaque page (macOS).
    sampler: Sampler,
    /// Le panneau « Utilisation de la mémoire » est ouvert : la mémoire des
    /// onglets est relevée plus souvent et envoyée dès qu'elle change.
    memory_watched: bool,
    /// Articles du mode lecture, un par onglet (lus par le protocole `navigateur`).
    reader: reader::Store,
    /// Secondes écoulées (pour ne mesurer le poids des onglets que toutes les 5 s).
    ticks: u64,
    /// Listes du bloqueur de publicités (affiché dans les paramètres).
    adblock: adblock::Status,
    /// Extensions installées et leur petite fenêtre ouverte.
    extensions: extensions::Extensions,
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("Feather")
        .with_window_icon(window_icon())
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(LogicalSize::new(480.0, 360.0))
        .build(&event_loop)
        .expect("impossible de créer la fenêtre");

    #[cfg(target_os = "macos")]
    mac_mouse::install(proxy.clone());

    let surface = Surface::new(&window);

    // L'interface est créée en premier pour rester sous les pages.
    let chrome_proxy = proxy.clone();
    // Sous Windows, toutes les webviews ont le même réglage des extensions (voir extensions.rs).
    let chrome = surface.attach(
        extensions::prepare_builder(WebViewBuilder::new()
            .with_bounds(ui::chrome_bounds(&window))
            .with_html(ui::CHROME_HTML)
            // Transparente là où les panneaux laissent voir la page (voir `layers.rs`).
            .with_transparent(layers::SUPPORTED)
            .with_devtools(cfg!(debug_assertions))
            .with_ipc_handler(move |request| {
                let _ = chrome_proxy.send_event(UserEvent::Chrome(request.into_body()));
            }), false),
        &window,
    )?;

    update::clean_leftovers();
    let settings = Settings::load();
    let check_update = settings.auto_update_check;
    let start_url = settings.start_url();
    let restore = settings.startup == Startup::Restore;
    let mut library = Library::load();
    library.prune(settings.history_days);
    let mut fulltext = FullText::load();
    fulltext.prune(settings.history_days);

    // Une seconde : progression des téléchargements, poids et veille des onglets.
    let tick_proxy = proxy.clone();
    std::thread::spawn(move || {
        while tick_proxy.send_event(UserEvent::Tick).is_ok() {
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    let mut app = App {
        window,
        surface,
        chrome,
        tabs: TabManager::default(),
        settings: Arc::new(RwLock::new(settings)),
        proxy,
        expanded: false,
        pointer_wanted: false,
        pointer_sent: None,
        pointer_due: None,
        focus: false,
        layers: layers::Layers::default(),
        strip: false,
        update: UpdateState::Idle,
        restart: None,
        library: Arc::new(RwLock::new(library)),
        sites: Arc::new(RwLock::new(SiteData::load())),
        fulltext,
        notes: Notes::load(),
        downloads: Vec::new(),
        next_download_id: 1,
        persist_pending: Cell::new(false),
        state_dirty: Cell::new(false),
        last_title_push: Cell::new(Instant::now().checked_sub(TITLE_PAUSE).unwrap_or_else(Instant::now)),
        titles_waiting: Cell::new(false),
        sent_state: RefCell::new(serde_json::Map::new()),
        full_state: Cell::new(false),
        saved_session: RefCell::new(None),
        page_opens: VecDeque::new(),
        permission_requests: HashMap::new(),
        next_permission_request: 1,
        sampler: Sampler::default(),
        memory_watched: false,
        reader: reader::Store::default(),
        ticks: 0,
        adblock: adblock::Status::default(),
        extensions: extensions::Extensions::load(),
    };
    extensions::start(&app.chrome, app.extensions.list(), &app.proxy);
    adblock::set_enabled(app.settings().blocks_ads());
    adblock::set_sites_off(app.sites.read().map(|s| s.ads_off()).unwrap_or_default());
    adblock::load(app.proxy.clone(), false);
    let session = library::load_session();
    // Adresses données au lancement (« Ouvrir avec Feather », banc de mesure de la
    // mémoire `outils/banc-memoire`) : une par onglet, la première affichée.
    let launch = network::launch_urls(std::env::args().skip(1));
    let full_restore = restore && !session.urls.is_empty() && launch.is_empty();
    // Les onglets épinglés reviennent à chaque démarrage (en veille, en haut de la colonne).
    if !full_restore {
        for (url, _) in session.urls.iter().zip(&session.pinned).filter(|(_, p)| **p) {
            let id = app.tabs.add_asleep(url, "", false);
            app.tabs.set_pinned(id, true);
        }
    }
    if let Some((first, others)) = launch.split_first() {
        app.open_tab(first, false);
        for url in others {
            app.open_tab_in_background(url);
        }
    } else if full_restore {
        // Les onglets restaurés restent en veille : seul celui affiché est chargé.
        for (i, url) in session.urls.iter().enumerate() {
            let id = app.tabs.add_asleep(url, "", false);
            if let Some(tab) = app.tabs.get_mut(id) {
                tab.pinned = session.pinned.get(i).copied().unwrap_or(false);
                tab.group = session.groups.get(i).copied().flatten();
            }
        }
        // L'onglet affiché est repéré avant que `restore_groups` ne range les onglets.
        let shown = app.tabs.iter().nth(session.active).map(|t| t.id);
        app.tabs.restore_groups(session.tab_groups.clone());
        match shown {
            Some(id) => app.tabs.activate(id, true),
            None => app.tabs.activate_index(0, true),
        }
        app.wake_active();
        app.push_state();
    } else {
        app.open_tab(&start_url, false);
    }
    if check_update {
        app.start_check(false);
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                app.persist();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. },
                ..
            } => app.relayout(),
            Event::UserEvent(event) => app.handle(event),
            // Tous les événements en attente sont traités : un seul envoi de l'état.
            Event::MainEventsCleared => {
                app.flush_state();
                app.watch_pointer();
            }
            _ => {}
        }
        // Panneau ouvert : la boucle se réveille pour relire la position de la souris.
        if *control_flow == ControlFlow::Wait
            && let Some(due) = app.pointer_due
        {
            *control_flow = ControlFlow::WaitUntil(due);
        }
    });
}

/// Les commandes de la barre (`/note`…), envoyées seulement avec l'état complet.
fn native_commands() -> serde_json::Value {
    // Les commandes de page (`features::page_command`) s'ajoutent à celles de `commands`.
    let mut page_commands = vec![("lecture", "Afficher l'article en mode lecture")];
    if capture::SUPPORTED {
        page_commands.push(("capture", "Capturer la page visible (Ctrl+Maj+S) · « copier » : dans le presse-papiers"));
    }
    commands::liste()
        .into_iter()
        .chain(page_commands)
        .map(|(name, desc)| json!({ "name": format!("/{name}"), "desc": desc }))
        .collect()
}

/// Icône de la fenêtre : image 64×64 en RGBA brut (voir packaging/).
fn window_icon() -> Option<Icon> {
    const RGBA: &[u8] = include_bytes!("../packaging/feather-64.rgba");
    Icon::from_rgba(RGBA.to_vec(), 64, 64).ok()
}

impl App {
    fn handle(&mut self, event: UserEvent) {
        match event {
            UserEvent::Chrome(raw) => match serde_json::from_str::<ChromeMessage>(&raw) {
                Ok(message) => self.handle_chrome(message),
                Err(err) => eprintln!("message de l'interface ignoré : {err}"),
            },
            UserEvent::Page(id, raw) => {
                if let Ok(message) = serde_json::from_str::<PageMessage>(&raw) {
                    self.handle_page(id, message);
                }
            }
            UserEvent::Title(id, title) => {
                // Une page qu'on vient d'endormir passe par about:blank (voir `stop_page`) :
                // son titre ne remplace pas celui de l'onglet.
                if let Some(tab) = self.tabs.get_mut(id).filter(|t| !t.is_asleep()) {
                    // Le titre d'une page d'erreur ne remplace pas celui de la page dans l'historique.
                    if !tab.private && !tab.on_internal_page() {
                        if let Ok(mut lib) = self.library.write() {
                            lib.set_title(tab.url(), &title);
                        }
                        self.fulltext.set_title(tab.url(), &title);
                    }
                    tab.title = title;
                }
                self.title_changed();
            }
            UserEvent::TitlesDue => {
                self.titles_waiting.set(false);
                self.last_title_push.set(Instant::now());
                self.push_state();
            }
            UserEvent::PageLoad(id, loading, url) => {
                // La page d'un onglet mis en veille charge about:blank avant d'être
                // détruite : sans ce filtre, l'onglet retenait cette adresse et se
                // réveillait sur une page blanche.
                let tab = self.tabs.get_mut(id).filter(|t| !t.is_asleep() && url != "about:blank");
                if let Some(tab) = tab {
                    // macOS : identité Safari sur Google seulement (la page est
                    // rechargée si l'onglet entre sur Google ou en sort). À la fin du
                    // chargement, pour les redirections ; un seul rechargement de suite,
                    // au cas où deux adresses se renverraient l'une à l'autre.
                    if let Some(webview) = tab.webview() {
                        if loading {
                            tab_manager::match_identity(webview, &url);
                        } else if !tab.identity_retry {
                            tab.identity_retry = tab_manager::match_identity(webview, &url);
                        } else {
                            tab.identity_retry = false;
                        }
                    }
                    // Page d'erreur demandée par Feather : l'onglet garde l'adresse en
                    // échec (barre, « Réessayer », session). Une page d'erreur ouverte
                    // autrement (lien d'un site) garde la sienne : rien à usurper.
                    let url = match errorpage::parse(&url) {
                        Some(failed) if tab.failed.as_ref() == Some(&failed) => failed.1,
                        _ => {
                            tab.failed = None;
                            url
                        }
                    };
                    // Page de lecture de cet onglet, pour l'article gardé : même principe.
                    let kept = |page: &(TabId, String)| {
                        self.reader.read().is_ok_and(|s| s.get(&id).is_some_and(|(u, _)| page.0 == id && *u == page.1))
                    };
                    let url = match reader::parse(&url) {
                        Some(page) if kept(&page) => {
                            tab.reading = Some(page.1.clone());
                            page.1
                        }
                        _ => {
                            tab.reading = None;
                            url
                        }
                    };
                    if loading {
                        tab.readable = false;
                    }
                    // Autre site : on efface l'icône de l'ancien en attendant celle du nouveau.
                    if loading && network::host_of(tab.url()) != network::host_of(&url) {
                        tab.icon = None;
                        tab.site_name = None;
                    }
                    tab.loading = loading;
                    tab.set_url(url);
                    // Bloqueur posé ou retiré selon le site (macOS ; voir `adblock::apply`).
                    let host = tab.host();
                    let ads = adblock::active_on(&host);
                    if tab.ads != Some(ads)
                        && let Some(w) = tab.webview()
                    {
                        adblock::apply(w, &host);
                        tab.ads = Some(ads);
                    }
                    // Un onglet privé ne laisse aucune trace dans l'historique.
                    if !loading
                        && !tab.private
                        && !tab.on_internal_page()
                        && let Ok(mut lib) = self.library.write()
                    {
                        lib.record_visit(tab.url(), &tab.title);
                    }
                    if loading {
                        tab.audible = false;
                        // La page qui posait une question s'en va : la question aussi.
                        self.permission_requests.retain(|_, p| p.tab != id);
                    } else if tab.muted {
                        // Nouvelle page : on garde l'onglet en sourdine.
                        tab.apply_mute();
                    }
                }
                self.apply_zoom(id);
                self.apply_economy(id);
                self.push_state();
            }
            UserEvent::LoadFailed { id, url, kind } => self.load_failed(id, url, kind),
            // window.open : seules les pages web sont acceptées. Avant, une page
            // pouvait ouvrir ainsi un onglet sur file:// (fichiers de l'ordinateur).
            UserEvent::OpenInNewTab { url, private } => {
                if network::is_web(&url) {
                    self.open_tab(&url, private);
                }
            }
            UserEvent::Update(event) => self.handle_update(event),
            UserEvent::Reap => self.tabs.reap(),
            UserEvent::DownloadStarted { url, path, private } => self.download_started(url, path, private),
            UserEvent::DownloadDone { url, path, success } => self.download_done(&url, path, success),
            UserEvent::ImageSaved(result) => self.image_saved(result),
            UserEvent::DownloadSize { id, total } => {
                if let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) {
                    d.total = Some(total);
                }
                self.push_state();
            }
            UserEvent::Persist => {
                self.persist_pending.set(false);
                self.persist();
            }
            UserEvent::Tick => self.tick(),
            UserEvent::TimerDone { seconds, label } => self.timer_done(seconds, &label),
            UserEvent::Captured(result) => self.captured(result),
            UserEvent::AdFilters(filters, status) => self.ad_filters(filters, status),
            UserEvent::AdRulesReady => {
                self.reapply_ads();
                adblock::applied_everywhere();
            }
            UserEvent::ExtensionPrepared(result) => {
                self.extension_prepared(result);
                self.push_state();
            }
            UserEvent::ExtensionLoaded { id, result } => {
                self.extension_loaded(&id, result);
                self.push_state();
            }
            UserEvent::ExtensionPopup(raw) => self.extension_popup_message(&raw),
            UserEvent::ExtensionOpen(url) => self.extension_open(&url),
        }
        // L'onglet affiché a pu changer : s'il était en veille, on recharge sa page.
        self.wake_active();
        if !self.permission_requests.is_empty() {
            self.drop_stale_permissions();
        }
    }

    fn handle_chrome(&mut self, message: ChromeMessage) {
        match message {
            ChromeMessage::Ready => {}
            ChromeMessage::CheckUpdate => self.start_check(true),
            ChromeMessage::InstallUpdate => self.start_install(),
            ChromeMessage::RestartUpdate => {
                // On garde `restart` en cas d'échec : le bouton reste utilisable.
                if self.restart.is_some() {
                    self.persist();
                }
                if let Some(restart) = &self.restart {
                    match restart.launch() {
                        Ok(()) => std::process::exit(0),
                        Err(err) => eprintln!("redémarrage impossible : {err}"),
                    }
                }
            }
            ChromeMessage::OpenReleasePage => {
                if let UpdateState::Available { info } | UpdateState::Ready { info } = &self.update {
                    let page = info.page.clone();
                    self.open_tab(&page, false);
                }
            }
            ChromeMessage::Navigate { input } => {
                let engine = self.settings().search_engine;
                if let Some(url) = network::resolve_input(&input, engine) {
                    self.load_in_active(&url);
                }
            }
            ChromeMessage::NewTab => self.open_new_tab(),
            ChromeMessage::ToggleMute { id } => self.handle_shortcut(Some(id), Shortcut::ToggleMute),
            ChromeMessage::Shortcut { shortcut } => self.handle_shortcut(self.tabs.active_id(), shortcut),
            ChromeMessage::MoveTab { id, index } => self.tabs.move_tab(id, index),
            // Un onglet précis (bouton, menu) se ferme toujours ; Ctrl+W ou /fermer
            // (onglet actif) épargne un onglet épinglé.
            ChromeMessage::CloseTab { id: Some(id) } => self.close_tab(id),
            ChromeMessage::CloseTab { id: None } => {
                if let Some(id) = self.tabs.active_id() {
                    self.close_unpinned(id);
                }
            }
            ChromeMessage::ActivateTab { id } => {
                self.close_extension_popup();
                self.tabs.activate(id, self.page_visible());
            }
            // Mêmes actions que les raccourcis des pages : un seul endroit pour les deux.
            ChromeMessage::Back => self.handle_shortcut(None, Shortcut::Back),
            ChromeMessage::Forward => self.handle_shortcut(None, Shortcut::Forward),
            ChromeMessage::Reload => self.handle_shortcut(None, Shortcut::Reload),
            ChromeMessage::Home => {
                let url = self.settings().start_url();
                self.load_in_active(&url);
            }
            ChromeMessage::Expand { open } => {
                if open {
                    self.close_extension_popup();
                }
                self.expanded = open;
                self.update_pointer_watch(open);
                if layers::SUPPORTED {
                    self.layers.set_on_top(&self.window, &self.chrome, open);
                    // Repassée sous la page : l'interface peut redevenir opaque.
                    if !open {
                        self.chrome_call("navigateur.lowered && navigateur.lowered()");
                    }
                } else {
                    self.tabs.set_active_visible(!open);
                }
            }
            // Messages fréquents (à chaque image) : rien à redessiner dans l'interface.
            ChromeMessage::Shape { rects } => {
                self.layers.set_shape(&self.window, &self.chrome, rects);
                return;
            }
            ChromeMessage::WatchPointer { on } => {
                self.pointer_wanted = on;
                self.update_pointer_watch(on);
                return;
            }
            ChromeMessage::SidebarWidth { width, done } => {
                self.set_sidebar_width(width, done);
                if !done {
                    return;
                }
            }
            ChromeMessage::FocusPage => self.focus_page(),
            ChromeMessage::FocusBar => self.focus_bar(),
            ChromeMessage::SaveSettings { settings } => self.save_settings(settings),
            ChromeMessage::ToggleBookmark { id } => {
                if let Some(id) = id.or(self.tabs.active_id()) {
                    self.toggle_bookmark(id);
                }
            }
            ChromeMessage::Library { kind, query } => {
                self.send_library(kind, &query);
                return;
            }
            ChromeMessage::OpenUrl { url, new_tab, private } => {
                if network::is_web(&url) {
                    if private {
                        self.open_tab(&url, true);
                    } else if new_tab {
                        self.open_tab_in_background(&url);
                    } else {
                        self.load_in_active(&url);
                    }
                }
            }
            ChromeMessage::RemoveBookmark { url } => {
                if let Ok(mut lib) = self.library.write() {
                    lib.remove_bookmark(&url);
                }
                // La liste n'est plus renvoyée ici sans filtre : l'interface la redemande
                // elle-même avec la recherche en cours.
                self.reload_home_tabs();
            }
            ChromeMessage::RemoveVisit { url } => {
                if let Ok(mut lib) = self.library.write() {
                    lib.remove_visits(&url);
                }
                self.fulltext.remove(&url);
            }
            ChromeMessage::ClearHistory => {
                if let Ok(mut lib) = self.library.write() {
                    lib.clear_history();
                }
                self.fulltext.clear();
                self.send_library(LibraryKind::History, "");
            }
            ChromeMessage::RevealDownload { id } => {
                if let Some(d) = self.downloads.iter().find(|d| d.id == id) {
                    downloads::reveal(&d.path);
                }
            }
            ChromeMessage::OpenDownload { id } => {
                if let Some(d) = self.downloads.iter().find(|d| d.id == id && d.state == downloads::State::Done) {
                    downloads::open(&d.path);
                }
            }
            ChromeMessage::ClearDownloads => {
                self.downloads.retain(|d| d.state == downloads::State::Progress);
            }
            ChromeMessage::CancelDownload { id } => self.cancel_download(id),
            ChromeMessage::Strip { open } => {
                self.strip = open;
                self.relayout();
            }
            ChromeMessage::Find { text, backwards } => {
                self.find(&text, backwards);
                return;
            }
            ChromeMessage::FindClose => {
                self.find("", false);
                self.focus_page();
            }
            // À chaque touche : la réponse part seule, l'état complet n'a pas changé.
            ChromeMessage::Suggest { query } => {
                self.send_suggestions(&query);
                return;
            }
            ChromeMessage::Command { input, run } => {
                self.run_command(&input, run);
                if !run {
                    return;
                }
            }
            ChromeMessage::PermissionAnswer { request, allow, remember } => {
                self.answer_permission(request, allow, remember)
            }
            ChromeMessage::ForgetPermission { host, permission } => {
                if let Ok(mut sites) = self.sites.write() {
                    sites.forget_permission(&host, permission);
                }
            }
            ChromeMessage::SleepTab { id } => {
                if self.tabs.sleep(id) {
                    self.schedule_reap();
                }
            }
            ChromeMessage::WatchMemory { on } => {
                self.watch_memory(on);
                return;
            }
            ChromeMessage::SleepHidden => {
                if self.tabs.sleep_hidden() {
                    self.schedule_reap();
                }
            }
            ChromeMessage::SetPermission { host, permission, allow } => {
                if let Ok(mut sites) = self.sites.write() {
                    sites.set_permission(&host, permission, allow);
                }
                self.schedule_persist();
            }
            ChromeMessage::ClearSiteData => self.clear_site_data(),
            ChromeMessage::SetSiteAds { host, on } => self.set_site_ads(&host, on),
            ChromeMessage::UpdateAdLists => {
                self.adblock.updating = true;
                adblock::load(self.proxy.clone(), true);
            }
            ChromeMessage::PinTab { id, pinned } => self.tabs.set_pinned(id, pinned),
            ChromeMessage::GroupTab { id, group } => self.tabs.set_group(id, group),
            ChromeMessage::NewGroup { id, name } => {
                self.tabs.new_group(id, &name);
            }
            ChromeMessage::RenameGroup { group, name } => self.tabs.rename_group(group, &name),
            ChromeMessage::Ungroup { group } => self.tabs.ungroup(group),
            ChromeMessage::CloseGroup { group } => {
                for id in self.tabs.group_members(group) {
                    self.close_tab(id);
                }
            }
            ChromeMessage::ToggleGroup { group } => {
                if self.tabs.toggle_group(group) {
                    self.schedule_reap();
                }
            }
            ChromeMessage::Reader => self.toggle_reader(),
            ChromeMessage::ImportSources => self.send_import_sources(),
            ChromeMessage::Import { sources, files } => self.import(&sources, &files),
            ChromeMessage::InstallExtension { input } => self.install_extension(&input),
            ChromeMessage::InstallExtensionFile { name, data } => self.install_extension_file(&name, &data),
            ChromeMessage::SetExtension { id, enabled } => self.set_extension(&id, enabled),
            ChromeMessage::RemoveExtension { id } => self.remove_extension(&id),
            ChromeMessage::ExtensionOptions { id } => self.extension_options(&id),
            ChromeMessage::ExtensionAction { id, x, y, w, h } => self.extension_action(&id, (x, y, w, h)),
            ChromeMessage::CloseExtensionPopup => self.close_extension_popup(),
            ChromeMessage::ClearExtensionError => self.extensions.error = None,
        }
        // L'interface a pu changer sa copie de l'état sans attendre (réglages,
        // liste vidée…) : on lui renvoie tout, pour qu'elle reparte du vrai.
        self.full_state.set(true);
        self.push_state();
    }

    fn handle_page(&mut self, id: TabId, message: PageMessage) {
        // Un raccourci ne vient que de l'onglet affiché : un onglet en arrière-plan
        // (ou un script de page) ne doit pas imprimer, capturer ou fermer à sa place.
        // Ceux qui ouvrent des onglets sont limités comme les ouvertures des pages.
        if let Some(shortcut) = message.shortcut
            && Some(id) == self.tabs.active_id()
            && (!matches!(shortcut, Shortcut::NewTab | Shortcut::NewPrivateTab | Shortcut::ReopenTab) || self.allow_page_tab())
        {
            self.handle_shortcut(Some(id), shortcut);
            self.push_state();
        }
        if let Some(site) = message.site {
            if let Some(tab) = self.tabs.get_mut(id)
                && !network::is_home(tab.url())
                && !tab.on_internal_page()
            {
                let (icon, name) = ui::clean_site(site);
                tab.icon = icon;
                tab.site_name = name;
            }
            self.push_state();
        }
        if let Some(url) = message.open_tab
            && network::is_web(&url)
            && self.allow_page_tab()
        {
            self.open_tab_in_background(&url);
        }
        if let Some(link) = message.link
            && network::is_web(&link.url)
            && self.allow_page_tab()
        {
            match link.how {
                LinkHow::Tab => self.open_tab(&link.url, false),
                LinkHow::Background => self.open_tab_in_background(&link.url),
                LinkHow::Private => self.open_tab(&link.url, true),
            }
        }
        // Même limite que les onglets : une page ne peut pas remplir les
        // téléchargements (ou Photos) à coups de messages.
        if let Some(image) = message.image
            && network::is_web(&image.url)
            && self.allow_page_tab()
        {
            self.save_image(id, image.url, image.how);
        }
        // Bouton « Ajouter à Feather » du Chrome Web Store : seulement depuis
        // l'onglet affiché, s'il est bien sur le store, et pour une page d'extension.
        if let Some(url) = message.store_install
            && Some(id) == self.tabs.active_id()
            && self.tabs.get(id).is_some_and(|t| network::host_of(t.url()).as_deref() == Some("chromewebstore.google.com"))
            && let Some(store_id) = extensions::store_page(&url)
        {
            self.install_extension(&store_id);
            self.push_state();
            // Le panneau des extensions montre le téléchargement, puis l'extension installée.
            self.focus_chrome("navigateur.openExtensions()");
        }
        if let Some(audible) = message.audible {
            if let Some(tab) = self.tabs.get_mut(id) {
                tab.audible = audible;
            }
            self.push_state();
        }
        if let Some(result) = message.find_result
            && Some(id) == self.tabs.active_id()
        {
            self.chrome_call(&format!("navigateur.setFind({})", json!(result)));
        }
        if let Some(metrics) = message.metrics {
            self.page_metrics(id, metrics);
        }
        if let Some(text) = message.text {
            self.index_page(id, &text);
        }
        if let Some(readable) = message.readable
            && let Some(tab) = self.tabs.get_mut(id)
            && tab.readable != readable
            && !tab.on_internal_page()
        {
            tab.readable = readable;
            self.push_state();
        }
        if let Some(article) = message.reader {
            self.open_reader(id, article);
        }
        if message.reader_failed {
            self.chrome_call("navigateur.showNotice('Feather n\\'a pas trouvé d\\'article à lire sur cette page.')");
        }
        // Clic dans la page pendant qu'un panneau est ouvert par-dessus : il se ferme.
        if message.pressed && self.expanded {
            self.chrome_call("navigateur.closePanel()");
        }
        // Comme dans Chrome, la fenêtre d'une extension se ferme au clic dans la page.
        if message.pressed {
            self.close_extension_popup();
        }
        if let Some(request) = message.permission {
            self.ask_permission(id, request);
        }
        if let Some(request) = message.cosmetic {
            self.hide_ads(id, request);
        }
        // Seule la page d'accueil intégrée a le droit d'utiliser sa barre de recherche.
        let Some(tab) = self.tabs.get(id) else { return };
        if !network::is_home(tab.url()) {
            return;
        }
        if let Some(input) = message.navigate {
            let engine = self.settings().search_engine;
            if let Some(url) = network::resolve_input(&input, engine)
                && let Some(w) = tab.webview()
            {
                let _ = w.load_url(&url);
            }
        }
        if let Some(text) = message.open_bar {
            let text = serde_json::to_string(&text).unwrap_or_default();
            self.focus_chrome(&format!("navigateur.openBar({text})"));
        }
    }

    /// Action d'un raccourci, venu d'une page ou de l'interface. `id` : l'onglet
    /// concerné (celui de la page, ou l'onglet actif). L'appelant redessine ensuite.
    fn handle_shortcut(&mut self, id: Option<TabId>, shortcut: Shortcut) {
        match shortcut {
            Shortcut::FocusBar => self.focus_bar(),
            Shortcut::Back => self.go_back(),
            Shortcut::Forward => self.with_active(|w| w.go_forward()),
            Shortcut::Reload => self.reload(),
            Shortcut::Settings => self.focus_chrome("navigateur.openSettings()"),
            Shortcut::NewTab => self.open_new_tab(),
            Shortcut::CloseTab => {
                if let Some(id) = id {
                    self.close_unpinned(id);
                }
            }
            Shortcut::ToggleMute => {
                if let Some(tab) = id.and_then(|id| self.tabs.get_mut(id)) {
                    tab.muted = !tab.muted;
                    tab.apply_mute();
                }
            }
            Shortcut::ToggleBookmark => {
                if let Some(id) = id {
                    self.toggle_bookmark(id);
                }
            }
            Shortcut::NextTab => self.tabs.cycle(1, self.page_visible()),
            Shortcut::PreviousTab => self.tabs.cycle(-1, self.page_visible()),
            Shortcut::Find => self.focus_chrome("navigateur.openFind()"),
            Shortcut::ZoomIn => self.zoom(1),
            Shortcut::ZoomOut => self.zoom(-1),
            Shortcut::ZoomReset => self.zoom(0),
            Shortcut::ReopenTab => {
                if let Some(url) = self.tabs.pop_closed() {
                    self.open_tab(&url, false);
                }
            }
            Shortcut::Print => self.with_active(|w| w.print()),
            Shortcut::NewPrivateTab => self.open_tab(network::HOME_URL, true),
            Shortcut::TaskManager => self.focus_chrome("navigateur.openMemory()"),
            Shortcut::Capture => self.capture(false),
            Shortcut::Focus => {
                self.focus = !self.focus;
                self.relayout();
                if self.focus
                    && let Some(tab) = self.tabs.active()
                {
                    tab.eval("window.__featherToast && __featherToast('Mode concentration · Ctrl+Maj+F pour revenir')");
                }
            }
        }
    }

    /// Une page a-t-elle encore le droit d'ouvrir un onglet ? (voir `PAGE_TAB_LIMIT`)
    fn allow_page_tab(&mut self) -> bool {
        let now = Instant::now();
        while self
            .page_opens
            .front()
            .is_some_and(|t| now.duration_since(*t) > PAGE_TAB_WINDOW)
        {
            self.page_opens.pop_front();
        }
        if self.page_opens.len() >= PAGE_TAB_LIMIT {
            return false;
        }
        self.page_opens.push_back(now);
        true
    }

    /// Un nouvel onglet s'ouvre toujours sur l'accueil Feather (heure et recherche).
    fn open_new_tab(&mut self) {
        self.open_tab(network::HOME_URL, false);
    }

    /// Zone des pages selon la position et la largeur de la barre, et le bandeau du bas.
    fn page_bounds(&self) -> wry::Rect {
        let s = self.settings();
        let frame = ui::Frame {
            bar: s.bar_position,
            style: s.sidebar_style,
            side: s.sidebar_width,
            strip: self.strip,
            focus: self.focus,
        };
        ui::page_bounds(&self.window, &frame)
    }

    /// Panneau ouvert par-dessus la page (dont la barre au survol), ou poignée
    /// de la barre survolée : la position
    /// de la souris est relue toutes les `POINTER_POLL` et envoyée à l'interface
    /// quand elle change. Sous macOS, l'interface ne reçoit pas toujours les
    /// mouvements de la souris au-dessus de la page : sans ça, la barre au
    /// survol ne savait pas que la souris l'avait quittée et restait ouverte.
    /// Démarre ou arrête la lecture de la souris. `fresh` : la position est
    /// renvoyée tout de suite, même inchangée.
    fn update_pointer_watch(&mut self, fresh: bool) {
        let on = layers::SUPPORTED && (self.expanded || self.pointer_wanted);
        if !on {
            self.pointer_due = None;
        } else if fresh || self.pointer_due.is_none() {
            self.pointer_sent = None;
            self.pointer_due = Some(Instant::now());
        }
    }

    fn watch_pointer(&mut self) {
        let Some(due) = self.pointer_due else { return };
        let now = Instant::now();
        if now < due {
            return;
        }
        self.pointer_due = Some(now + POINTER_POLL);
        let at = layers::pointer(&self.chrome).map(|(x, y)| (x.round() as i32, y.round() as i32));
        if self.pointer_sent == Some(at) {
            return;
        }
        self.pointer_sent = Some(at);
        let args = at.map_or_else(|| "null, null".to_owned(), |(x, y)| format!("{x}, {y}"));
        self.chrome_call(&format!("navigateur.pointer && navigateur.pointer({args})"));
    }

    /// La page active reste affichée : toujours quand les panneaux passent
    /// par-dessus, sinon seulement si aucun panneau n'est ouvert.
    fn page_visible(&self) -> bool {
        !self.expanded || layers::SUPPORTED
    }

    /// La barre latérale change de largeur ; on enregistre quand le bouton est relâché.
    fn set_sidebar_width(&mut self, width: u32, save: bool) {
        let width = width.clamp(settings::SIDEBAR_MIN, settings::SIDEBAR_MAX);
        if let Ok(mut s) = self.settings.write() {
            s.sidebar_width = width;
            if save && let Err(err) = s.save() {
                eprintln!("paramètres non enregistrés : {err}");
            }
        }
        self.tabs.set_bounds(self.page_bounds());
    }

    fn open_tab(&mut self, url: &str, private: bool) {
        let ctx = PageContext {
            window: &self.window,
            surface: &self.surface,
            proxy: &self.proxy,
            settings: &self.settings,
            library: &self.library,
            sites: &self.sites,
            reader: &self.reader,
            bounds: self.page_bounds(),
        };
        match self.tabs.open(url, &ctx, self.page_visible(), private) {
            Ok(_) => self.focus_page(),
            Err(err) => eprintln!("impossible d'ouvrir l'onglet : {err}"),
        }
        // La nouvelle page est créée au-dessus de tout, panneau ouvert compris.
        self.layers.restack(&self.window, &self.chrome);
        self.push_state();
    }

    /// Recharge la page de l'onglet affiché s'il était en veille.
    fn wake_active(&mut self) {
        let Some(id) = self.tabs.active().filter(|t| t.is_asleep()).map(|t| t.id) else {
            return;
        };
        let ctx = PageContext {
            window: &self.window,
            surface: &self.surface,
            proxy: &self.proxy,
            settings: &self.settings,
            library: &self.library,
            sites: &self.sites,
            reader: &self.reader,
            bounds: self.page_bounds(),
        };
        if let Err(err) = self.tabs.wake(id, &ctx, self.page_visible()) {
            eprintln!("impossible de réveiller l'onglet : {err}");
        }
        self.layers.restack(&self.window, &self.chrome);
        self.focus_page();
        self.push_state();
    }

    /// Ouvre un onglet sans le montrer : on reste sur la page en cours (clic de molette).
    fn open_tab_in_background(&mut self, url: &str) {
        let previous = self.tabs.active().map(|t| t.id);
        self.open_tab(url, false);
        if let Some(id) = previous {
            self.tabs.activate(id, self.page_visible());
            // `open_tab` a donné le focus clavier au nouvel onglet, caché : on le
            // rend à la page visible, sinon les touches partaient dans l'onglet caché.
            self.focus_page();
            self.push_state();
        }
    }

    /// Ferme un onglet ; on garde toujours au moins un onglet ouvert.
    fn close_tab(&mut self, id: TabId) {
        if let Ok(mut articles) = self.reader.write() {
            articles.remove(&id);
        }
        self.tabs.close(id, self.page_visible());
        self.schedule_reap();
        if self.tabs.is_empty() {
            self.open_new_tab();
        }
    }

    /// Les pages fermées ou mises en veille sont détruites trois secondes plus tard.
    fn schedule_reap(&self) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(3100));
            let _ = proxy.send_event(UserEvent::Reap);
        });
    }

    fn load_in_active(&mut self, url: &str) {
        match self.tabs.active().and_then(|t| t.webview()) {
            Some(w) => {
                let _ = w.load_url(url);
                self.focus_page();
            }
            None => self.open_tab(url, false),
        }
    }

    fn with_active(&self, action: impl FnOnce(&WebView) -> wry::Result<()>) {
        if let Some(w) = self.tabs.active().and_then(|t| t.webview()) {
            let _ = action(w);
        }
    }

    fn focus_page(&self) {
        if !self.expanded {
            self.with_active(|w| w.focus());
        }
    }

    /// Sur l'accueil, la barre est au milieu de la page ; ailleurs, elle
    /// s'ouvre par-dessus la page.
    fn focus_bar(&self) {
        match self.tabs.active() {
            Some(tab) if network::is_home(tab.url()) && !self.expanded && tab.webview().is_some() => {
                self.focus_page();
                tab.eval("window.focusSearch && focusSearch()");
            }
            _ => self.focus_chrome("navigateur.openBar('')"),
        }
    }

    fn focus_chrome(&self, script: &str) {
        // Le panneau qui s'ouvre lit l'état (onglet actif, réglages) : il part d'abord.
        self.flush_state();
        let _ = self.chrome.focus();
        self.chrome_call(script);
    }

    /// Appelle une fonction de l'interface (`navigateur.…`), si elle est prête.
    fn chrome_call(&self, script: &str) {
        let _ = self
            .chrome
            .evaluate_script(&format!("window.navigateur && {script}"));
    }

    fn toggle_bookmark(&mut self, id: TabId) {
        let Some(tab) = self.tabs.get(id) else { return };
        if let Ok(mut lib) = self.library.write() {
            lib.toggle_bookmark(tab.url(), &tab.title, tab.icon.as_deref());
        }
        self.reload_home_tabs();
        self.push_state();
    }

    /// Les pages d'accueil ouvertes se redessinent (nouveaux paramètres ou favoris).
    fn reload_home_tabs(&self) {
        for w in self.tabs.home_tabs().filter_map(|t| t.webview()) {
            let _ = w.reload();
        }
    }

    fn save_settings(&mut self, settings: Settings) {
        let settings = settings.cleaned();
        if let Err(err) = settings.save() {
            eprintln!("paramètres non enregistrés : {err}");
        }
        let before = self.settings().clone();
        if let Ok(mut lib) = self.library.write() {
            lib.prune(settings.history_days);
        }
        self.fulltext.prune(settings.history_days);
        // Désactiver la recherche dans le texte des pages efface ce qui était gardé.
        if before.index_pages && !settings.index_pages {
            self.fulltext.clear();
        }
        if before.blocks_trackers() != settings.blocks_trackers() {
            for w in self.tabs.iter().filter_map(|t| t.webview()) {
                trackers::set_enabled(w, settings.blocks_trackers());
            }
        }
        let ads_changed = before.blocks_ads() != settings.blocks_ads();
        adblock::set_enabled(settings.blocks_ads());
        let economy_changed = before.economy != settings.economy;
        if let Ok(mut current) = self.settings.write() {
            *current = settings;
        }
        if economy_changed {
            let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
            for id in ids {
                self.apply_economy(id);
            }
        }
        if ads_changed {
            self.reapply_ads();
        }
        self.relayout();
        // Les pages d'accueil ouvertes reprennent le nouveau prénom et thème.
        self.reload_home_tabs();
    }

    /// Envoie au panneau Bibliothèque la liste demandée.
    fn send_library(&self, kind: LibraryKind, query: &str) {
        let items = match kind {
            LibraryKind::Downloads => json!(self.downloads),
            LibraryKind::Bookmarks => match self.library.read() {
                Ok(lib) => json!(lib.search_bookmarks(query)),
                Err(_) => json!([]),
            },
            LibraryKind::History => self.history_items(query),
        };
        let kind = match kind {
            LibraryKind::Bookmarks => "bookmarks",
            LibraryKind::History => "history",
            LibraryKind::Downloads => "downloads",
        };
        let data = json!({ "kind": kind, "items": items });
        self.chrome_call(&format!("navigateur.setLibrary({data})"));
    }

    /// Prévoit un enregistrement dans deux secondes, pour regrouper les changements.
    fn schedule_persist(&self) {
        if self.persist_pending.replace(true) {
            return;
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            let _ = proxy.send_event(UserEvent::Persist);
        });
    }

    /// Enregistre tout de suite l'historique, les favoris, les réglages par site,
    /// les notes, le texte des pages et les onglets ouverts (seul ce qui a changé).
    fn persist(&mut self) {
        if let Ok(mut lib) = self.library.write()
            && let Err(err) = lib.save()
        {
            eprintln!("historique ou favoris non enregistrés : {err}");
        }
        if let Ok(mut sites) = self.sites.write()
            && let Err(err) = sites.save()
        {
            eprintln!("réglages des sites non enregistrés : {err}");
        }
        if let Err(err) = self.notes.save() {
            eprintln!("notes non enregistrées : {err}");
        }
        if let Err(err) = self.fulltext.save() {
            eprintln!("texte des pages non enregistré : {err}");
        }
        if self.tabs.is_empty() {
            return;
        }
        // `push_state` prévoit un enregistrement à chaque changement (son, icône…) :
        // on n'écrit la session que si les onglets ont vraiment changé.
        let session = self.tabs.session();
        if self.saved_session.borrow().as_ref() == Some(&session) {
            return;
        }
        match library::save_session(&session) {
            Ok(()) => *self.saved_session.borrow_mut() = Some(session),
            Err(err) => eprintln!("session non enregistrée : {err}"),
        }
    }

    /// Les réglages, lus sur place : avant, chaque lecture copiait tous les
    /// réglages (textes compris) pour n'en garder qu'une valeur. Ne pas garder
    /// ce verrou en appelant ce qui écrit les réglages.
    fn settings(&self) -> std::sync::RwLockReadGuard<'_, Settings> {
        self.settings.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn relayout(&mut self) {
        self.close_extension_popup();
        let _ = self.chrome.set_bounds(ui::chrome_bounds(&self.window));
        self.tabs.set_bounds(self.page_bounds());
    }

    fn start_check(&mut self, manual: bool) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready { .. }) {
            return;
        }
        self.update = UpdateState::Checking;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let found = update::check();
            let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Checked { found, manual }));
        });
    }

    fn start_install(&mut self) {
        let UpdateState::Available { info } = &self.update else { return };
        let info = info.clone();
        // Un reste d'un essai précédent fausserait la progression.
        if let Some(path) = update::download_path() {
            let _ = std::fs::remove_file(path);
        }
        self.update = UpdateState::Downloading { info: info.clone(), received: 0, speed: 0 };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = update::install(&info);
            let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Installed(result)));
        });
    }

    fn handle_update(&mut self, event: UpdateEvent) {
        match event {
            UpdateEvent::Checked { found, manual } => {
                self.update = match found {
                    Ok(Some(info)) => UpdateState::Available { info },
                    Ok(None) => UpdateState::UpToDate,
                    // Une vérification automatique qui échoue (hors ligne) reste silencieuse.
                    Err(_) if !manual => UpdateState::Idle,
                    Err(message) => UpdateState::Error { message },
                };
            }
            UpdateEvent::Installed(result) => {
                let info = match &self.update {
                    UpdateState::Downloading { info, .. } => Some(info.clone()),
                    _ => None,
                };
                self.update = match (result, info) {
                    (Ok(restart), Some(info)) => {
                        self.restart = Some(restart);
                        UpdateState::Ready { info }
                    }
                    (Err(message), _) => UpdateState::Error { message },
                    _ => UpdateState::Idle,
                };
            }
        }
        self.push_state();
    }

    /// L'état a changé : il sera envoyé à l'interface après les événements en
    /// attente. Un message de page en déclenchait jusqu'à trois d'affilée, et
    /// chacun refaisait tout le JSON (onglets, réglages, commandes…).
    fn push_state(&self) {
        self.state_dirty.set(true);
    }

    /// Un titre a changé : envoyé tout de suite, ou à la fin de la pause s'il en
    /// arrive trop vite. Le titre est gardé dans l'onglet dans tous les cas.
    fn title_changed(&self) {
        if self.titles_waiting.get() {
            return;
        }
        let since = self.last_title_push.get().elapsed();
        if since >= TITLE_PAUSE {
            self.last_title_push.set(Instant::now());
            self.push_state();
            return;
        }
        self.titles_waiting.set(true);
        let proxy = self.proxy.clone();
        let wait = TITLE_PAUSE - since;
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            let _ = proxy.send_event(UserEvent::TitlesDue);
        });
    }

    fn flush_state(&self) {
        if self.state_dirty.replace(false) {
            self.sync_extension_tabs();
            self.send_state();
        }
    }

    /// Envoie à l'interface l'état complet (onglets et paramètres) à redessiner.
    fn send_state(&self) {
        let active = self.tabs.active().and_then(|t| t.webview());
        let lib = self.library.read().ok();
        let permissions = self.sites.read().map(|s| s.decisions()).unwrap_or_default();
        let state = json!({
            "tabs": self.tabs.snapshot(|url| lib.as_ref().is_some_and(|l| l.is_bookmarked(url))),
            "downloads": self.downloads,
            "can_back": active.and_then(|w| w.can_go_back().ok()).unwrap_or(false),
            "can_forward": active.and_then(|w| w.can_go_forward().ok()).unwrap_or(false),
            "settings": &*self.settings(),
            "update": self.update,
            "permissions": permissions,
            "permission_prompt": self.permission_prompt(),
            "site": self.site_info(),
            "groups": self.tabs.groups(),
            "adblock": self.adblock,
            "focus": self.focus,
            "extensions": self.extensions.snapshot(),
        });
        drop(lib);
        let serde_json::Value::Object(state) = state else { return };
        let mut sent = self.sent_state.borrow_mut();
        if self.full_state.replace(false) || sent.is_empty() {
            // État complet, avec ce qui ne change jamais (version, commandes…).
            let mut full = state.clone();
            full.insert("version".into(), json!(update::CURRENT));
            full.insert("native_commands".into(), native_commands());
            full.insert("overlay".into(), json!(layers::SUPPORTED));
            self.chrome_call(&format!("navigateur.setState({})", serde_json::Value::Object(full)));
        } else {
            // Seulement les parties qui ont changé depuis le dernier envoi : souvent
            // un onglet (titre, chargement) ou un téléchargement, pas les réglages.
            let changed: serde_json::Map<String, serde_json::Value> =
                state.iter().filter(|(k, v)| sent.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect();
            if changed.is_empty() {
                return;
            }
            self.chrome_call(&format!("navigateur.patchState({})", serde_json::Value::Object(changed)));
        }
        *sent = state;
        drop(sent);
        self.schedule_persist();
    }
}
