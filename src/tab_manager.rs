//! Les onglets : une webview par onglet, toutes possédées par le `TabManager`.
//!
//! Fermer un onglet le retire du `Vec` : la webview est alors détruite (`drop`)
//! et sa mémoire libérée. Aucun `Rc`/`RefCell` : les rappels des webviews ne
//! connaissent que l'identifiant de leur onglet et envoient des messages à la
//! boucle d'événements.
//!
//! Un onglet peut aussi être « en veille » : sa webview est détruite mais on
//! garde son adresse, son titre et son icône ; elle est recréée quand on y revient.
//! Son historique Précédent/Suivant et sa position dans la page sont gardés
//! aussi (voir `Souvenir`), pour que la veille passe presque inaperçue.

use std::borrow::Cow;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tao::event_loop::EventLoopProxy;
use tao::window::Window;
use wry::http::{Response, header::CONTENT_TYPE};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder};

use crate::UserEvent;
use crate::downloads;
use crate::media::MediaState;
use crate::errorpage::{self, ErrorKind};
use crate::extensions;
use crate::library::{Library, SavedSplit, Session, TabGroup};
use crate::network;
use crate::reader;
use crate::settings::Settings;
use crate::sitedata::SiteData;
use crate::softsleep::{self, Repos};
use crate::trackers;
use crate::adblock;
use crate::ui::{self, Surface};
use crate::weight::Weight;

pub type TabId = u32;

/// Nombre d'onglets fermés que `Ctrl+Maj+T` peut rouvrir.
const CLOSED_MEMORY: usize = 10;

/// Souvenirs gardés au plus, les plus récents (voir `Souvenir`) : celui d'un
/// site lourd peut peser plusieurs Mo. Au-delà, ou après `GARDE_SOUVENIR`,
/// l'onglet se réveille avec sa position seulement (voir `Tab::scroll`).
const MAX_SOUVENIRS: usize = 20;
const GARDE_SOUVENIR: Duration = Duration::from_secs(3600);

/// Couleurs proposées aux groupes, l'une après l'autre.
const GROUP_COLORS: [&str; 6] = ["#5b5bd6", "#2f9e5b", "#e8a317", "#e5484d", "#0e9fb5", "#c2489b"];

pub struct Tab {
    pub id: TabId,
    pub title: String,
    url: String,
    pub loading: bool,
    /// La page joue du son (vidéo ou musique), d'après `assets/page.js`.
    pub audible: bool,
    /// Une vidéo de la page joue sans son (d'après la page).
    pub quiet: bool,
    /// Son et caméra lus dans le moteur, cadres compris (voir `media.rs`).
    pub engine: MediaState,
    /// Un champ de la page a été modifié depuis son chargement.
    pub dirty: bool,
    /// La page filme ou enregistre (d'après la page).
    pub capturing: bool,
    /// L'utilisateur a coupé le son de cet onglet.
    pub muted: bool,
    /// Adresse de l'icône du site et nom du site, donnés par la page.
    pub icon: Option<String>,
    pub site_name: Option<String>,
    /// Onglet privé : ni historique, ni session, ni cookies gardés.
    pub private: bool,
    /// Dernière fois que l'onglet était au premier plan (pour la mise en veille).
    pub last_active: Instant,
    /// Dernière fois que la page jouait ou filmait, vu au dernier relevé (voir
    /// `App::probe_media`).
    pub heard: Option<Instant>,
    /// Mémoire et processeur de la page (voir `weight.rs`).
    pub weight: Weight,
    /// Veille douce de la page cachée (voir `softsleep.rs`).
    pub repos: Repos,
    /// La page ressemble à un article : le mode lecture est proposé.
    pub readable: bool,
    /// Mode lecture : l'adresse de la page d'origine de l'article affiché.
    pub reading: Option<String>,
    /// Onglet épinglé : en haut de la colonne, jamais fermé par Ctrl+W, rouvert à chaque démarrage.
    pub pinned: bool,
    /// Groupe de l'onglet (voir `TabGroup`).
    pub group: Option<u32>,
    /// Échec du dernier chargement, affiché par une page d'erreur Feather :
    /// la cause et l'adresse en échec (voir `errorpage.rs`).
    pub failed: Option<(ErrorKind, String)>,
    /// Bloqueur de publicités posé sur la page (`None` : pas encore ; voir `adblock::apply`).
    pub ads: Option<bool>,
    /// macOS : la page vient d'être rechargée parce qu'une redirection l'a fait
    /// entrer sur Google (ou un autre site à identité Safari) ou en sortir (voir
    /// `match_identity`) ; une seule fois de suite.
    pub identity_retry: bool,
    /// Mode lecture demandé (clic sur le bouton livre) : l'article attendu peut
    /// arriver, une seule fois (voir `PageMessage::for_untrusted`).
    pub reader_asked: bool,
    /// Clé secrète de l'onglet, connue du seul script de Feather dans la page :
    /// elle distingue ses messages de ceux du site (voir `ui::split_key`).
    key: String,
    /// Hôte de la page, partagé avec les rappels de la webview (autorisations, traqueurs).
    host: Arc<RwLock<String>>,
    /// Historique et position de la page détruite, rendus au réveil (macOS 12
    /// ou plus), avec l'heure où ils ont été pris (voir `forget_souvenirs`).
    souvenir: Option<(Instant, Souvenir)>,
    /// Dernière position dans la page, envoyée par la page quand l'onglet est
    /// caché (voir `assets/page.js`) : sert au réveil là où `Souvenir` manque.
    pub scroll: Option<Scroll>,
    /// Position à rendre à la page quand son chargement se termine (après un réveil).
    pub scroll_due: Option<Scroll>,
    /// `None` : l'onglet est en veille.
    webview: Option<WebView>,
}

impl Tab {
    /// La webview de l'onglet, sauf s'il est en veille.
    pub fn webview(&self) -> Option<&WebView> {
        self.webview.as_ref()
    }

    /// La page affichée est une page Feather (erreur, lecture) à la place du site :
    /// ni historique, ni titre, ni icône venant d'elle.
    pub fn on_internal_page(&self) -> bool {
        self.failed.is_some() || self.reading.is_some()
    }

    pub fn is_asleep(&self) -> bool {
        self.webview.is_none()
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Change l'adresse connue de l'onglet (et l'hôte vu par ses rappels).
    pub fn set_url(&mut self, url: String) {
        if let Ok(mut host) = self.host.write() {
            *host = network::host_of(&url).unwrap_or_default();
        }
        self.url = url;
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn host(&self) -> String {
        self.host.read().map(|h| h.clone()).unwrap_or_default()
    }

    /// Exécute un script dans la page (rien si l'onglet est en veille).
    pub fn eval(&self, script: &str) {
        if let Some(w) = &self.webview {
            let _ = w.evaluate_script(script);
        }
    }

    /// Coupe ou remet le son des vidéos et musiques de la page (voir assets/page.js),
    /// et sous Windows de tout l'onglet, cadres et WebAudio compris.
    pub fn apply_mute(&self) {
        self.eval(&format!("window.__featherSetMuted && __featherSetMuted({})", self.muted));
        if let Some(w) = &self.webview {
            crate::media::set_muted(w, self.muted);
        }
    }

    /// Fait du son, d'après la page ou (Windows) d'après le moteur, cadres compris.
    pub fn sounds(&self) -> bool {
        self.audible || (cfg!(windows) && self.engine.playing)
    }

    /// La page fait quelque chose qu'une mise en veille interromprait : du son
    /// (cadres compris), la caméra ou le micro, un formulaire en cours. Sous
    /// macOS, le moteur dit seulement qu'un média joue : une vidéo muette de la
    /// page elle-même (fond animé) ne compte pas.
    pub fn busy(&self) -> bool {
        self.sounds()
            || self.capturing
            || self.dirty
            || self.engine.capturing
            || (self.engine.playing && !self.quiet)
    }
}

/// Ce que l'interface reçoit pour dessiner les pastilles.
#[derive(Serialize)]
pub struct TabInfo<'a> {
    pub id: TabId,
    pub title: &'a str,
    pub url: &'a str,
    pub active: bool,
    pub loading: bool,
    pub home: bool,
    pub audible: bool,
    pub muted: bool,
    pub icon: Option<&'a str>,
    pub site_name: Option<&'a str>,
    pub bookmarked: bool,
    pub private: bool,
    pub asleep: bool,
    /// Connexion chiffrée (https) ou page intégrée.
    pub secure: bool,
    pub weight: Weight,
    /// Veille douce : éveillé, allégé ou gelé.
    pub repos: Repos,
    /// La page affichée est une page d'erreur Feather.
    pub error: bool,
    pub pinned: bool,
    pub group: Option<u32>,
    pub readable: bool,
    pub reading: bool,
}

/// Une zone de la fenêtre : x, y, largeur et hauteur, en pixels logiques.
pub type Area = (f64, f64, f64, f64);

/// Part d'un côté de la vue partagée, au moins et au plus.
const SPLIT_RATIO_MIN: f64 = 0.2;
const SPLIT_RATIO_MAX: f64 = 0.8;
/// En dessous de cette largeur par page, les deux pages passent l'une sur
/// l'autre d'elles-mêmes, et reviennent côte à côte quand la fenêtre s'élargit.
const SPLIT_NARROW: f64 = 280.0;

/// Vue partagée : deux onglets affichés ensemble dans la zone des pages, côte
/// à côte ou l'un sur l'autre. Deux au plus, pour la mémoire : chaque côté
/// affiché garde sa page éveillée (ni veille, ni veille douce). Elle est à
/// l'écran quand l'onglet actif en est un côté ; l'onglet actif est le côté
/// où l'on tape (barre d'adresse, raccourcis, recherche dans la page).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Split {
    /// À gauche (ou en haut).
    pub first: TabId,
    /// À droite (ou en bas).
    pub second: TabId,
    /// Part de la place donnée au premier.
    pub ratio: f64,
    /// L'un sur l'autre, choisi par l'utilisateur.
    pub stacked: bool,
}

impl Split {
    pub fn has(&self, id: TabId) -> bool {
        self.first == id || self.second == id
    }

    /// L'autre côté de cet onglet.
    pub fn other(&self, id: TabId) -> Option<TabId> {
        if id == self.first {
            Some(self.second)
        } else if id == self.second {
            Some(self.first)
        } else {
            None
        }
    }

    /// L'un sur l'autre : choisi, ou parce que la zone est trop étroite pour
    /// deux pages lisibles côte à côte.
    pub fn stacked_in(&self, width: f64) -> bool {
        self.stacked || (width - ui::PAGE_GAP) * self.ratio.min(1.0 - self.ratio) < SPLIT_NARROW
    }

    /// Les places du premier et du second dans la zone des pages, séparés par
    /// l'espace habituel entre la page et la barre (la poignée s'y loge).
    pub fn rects(&self, (x, y, w, h): Area) -> [Area; 2] {
        if self.stacked_in(w) {
            let first = ((h - ui::PAGE_GAP) * self.ratio).round();
            [(x, y, w, first), (x, y + first + ui::PAGE_GAP, w, h - first - ui::PAGE_GAP)]
        } else {
            let first = ((w - ui::PAGE_GAP) * self.ratio).round();
            [(x, y, first, h), (x + first + ui::PAGE_GAP, y, w - first - ui::PAGE_GAP, h)]
        }
    }
}

/// Ce que l'interface reçoit pour dessiner la vue partagée : la pastille
/// jumelée, le liseré du côté actif et la poignée entre les pages.
#[derive(Serialize)]
pub struct SplitInfo {
    pub first: TabId,
    pub second: TabId,
    pub ratio: f64,
    /// Disposition affichée (l'un sur l'autre, choisi ou faute de place).
    pub stacked: bool,
    /// Disposition choisie.
    pub chosen_stacked: bool,
    /// À l'écran (l'onglet actif en est un côté).
    pub shown: bool,
    pub rects: [Area; 2],
}

/// Tout ce qu'il faut pour créer la webview d'un nouvel onglet.
pub struct PageContext<'a> {
    pub window: &'a Window,
    pub surface: &'a Surface,
    pub proxy: &'a EventLoopProxy<UserEvent>,
    pub settings: &'a Arc<RwLock<Settings>>,
    pub library: &'a Arc<RwLock<Library>>,
    pub sites: &'a Arc<RwLock<SiteData>>,
    pub reader: &'a reader::Store,
    pub bounds: Rect,
}

#[derive(Default)]
pub struct TabManager {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: TabId,
    /// Onglets fermés, gardés quelques secondes le temps que leur page s'arrête vraiment.
    closing: Vec<(Instant, WebView)>,
    /// Adresses des derniers onglets fermés (les plus récents à la fin).
    closed: Vec<String>,
    /// Groupes d'onglets, dans l'ordre de création.
    groups: Vec<TabGroup>,
    next_group: u32,
    /// La vue partagée (une seule à la fois).
    split: Option<Split>,
    /// Zone des pages dans la fenêtre (voir `set_area`).
    area: Area,
}

impl TabManager {
    /// Ouvre un onglet, le rend actif et renvoie son identifiant.
    pub fn open(&mut self, url: &str, ctx: &PageContext, visible: bool, private: bool) -> wry::Result<TabId> {
        let id = self.add_asleep(url, "", private);
        self.wake(id, ctx, false)?;
        self.activate(id, visible);
        Ok(id)
    }

    /// Ajoute un onglet en veille (sans page), par exemple à la restauration de
    /// la session : seul l'onglet affiché sera chargé.
    pub fn add_asleep(&mut self, url: &str, title: &str, private: bool) -> TabId {
        self.next_id += 1;
        let id = self.next_id;
        let mut tab = Tab {
            id,
            title: title.to_string(),
            url: String::new(),
            loading: false,
            audible: false,
            quiet: false,
            engine: MediaState::default(),
            dirty: false,
            capturing: false,
            muted: false,
            icon: None,
            site_name: None,
            private,
            last_active: Instant::now(),
            heard: None,
            weight: Weight::default(),
            repos: Repos::Eveille,
            readable: false,
            reading: None,
            pinned: false,
            group: None,
            failed: None,
            identity_retry: false,
            ads: None,
            reader_asked: false,
            key: new_key(),
            host: Arc::default(),
            souvenir: None,
            scroll: None,
            scroll_due: None,
            webview: None,
        };
        tab.set_url(url.to_string());
        self.tabs.push(tab);
        id
    }

    /// Recrée la page d'un onglet en veille. `visible` : la montrer tout de suite.
    pub fn wake(&mut self, id: TabId, ctx: &PageContext, visible: bool) -> wry::Result<()> {
        // Un côté de la vue partagée n'a que sa part de la zone des pages.
        let place = match self.split {
            Some(split) if split.has(id) => {
                let [first, second] = split.rects(self.area);
                Some(if split.first == id { first } else { second })
            }
            _ => None,
        };
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return Ok(());
        };
        if tab.webview.is_some() {
            return Ok(());
        }
        // Avec un souvenir, la page ne charge rien elle-même : WebKit rend
        // l'historique, puis rouvre la page au même endroit.
        let souvenir = tab.souvenir.take().map(|(_, s)| s);
        let webview = build_page(id, &tab.url, &tab.key, ctx, tab.private, Arc::clone(&tab.host), souvenir.is_none())?;
        match souvenir {
            Some(s) => {
                if !souvenir::rendre(&webview, &s) {
                    let _ = webview.load_url(&tab.url);
                    tab.scroll_due = tab.scroll.take();
                }
            }
            // Sans souvenir (Windows, Linux, macOS 11), au moins la position.
            None => tab.scroll_due = tab.scroll.take(),
        }
        if let Some(area) = place {
            let _ = webview.set_bounds(ui::rect(area));
        }
        crate::layers::round_page(&webview);
        let host = tab.host();
        adblock::apply(&webview, &host, tab.private);
        tab.ads = Some(adblock::active_on(&host, tab.private));
        let _ = webview.set_visible(visible);
        tab.webview = Some(webview);
        tab.loading = true;
        tab.weight = Weight::default();
        tab.repos = Repos::Eveille;
        tab.failed = None;
        Ok(())
    }

    /// Met un onglet en veille : sa page est détruite, son adresse gardée.
    /// L'onglet actif, un onglet qui joue du son ou un onglet privé (qui
    /// perdrait sa session) ne sont pas mis en veille.
    pub fn sleep(&mut self, id: TabId) -> bool {
        // L'autre côté de la vue partagée affichée est à l'écran lui aussi.
        let shown = self.shown(id);
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        if shown || tab.audible || tab.private {
            return false;
        }
        let Some(webview) = tab.webview.take() else {
            return false;
        };
        // Avant `stop_page`, qui remplace la page par about:blank.
        tab.souvenir = remember(tab, &webview);
        softsleep::oublier(&webview);
        tab.loading = false;
        tab.weight = Weight::default();
        tab.repos = Repos::Eveille;
        stop_page(&webview);
        self.closing.push((Instant::now(), webview));
        true
    }

    /// Détache la page d'un onglet pour en recréer une neuve (voir `App::reload`) :
    /// l'ancienne, et son processus WebKit, sont détruits avec la mémoire
    /// qu'ils avaient accumulée. Contrairement à la veille, vaut aussi pour
    /// l'onglet actif et un onglet qui joue du son. L'appelant réveille ensuite
    /// l'onglet, qui garde son historique sous macOS 12 ou plus (voir `Souvenir`).
    /// Faux si l'onglet n'avait pas de page.
    pub fn detach(&mut self, id: TabId) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        let Some(webview) = tab.webview.take() else {
            return false;
        };
        tab.souvenir = remember(tab, &webview);
        // La position relevée quand l'onglet a été caché n'est plus la bonne.
        tab.scroll = None;
        tab.weight = Weight::default();
        tab.audible = false;
        // Pas de passage par about:blank : son titre vide remplacerait celui de la
        // page dans l'historique, puisque l'onglet est aussitôt réveillé.
        silence_page(&webview);
        self.closing.push((Instant::now(), webview));
        true
    }

    /// Met en veille tous les onglets cachés qui peuvent l'être (gestionnaire des onglets).
    pub fn sleep_hidden(&mut self) -> bool {
        let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        let mut slept = false;
        for id in ids {
            slept |= self.sleep(id);
        }
        slept
    }

    /// Ferme un onglet. Si c'était l'onglet actif, son voisin devient actif.
    pub fn close(&mut self, id: TabId, visible: bool) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        // Fermer un côté défait la vue partagée : l'autre côté prend toute la
        // place et, si c'était l'onglet affiché, reste à l'écran.
        let partner = self.split.and_then(|s| s.other(id));
        if partner.is_some() {
            self.split = None;
        }
        let mut closed = self.tabs.remove(index);
        let tabs = &self.tabs;
        self.groups.retain(|g| tabs.iter().any(|t| t.group == Some(g.id)));
        // On garde l'adresse pour « rouvrir l'onglet fermé », sauf pour un onglet privé.
        if !closed.private && !network::is_home(&closed.url) {
            self.closed.retain(|u| *u != closed.url);
            self.closed.push(closed.url.clone());
            if self.closed.len() > CLOSED_MEMORY {
                self.closed.remove(0);
            }
        }
        // On arrête le son avant de détruire la page, et on lui laisse quelques secondes
        // pour s'arrêter (voir `reap`) : détruite tout de suite, sa vidéo pouvait continuer.
        if let Some(webview) = closed.webview.take() {
            softsleep::oublier(&webview);
            stop_page(&webview);
            self.closing.push((Instant::now(), webview));
        }
        if self.active == Some(id) {
            self.active = None;
            let neighbour = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|t| t.id);
            if let Some(next) = partner.or(neighbour) {
                self.activate(next, visible);
            }
        }
        if partner.is_some() {
            self.place_all();
        }
    }

    /// Adresse du dernier onglet fermé, retirée de la liste.
    pub fn pop_closed(&mut self) -> Option<String> {
        self.closed.pop()
    }

    /// Cet onglet fermé (menu Historique du Mac) : retiré de la liste s'il y est.
    #[cfg(target_os = "macos")]
    pub fn take_closed(&mut self, url: &str) -> bool {
        let before = self.closed.len();
        self.closed.retain(|u| u != url);
        self.closed.len() != before
    }

    /// Les onglets fermés, le plus ancien d'abord.
    #[cfg(target_os = "macos")]
    pub fn closed(&self) -> &[String] {
        &self.closed
    }

    /// Oublie les souvenirs des onglets en veille depuis plus d'une heure, puis
    /// les plus anciens au-delà de `MAX_SOUVENIRS`. Appelé toutes les cinq
    /// secondes (voir `App::survey`) ; sans effet hors de macOS, qui seul en garde.
    pub fn forget_souvenirs(&mut self, now: Instant) {
        let held: Vec<(usize, Instant)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.souvenir.as_ref().map(|(at, _)| (i, *at)))
            .collect();
        for i in to_forget(held, now) {
            self.tabs[i].souvenir = None;
        }
    }

    /// Détruit les pages fermées depuis un moment.
    pub fn reap(&mut self) {
        self.closing
            .retain(|(since, _)| since.elapsed() < std::time::Duration::from_secs(3));
    }

    /// Rend un onglet actif. S'il est en veille, l'appelant le réveille ensuite
    /// (voir `App::wake_active`).
    pub fn activate(&mut self, id: TabId, visible: bool) {
        if !self.tabs.iter().any(|t| t.id == id) {
            return;
        }
        let now = Instant::now();
        let before = self.shown_ids();
        self.active = Some(id);
        // L'onglet choisi, et l'autre côté si c'est une vue partagée.
        let after = self.shown_ids();
        for tab in &mut self.tabs {
            let shown = after.contains(&tab.id);
            // L'onglet qu'on quitte compte comme vu à l'instant.
            if before.contains(&tab.id) || shown {
                tab.last_active = now;
            }
            if let Some(w) = &tab.webview {
                // L'onglet qui revient retrouve sa mémoire normale et se dégèle.
                if shown && !before.contains(&tab.id) {
                    softsleep::reveiller(w, tab.repos);
                    tab.repos = Repos::Eveille;
                }
                let _ = w.set_visible(visible && shown);
            }
        }
    }

    /// Onglet suivant (`step = 1`) ou précédent (`step = -1`), en boucle.
    pub fn cycle(&mut self, step: isize, visible: bool) {
        let Some(current) = self.active_index() else {
            return;
        };
        let len = self.tabs.len() as isize;
        let next = (current as isize + step).rem_euclid(len) as usize;
        let id = self.tabs[next].id;
        self.activate(id, visible);
    }

    /// Déplace un onglet à une nouvelle position (glisser-déposer dans la colonne).
    pub fn move_tab(&mut self, id: TabId, index: usize) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        let tab = self.tabs.remove(from);
        let index = index.min(self.tabs.len());
        self.tabs.insert(index, tab);
        self.arrange();
        // Les deux côtés d'une vue partagée restent l'un contre l'autre.
        if let Some(split) = self.split.filter(|s| s.has(id)) {
            if split.first == id {
                self.put_next_to(split.second, id, false);
            } else {
                self.put_next_to(split.first, id, true);
            }
        }
    }

    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.tabs.iter_mut()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Montre ou cache l'onglet actif, et l'autre côté de la vue partagée
    /// (cachés quand un panneau de l'interface est ouvert, sous Linux).
    pub fn set_active_visible(&self, visible: bool) {
        for tab in self.tabs.iter().filter(|t| self.shown(t.id)) {
            if let Some(w) = tab.webview() {
                let _ = w.set_visible(visible);
            }
        }
    }

    /// Nouvelle zone des pages (fenêtre, barre ou bandeau changés).
    pub fn set_area(&mut self, area: Area) {
        self.area = area;
        self.place_all();
    }

    /// Place de chaque page : sa part de la zone en vue partagée, toute la zone sinon.
    fn place_all(&self) {
        let split = self.split.map(|s| (s, s.rects(self.area)));
        for tab in &self.tabs {
            let Some(w) = tab.webview() else { continue };
            let area = match split {
                Some((s, [first, _])) if s.first == tab.id => first,
                Some((s, [_, second])) if s.second == tab.id => second,
                _ => self.area,
            };
            let _ = w.set_bounds(ui::rect(area));
            // Windows : la découpe arrondie suit la taille de la page.
            crate::layers::round_page(w);
        }
    }

    // --- Vue partagée ---

    pub fn area(&self) -> Area {
        self.area
    }

    /// La vue partagée, si elle est à l'écran (l'onglet actif en est un côté).
    pub fn split_shown(&self) -> Option<Split> {
        self.split.filter(|s| self.active.is_some_and(|a| s.has(a)))
    }

    /// L'onglet est à l'écran : actif, ou l'autre côté de la vue partagée affichée.
    pub fn shown(&self, id: TabId) -> bool {
        self.active == Some(id) || self.split_shown().is_some_and(|s| s.has(id))
    }

    /// Les onglets à l'écran : un, ou deux en vue partagée.
    pub fn shown_ids(&self) -> Vec<TabId> {
        match self.split_shown() {
            Some(s) => vec![s.first, s.second],
            None => self.active.into_iter().collect(),
        }
    }

    /// Affiche `other` à côté de `base`, en premier (à gauche, en haut) ou en
    /// second. Une vue partagée déjà là se défait : ses onglets restent
    /// ouverts. L'appelant rend ensuite `other` actif. Faux si l'un des deux
    /// onglets manque, ou si c'est le même.
    pub fn open_split(&mut self, base: TabId, other: TabId, other_first: bool, stacked: bool) -> bool {
        if base == other || self.get(other).is_none() {
            return false;
        }
        let Some((pinned, group)) = self.get(base).map(|t| (t.pinned, t.group)) else {
            return false;
        };
        // Même rang que l'autre côté (épinglé, groupe), pour que le rangement
        // des onglets les garde ensemble.
        if let Some(tab) = self.get_mut(other) {
            tab.pinned = pinned;
            tab.group = group;
        }
        let ratio = self.split.map_or(0.5, |s| s.ratio);
        let (first, second) = if other_first { (other, base) } else { (base, other) };
        self.split = Some(Split { first, second, ratio, stacked });
        self.arrange();
        self.put_next_to(other, base, other_first);
        self.place_all();
        true
    }

    /// Quitte la vue partagée : les deux onglets restent ouverts, seul l'onglet
    /// actif reste à l'écran.
    pub fn close_split(&mut self, visible: bool) {
        if self.split.take().is_none() {
            return;
        }
        self.place_all();
        if let Some(id) = self.active {
            self.activate(id, visible);
        }
    }

    /// Les deux côtés changent de place.
    pub fn swap_split(&mut self) {
        let Some(split) = self.split.as_mut() else { return };
        std::mem::swap(&mut split.first, &mut split.second);
        let (first, second) = (split.first, split.second);
        self.put_next_to(first, second, true);
        self.place_all();
    }

    /// Côte à côte ou l'un sur l'autre.
    pub fn turn_split(&mut self) {
        let width = self.area.2;
        let Some(split) = self.split.as_mut() else { return };
        // Passées d'elles-mêmes l'une sur l'autre (fenêtre étroite) : on les
        // garde ainsi, en le choisissant pour de bon.
        split.stacked = !split.stacked_in(width);
        self.place_all();
    }

    /// Part du premier côté (bornée), quand on tire la barre entre les pages.
    pub fn set_split_ratio(&mut self, ratio: f64) {
        let Some(split) = self.split.as_mut() else { return };
        if !ratio.is_finite() {
            return;
        }
        split.ratio = ratio.clamp(SPLIT_RATIO_MIN, SPLIT_RATIO_MAX);
        self.place_all();
    }

    /// Ce que l'interface dessine de la vue partagée.
    pub fn split_info(&self) -> Option<SplitInfo> {
        let s = self.split?;
        Some(SplitInfo {
            first: s.first,
            second: s.second,
            ratio: s.ratio,
            stacked: s.stacked_in(self.area.2),
            chosen_stacked: s.stacked,
            shown: self.split_shown().is_some(),
            rects: s.rects(self.area),
        })
    }

    /// Reprend la vue partagée d'une session (places dans l'ordre des onglets
    /// au moment de l'enregistrement : à appeler avant tout rangement).
    pub fn restore_split(&mut self, saved: SavedSplit, ids: &[TabId]) {
        let (Some(&first), Some(&second)) = (ids.get(saved.first), ids.get(saved.second)) else { return };
        if first == second || !saved.ratio.is_finite() {
            return;
        }
        let ratio = saved.ratio.clamp(SPLIT_RATIO_MIN, SPLIT_RATIO_MAX);
        self.split = Some(Split { first, second, ratio, stacked: saved.stacked });
    }

    /// Range l'onglet `id` juste avant (`before`) ou juste après `next_to`.
    fn put_next_to(&mut self, id: TabId, next_to: TabId, before: bool) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return };
        let tab = self.tabs.remove(from);
        let at = self.tabs.iter().position(|t| t.id == next_to).map_or(self.tabs.len(), |i| if before { i } else { i + 1 });
        self.tabs.insert(at, tab);
    }

    pub fn home_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter().filter(|t| network::is_home(&t.url))
    }

    /// Les onglets ouverts, pour les rouvrir au prochain démarrage (sans les onglets privés).
    pub fn session(&self) -> Session {
        let public: Vec<&Tab> = self.tabs.iter().filter(|t| !t.private).collect();
        let used: Vec<u32> = public.iter().filter_map(|t| t.group).collect();
        let place = |id: TabId| public.iter().position(|t| t.id == id);
        let split = self.split.and_then(|s| {
            Some(SavedSplit { first: place(s.first)?, second: place(s.second)?, ratio: s.ratio, stacked: s.stacked })
        });
        Session {
            split,
            urls: public.iter().map(|t| t.url.clone()).collect(),
            active: public.iter().position(|t| Some(t.id) == self.active).unwrap_or(0),
            pinned: public.iter().map(|t| t.pinned).collect(),
            groups: public.iter().map(|t| t.group).collect(),
            tab_groups: self.groups.iter().filter(|g| used.contains(&g.id)).cloned().collect(),
        }
    }

    // --- Épinglés et groupes ---

    pub fn groups(&self) -> &[TabGroup] {
        &self.groups
    }

    /// Reprend les groupes d'une session enregistrée (les numéros sont gardés).
    pub fn restore_groups(&mut self, groups: Vec<TabGroup>) {
        self.next_group = groups.iter().map(|g| g.id).max().unwrap_or(0);
        self.groups = groups;
        // Un onglet dont le groupe a disparu du fichier redevient libre.
        let known: Vec<u32> = self.groups.iter().map(|g| g.id).collect();
        for tab in &mut self.tabs {
            if tab.group.is_some_and(|g| !known.contains(&g)) {
                tab.group = None;
            }
        }
        self.arrange();
    }

    /// Épingle un onglet (ou le détache). Un onglet épinglé quitte son groupe.
    pub fn set_pinned(&mut self, id: TabId, pinned: bool) {
        // Les deux côtés d'une vue partagée restent ensemble.
        let partner = self.split.and_then(|s| s.other(id));
        for id in std::iter::once(id).chain(partner) {
            if let Some(tab) = self.get_mut(id) {
                tab.pinned = pinned;
                if pinned {
                    tab.group = None;
                }
            }
        }
        self.arrange();
    }

    /// Crée un groupe avec cet onglet ; renvoie son numéro.
    pub fn new_group(&mut self, id: TabId, name: &str) -> Option<u32> {
        self.get(id)?;
        self.next_group += 1;
        let group = self.next_group;
        let color = GROUP_COLORS[(group as usize - 1) % GROUP_COLORS.len()];
        self.groups.push(TabGroup { id: group, name: clean_name(name), color: color.to_string(), collapsed: false });
        self.set_group(id, Some(group));
        Some(group)
    }

    /// Range un onglet dans un groupe (`None` : le sort de son groupe).
    pub fn set_group(&mut self, id: TabId, group: Option<u32>) {
        if group.is_some_and(|g| !self.groups.iter().any(|x| x.id == g)) {
            return;
        }
        let partner = self.split.and_then(|s| s.other(id));
        for id in std::iter::once(id).chain(partner) {
            if let Some(tab) = self.get_mut(id) {
                tab.group = group;
                if group.is_some() {
                    tab.pinned = false;
                }
            }
        }
        self.arrange();
    }

    pub fn rename_group(&mut self, group: u32, name: &str) {
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == group) {
            g.name = clean_name(name);
        }
    }

    /// Défait un groupe : ses onglets restent ouverts, hors groupe.
    pub fn ungroup(&mut self, group: u32) {
        for tab in self.tabs.iter_mut().filter(|t| t.group == Some(group)) {
            tab.group = None;
        }
        self.arrange();
    }

    /// Les onglets d'un groupe (pour le fermer en entier).
    pub fn group_members(&self, group: u32) -> Vec<TabId> {
        self.tabs.iter().filter(|t| t.group == Some(group)).map(|t| t.id).collect()
    }

    /// Replie ou déplie un groupe. Replié, ses onglets cachés passent en veille :
    /// leur mémoire est rendue. Renvoie vrai si au moins un onglet a été endormi.
    pub fn toggle_group(&mut self, group: u32) -> bool {
        let Some(g) = self.groups.iter_mut().find(|g| g.id == group) else {
            return false;
        };
        g.collapsed = !g.collapsed;
        if !g.collapsed {
            return false;
        }
        let mut slept = false;
        for id in self.group_members(group) {
            slept |= self.sleep(id);
        }
        slept
    }

    /// Range les onglets : les épinglés d'abord, puis les onglets de chaque
    /// groupe côte à côte (à la place du premier d'entre eux). Les groupes
    /// vides disparaissent.
    fn arrange(&mut self) {
        let mut rest: Vec<Tab> = Vec::with_capacity(self.tabs.len());
        let mut pinned = Vec::new();
        for tab in self.tabs.drain(..) {
            if tab.pinned { pinned.push(tab) } else { rest.push(tab) }
        }
        let mut ordered = pinned;
        // Le premier onglet restant emmène avec lui tous ceux de son groupe.
        while !rest.is_empty() {
            match rest[0].group {
                Some(g) => {
                    let (members, others): (Vec<Tab>, Vec<Tab>) = rest.into_iter().partition(|t| t.group == Some(g));
                    ordered.extend(members);
                    rest = others;
                }
                None => ordered.push(rest.remove(0)),
            }
        }
        self.tabs = ordered;
        let tabs = &self.tabs;
        self.groups.retain(|g| tabs.iter().any(|t| t.group == Some(g.id)));
    }

    /// Active l'onglet à cette position (restauration de la session).
    pub fn activate_index(&mut self, index: usize, visible: bool) {
        if let Some(id) = self.tabs.get(index).map(|t| t.id) {
            self.activate(id, visible);
        }
    }

    pub fn snapshot(&self, is_bookmarked: impl Fn(&str) -> bool) -> Vec<TabInfo<'_>> {
        self.tabs
            .iter()
            .map(|t| {
                let home = network::is_home(&t.url);
                TabInfo {
                    id: t.id,
                    title: &t.title,
                    url: &t.url,
                    active: Some(t.id) == self.active,
                    loading: t.loading,
                    home,
                    audible: t.sounds(),
                    muted: t.muted,
                    icon: t.icon.as_deref(),
                    site_name: t.site_name.as_deref(),
                    bookmarked: is_bookmarked(&t.url),
                    private: t.private,
                    asleep: t.webview.is_none(),
                    secure: home || !t.url.starts_with("http://"),
                    weight: t.weight,
                    repos: t.repos,
                    error: t.failed.is_some(),
                    pinned: t.pinned,
                    group: t.group,
                    readable: t.readable,
                    reading: t.reading.is_some(),
                }
            })
            .collect()
    }

    fn active_index(&self) -> Option<usize> {
        self.active
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
    }
}

/// Nom d'un groupe : court, sans caractères de contrôle ; « Groupe » s'il est vide.
fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(30).collect();
    let name = name.trim();
    if name.is_empty() { "Groupe".to_string() } else { name.to_string() }
}

/// Clé de 128 bits tirée au hasard (voir `Tab::key`). `RandomState` est semé
/// par le système avec des octets aléatoires : sans ses graines, impossible de
/// deviner le résultat, et chaque appel donne une clé différente.
fn new_key() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    (0..2u8)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u8(i);
            h.write_u128(nanos);
            format!("{:016x}", h.finish())
        })
        .collect()
}

/// Souvenir d'une page qu'on va détruire, daté, sauf pour une page Feather
/// (erreur, lecture) : elle se recrée autrement (voir `App::load_failed`, `App::open_reader`).
fn remember(tab: &Tab, webview: &WebView) -> Option<(Instant, Souvenir)> {
    if tab.on_internal_page() {
        return None;
    }
    souvenir::garder(webview).map(|s| (Instant::now(), s))
}

/// Parmi les souvenirs gardés (place de l'onglet, heure du souvenir), ceux à
/// oublier : plus d'une heure, ou au-delà des `MAX_SOUVENIRS` plus récents.
fn to_forget(held: Vec<(usize, Instant)>, now: Instant) -> Vec<usize> {
    let (old, mut recent): (Vec<_>, Vec<_>) =
        held.into_iter().partition(|(_, at)| now.saturating_duration_since(*at) >= GARDE_SOUVENIR);
    let mut out: Vec<usize> = old.into_iter().map(|(i, _)| i).collect();
    if recent.len() > MAX_SOUVENIRS {
        recent.sort_unstable_by_key(|(_, at)| std::cmp::Reverse(*at));
        out.extend(recent[MAX_SOUVENIRS..].iter().map(|(i, _)| *i));
    }
    out
}

/// Position dans une page, en pixels CSS, et l'adresse où elle a été relevée.
#[derive(Clone, Debug, PartialEq)]
pub struct Scroll {
    pub x: f64,
    pub y: f64,
    pub url: String,
}

/// Historique Précédent/Suivant d'une page et sa position, tels que WebKit les
/// garde (`WKWebView.interactionState`, macOS 12 ou plus). Opaque : on le prend
/// à la page avant de la détruire, on le rend à la nouvelle ; gardé une heure
/// au plus, pour les `MAX_SOUVENIRS` onglets endormis le plus récemment (voir
/// `TabManager::forget_souvenirs`). Ailleurs, il n'y en a jamais : WebView2 ne
/// sait pas rendre un historique, et seule la position est gardée, par la page
/// elle-même (voir `Tab::scroll`).
#[cfg(target_os = "macos")]
pub struct Souvenir(objc2::rc::Retained<objc2::runtime::AnyObject>);

#[cfg(not(target_os = "macos"))]
pub enum Souvenir {}

#[cfg(target_os = "macos")]
mod souvenir {
    use wry::{WebView, WebViewExtMacOS};

    use super::Souvenir;

    pub fn garder(webview: &WebView) -> Option<Souvenir> {
        if !objc2::available!(macos = 12.0) {
            return None;
        }
        // SAFETY : fil principal, vue vivante ; propriété de macOS 12.
        unsafe { webview.webview().interactionState() }.map(Souvenir)
    }

    /// Rend l'historique à une page neuve, qui rouvre aussitôt la page
    /// courante. Faux si WebKit n'en a rien fait (l'appelant charge l'adresse).
    pub fn rendre(webview: &WebView, souvenir: &Souvenir) -> bool {
        let wk = webview.webview();
        // SAFETY : fil principal, vue vivante ; l'objet vient de `interactionState`
        // d'une autre vue, le seul type que WebKit attend ici.
        unsafe {
            wk.setInteractionState(Some(&souvenir.0));
            wk.backForwardList().currentItem().is_some()
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod souvenir {
    use wry::WebView;

    use super::Souvenir;

    pub fn garder(_webview: &WebView) -> Option<Souvenir> {
        None
    }

    pub fn rendre(_webview: &WebView, _souvenir: &Souvenir) -> bool {
        false
    }
}

/// Coupe le son et vide une page avant de la détruire.
fn stop_page(webview: &WebView) {
    silence_page(webview);
    let _ = webview.load_url("about:blank");
}

/// Cache une page qui va être détruite et coupe ses vidéos et musiques.
fn silence_page(webview: &WebView) {
    errorpage::forget(webview);
    let _ = webview.set_visible(false);
    let _ = webview.evaluate_script(
        "document.querySelectorAll('video, audio').forEach((m) => { m.pause(); m.removeAttribute('src'); m.load(); })",
    );
}

/// Vrai pour les sites de Google (google.fr, www.google.com, news.google.co.uk…).
/// Sur macOS, WKWebView s'annonce sans « Version/… Safari/… » : Google prend alors
/// Feather pour un vieux navigateur et sert une recherche dépouillée (sans l'IA).
/// L'identité Safari avait été mise partout le 19/10 puis retirée, car YouTube se
/// comportait autrement : on ne la prend donc que sur Google.
#[cfg(any(target_os = "macos", test))]
pub fn is_google(url: &str) -> bool {
    let Some(host) = network::host_of(url) else { return false };
    let labels: Vec<&str> = host.split('.').collect();
    // « google » suivi d'un domaine de premier niveau court : « fr », « com », « co.uk », « com.br ».
    labels.iter().position(|l| *l == "google").is_some_and(|i| {
        let tld = &labels[i + 1..];
        (1..=2).contains(&tld.len()) && tld.iter().all(|l| (2..=3).contains(&l.len()))
    })
}

/// Identité Safari, avec la version de Safari installée (même moteur que Feather).
#[cfg(target_os = "macos")]
fn safari_agent() -> &'static str {
    use std::sync::OnceLock;
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        let version = safari_version().unwrap_or_else(|| "18.0".into());
        format!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/{version} Safari/605.1.15"
        )
    })
}

/// Version de Safari installée (ex. « 18.5 »), lue dans son Info.plist.
#[cfg(target_os = "macos")]
fn safari_version() -> Option<String> {
    use objc2_foundation::{NSBundle, NSString, ns_string};
    let bundle = NSBundle::bundleWithPath(ns_string!("/Applications/Safari.app"))?;
    let value = bundle.objectForInfoDictionaryKey(ns_string!("CFBundleShortVersionString"))?;
    let version = value.downcast::<NSString>().ok()?.to_string();
    // Seulement « chiffres.chiffres(.chiffres) », pour ne rien glisser d'étrange dans l'en-tête.
    let parts: Vec<&str> = version.split('.').collect();
    (parts.len() <= 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
        .then_some(version)
}

/// macOS : les sites qui reçoivent l'identité Safari. Google (voir `is_google`), et
/// les sites déclarés par une extension active pour lui parler, comme la page de
/// connexion de Proton pour Proton Pass (voir `extensions::declared_site`).
#[cfg(target_os = "macos")]
fn wants_safari(url: &str) -> bool {
    is_google(url) || extensions::declared_site(url)
}

/// Identité de départ d'une page, selon sa première adresse. Windows (WebView2 = Edge)
/// et Linux (WebKitGTK) envoient déjà un identifiant complet : on le garde.
fn page_user_agent(url: &str) -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    if wants_safari(url) {
        return Some(safari_agent());
    }
    let _ = url;
    None
}

/// macOS, au début et à la fin de chaque chargement : un onglet qui arrive sur Google
/// (ou un autre site de `wants_safari`), ou qui en sort, change d'identité puis
/// recharge l'adresse (la première requête est déjà partie avec l'ancienne). La
/// fin compte aussi : une redirection (lien d'un résultat Google vers YouTube,
/// retour de la connexion Google vers YouTube) change de site sans nouveau début
/// de chargement, et YouTube gardait l'identité Safari.
/// Vrai si la page est rechargée.
#[cfg(target_os = "macos")]
pub fn match_identity(webview: &WebView, url: &str) -> bool {
    use objc2_foundation::NSString;
    use wry::WebViewExtMacOS;
    let wanted = wants_safari(url);
    let wk = webview.webview();
    // SAFETY : fil principal (boucle d'évènements), vue vivante.
    let current = unsafe { wk.customUserAgent() }.is_some_and(|ua| ua.length() > 0);
    if wanted == current {
        return false;
    }
    let agent = wanted.then(|| NSString::from_str(safari_agent()));
    // SAFETY : idem ; `nil` rend l'identité par défaut de WebKit.
    unsafe { wk.setCustomUserAgent(agent.as_deref()) };
    let _ = webview.load_url(url);
    true
}

#[cfg(not(target_os = "macos"))]
pub fn match_identity(_webview: &WebView, _url: &str) -> bool {
    false
}

fn build_page(
    id: TabId,
    url: &str,
    key: &str,
    ctx: &PageContext,
    private: bool,
    host: Arc<RwLock<String>>,
    load: bool,
) -> wry::Result<WebView> {
    let settings = Arc::clone(ctx.settings);
    let library = Arc::clone(ctx.library);
    let download_settings = Arc::clone(ctx.settings);
    let sites = Arc::clone(ctx.sites);
    let articles = Arc::clone(ctx.reader);
    let permission_host = Arc::clone(&host);
    let on_download = ctx.proxy.clone();
    let on_downloaded = ctx.proxy.clone();
    let on_ipc = ctx.proxy.clone();
    let on_title = ctx.proxy.clone();
    let on_load = ctx.proxy.clone();
    let on_new_window = ctx.proxy.clone();
    let (block_trackers, economy, index) = ctx
        .settings
        .read()
        .map(|s| (s.blocks_trackers(), s.economy, s.index_pages))
        .unwrap_or((true, false, true));
    // Onglet privé ou recherche dans le texte des pages coupée : la page n'a pas à
    // extraire son texte (`innerText` oblige le moteur à recalculer la mise en page).
    let no_index = private || !index;

    let builder = WebViewBuilder::new()
        .with_bounds(ctx.bounds)
        .with_devtools(cfg!(debug_assertions))
        // Onglet privé : stockage à part, effacé à la fermeture.
        .with_incognito(private)
        // macOS : un clic sur une page peu après l'avoir quittée ne doit pas servir qu'à la réactiver.
        .with_accept_first_mouse(true)
        .with_initialization_script(if std::env::var_os("FEATHER_DIAG").is_some() {
            ui::DIAG_SCRIPT
        } else {
            ""
        })
        // Mode économie au moment où la page est créée ; ensuite, Rust prévient la
        // page à chaque chargement et à chaque changement (voir `App::apply_economy`).
        .with_initialization_script(format!("window.__featherEconomy = {economy}; window.__featherNoIndex = {no_index};"))
        // FEATHER_SANS_SCRIPT=1 : pas de script injecté (pour chercher l'origine d'un défaut).
        .with_initialization_script(if std::env::var_os("FEATHER_SANS_SCRIPT").is_some() {
            String::new()
        } else {
            ui::page_script(key)
        })
        // La page d'accueil intégrée est générée à chaque visite avec les paramètres
        // actuels ; `navigateur://erreur/` sert les pages d'erreur (voir errorpage.rs).
        .with_custom_protocol("navigateur".into(), move |_id, request| {
            let settings = settings.read().unwrap_or_else(std::sync::PoisonError::into_inner);
            let uri = request.uri().to_string();
            let html = if request.uri().host() == Some("erreur") {
                errorpage::render(&uri, &settings)
            } else if request.uri().host() == Some("lecture") {
                // Seul l'article de cet onglet, et seulement pour l'adresse demandée.
                let store = articles.read().ok();
                let article = store.as_ref().and_then(|s| s.get(&id)).and_then(|(url, a)| {
                    (reader::parse(&uri).as_ref() == Some(&(id, url.clone()))).then_some(a)
                });
                reader::render(&uri, article, &settings)
            } else {
                match library.read() {
                    // Favoris et dernières pages se règlent chacun de leur côté ;
                    // un onglet privé ne montre jamais les dernières pages visitées.
                    Ok(lib) => {
                        let bookmarks = if settings.bookmarks_on_home { lib.bookmarks() } else { &[] };
                        let recent = if settings.recent_on_home && !private { lib.search_history("", 20) } else { Vec::new() };
                        ui::home_page(&settings, bookmarks, &recent)
                    }
                    Err(_) => ui::home_page(&settings, &[], &[]),
                }
            };
            // Ces pages (accueil avec l'historique, lecture, erreur) ne s'affichent
            // jamais dans le cadre d'un autre site.
            Response::builder()
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .header("Content-Security-Policy", "frame-ancestors 'none'")
                .header("X-Frame-Options", "DENY")
                .body(Cow::Owned(html.into_bytes()))
                .expect("réponse HTTP valide")
        })
        // Autorisations : la décision retenue pour ce site (la question est posée par
        // le script injecté avant que la page n'appelle le moteur).
        .with_permission_handler(move |kind| {
            let host = permission_host.read().map(|h| h.clone()).unwrap_or_default();
            sites
                .read()
                .map(|s| s.answer(&host, kind))
                .unwrap_or(wry::PermissionResponse::Default)
        })
        // Les pages envoient des raccourcis clavier (voir assets/page.js) ; l'accueil, sa barre de recherche.
        // L'adresse du cadre qui écrit part avec le message : seule la page elle-même
        // a tous les droits (voir `PageMessage::for_other_frame`).
        .with_ipc_handler(move |request| {
            let frame = request.uri().to_string();
            let _ = on_ipc.send_event(UserEvent::Page(id, frame, request.into_body()));
        })
        .with_document_title_changed_handler(move |title| {
            let _ = on_title.send_event(UserEvent::Title(id, title));
        })
        .with_on_page_load_handler(move |event, url| {
            let loading = matches!(event, PageLoadEvent::Started);
            let _ = on_load.send_event(UserEvent::PageLoad(id, loading, url));
        })
        // Téléchargements : rangés dans le dossier choisi, suivis dans la bibliothèque.
        .with_download_started_handler(move |url, path| {
            let dir = download_settings.read().map(|s| s.download_dir.clone()).unwrap_or_default();
            *path = downloads::destination(path, &dir);
            let _ = on_download.send_event(UserEvent::DownloadStarted { url, path: path.clone() });
            true
        })
        .with_download_completed_handler(move |url, path, success| {
            let _ = on_downloaded.send_event(UserEvent::DownloadDone { url, path, success });
        })
        // Les liens « nouvelle fenêtre » s'ouvrent dans un nouvel onglet (privé si la page l'est).
        .with_new_window_req_handler(move |url, _features| {
            let _ = on_new_window.send_event(UserEvent::OpenInNewTab { url, private });
            NewWindowResponse::Deny
        });

    // Sans `load`, la page reste vide : l'appelant lui rend son historique (voir `Souvenir`).
    let builder = if load { builder.with_url(url) } else { builder };
    // Identité Safari sur Google et les sites des extensions (voir `wants_safari`).
    let builder = match page_user_agent(url) {
        Some(agent) => builder.with_user_agent(agent),
        None => builder,
    };
    // Les extensions agissent sur les pages, sauf dans les onglets privés.
    let builder = extensions::prepare_builder(builder, !private, url);
    let webview = ctx.surface.attach(builder, ctx.window)?;
    trackers::install(&webview, host, block_trackers, private);
    errorpage::install(&webview, id, ctx.proxy.clone());
    Ok(webview)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_seulement() {
        for url in ["https://www.google.com/search?q=a", "https://google.fr/", "https://news.google.co.uk/", "https://www.google.com.br/"] {
            assert!(is_google(url), "{url}");
        }
        for url in ["https://www.youtube.com/", "https://google.exemple.org/", "https://googleusercontent.com/", "navigateur://accueil/", "pas une adresse"] {
            assert!(!is_google(url), "{url}");
        }
    }

    fn order(m: &TabManager) -> Vec<TabId> {
        m.tabs.iter().map(|t| t.id).collect()
    }

    #[test]
    fn souvenirs_bornes() {
        let now = Instant::now() + Duration::from_secs(7200);
        let ago = |secs| now - Duration::from_secs(secs);
        // Une heure passée : oublié, même s'il est seul.
        assert_eq!(to_forget(vec![(0, ago(10)), (1, ago(3600)), (2, ago(3599))], now), [1]);
        // Vingt-cinq souvenirs récents : les cinq plus anciens partent.
        let held: Vec<(usize, Instant)> = (0..25).map(|i| (i, ago(100 + i as u64))).collect();
        let mut out = to_forget(held, now);
        out.sort_unstable();
        assert_eq!(out, [20, 21, 22, 23, 24]);
        assert!(to_forget(Vec::new(), now).is_empty());
    }

    #[test]
    fn epingles_en_tete() {
        let mut m = TabManager::default();
        let a = m.add_asleep("https://a.fr/", "A", false);
        let b = m.add_asleep("https://b.fr/", "B", false);
        let c = m.add_asleep("https://c.fr/", "C", false);
        m.set_pinned(c, true);
        assert_eq!(order(&m), vec![c, a, b]);
        // Un onglet ordinaire ne passe pas devant un épinglé.
        m.move_tab(b, 0);
        assert_eq!(order(&m), vec![c, b, a]);
        m.set_pinned(c, false);
        assert_eq!(m.session().pinned, vec![false, false, false]);
    }

    #[test]
    fn vue_partagee_places() {
        let s = Split { first: 1, second: 2, ratio: 0.5, stacked: false };
        // 1000 px : deux pages de 496 px séparées par l'espace de 8 px.
        assert_eq!(s.rects((10.0, 20.0, 1000.0, 600.0)), [(10.0, 20.0, 496.0, 600.0), (514.0, 20.0, 496.0, 600.0)]);
        // Fenêtre étroite : moins de 280 px par page, elles passent l'une sur l'autre.
        assert!(!s.stacked_in(600.0));
        assert!(s.stacked_in(560.0));
        assert_eq!(s.rects((0.0, 0.0, 500.0, 408.0)), [(0.0, 0.0, 500.0, 200.0), (0.0, 208.0, 500.0, 200.0)]);
        let narrow = Split { ratio: 0.25, ..s };
        assert!(narrow.stacked_in(1000.0));
    }

    #[test]
    fn vue_partagee_deux_pages() {
        let mut m = TabManager::default();
        let a = m.add_asleep("https://a.fr/", "A", false);
        let b = m.add_asleep("https://b.fr/", "B", false);
        let c = m.add_asleep("https://c.fr/", "C", false);
        assert!(!m.open_split(a, a, false, false));
        // C s'affiche à gauche de A : il se range juste avant lui.
        assert!(m.open_split(a, c, true, false));
        assert_eq!(order(&m), vec![c, a, b]);
        let s = m.split.unwrap();
        assert_eq!((s.first, s.second), (c, a));
        // Une seule vue partagée : B remplace C, qui reste ouvert (deux pages au plus).
        m.set_split_ratio(0.9);
        assert!(m.open_split(a, b, false, false));
        let s = m.split.unwrap();
        assert_eq!((s.first, s.second, s.ratio), (a, b, SPLIT_RATIO_MAX));
        assert_eq!(order(&m), vec![c, a, b]);
        m.swap_split();
        assert_eq!(order(&m), vec![c, b, a]);
        // Enregistrée avec la session, reprise au démarrage.
        let saved = m.session().split.unwrap();
        assert_eq!((saved.first, saved.second), (1, 2));
        let mut n = TabManager::default();
        let ids: Vec<TabId> = ["c", "b", "a"].iter().map(|u| n.add_asleep(u, u, false)).collect();
        n.restore_split(saved, &ids);
        assert_eq!(n.split.map(|s| (s.first, s.second)), Some((ids[1], ids[2])));
        // Fermer un côté défait la vue partagée.
        m.close(b, false);
        assert!(m.split.is_none());
        assert_eq!(order(&m), vec![c, a]);
    }

    #[test]
    fn groupes_cote_a_cote() {
        let mut m = TabManager::default();
        let a = m.add_asleep("https://a.fr/", "A", false);
        let b = m.add_asleep("https://b.fr/", "B", false);
        let c = m.add_asleep("https://c.fr/", "C", false);
        let g = m.new_group(a, "  ").unwrap();
        assert_eq!(m.groups()[0].name, "Groupe");
        m.set_group(c, Some(g));
        assert_eq!(order(&m), vec![a, c, b]);
        // Épingler sort l'onglet de son groupe.
        m.set_pinned(c, true);
        assert_eq!(order(&m), vec![c, a, b]);
        assert_eq!(m.get(c).unwrap().group, None);
        // Groupe vidé : il disparaît.
        m.set_group(a, None);
        assert!(m.groups().is_empty());
        // Un groupe inconnu est refusé.
        m.set_group(b, Some(99));
        assert_eq!(m.get(b).unwrap().group, None);
    }

    #[test]
    fn session_et_restauration() {
        let mut m = TabManager::default();
        let a = m.add_asleep("https://a.fr/", "A", false);
        let b = m.add_asleep("https://b.fr/", "B", false);
        let g = m.new_group(b, "Travail").unwrap();
        m.rename_group(g, "Bureau");
        m.toggle_group(g);
        let s = m.session();
        assert_eq!(s.groups, vec![None, Some(g)]);
        assert_eq!(s.tab_groups.len(), 1);
        assert!(s.tab_groups[0].collapsed);
        assert_eq!(m.group_members(g), vec![b]);
        m.ungroup(g);
        assert!(m.groups().is_empty());
        assert_eq!(order(&m), vec![a, b]);
        // Restauration : un onglet dont le groupe manque redevient libre.
        let mut r = TabManager::default();
        let x = r.add_asleep("https://x.fr/", "X", false);
        r.get_mut(x).unwrap().group = Some(7);
        r.restore_groups(Vec::new());
        assert_eq!(r.get(x).unwrap().group, None);
        let n = r.new_group(x, "Neuf").unwrap();
        assert_eq!(n, 1);
    }
}
