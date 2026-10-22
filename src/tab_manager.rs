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
use std::time::Instant;

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
use crate::library::{Library, Session, TabGroup};
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
    /// arriver, une seule fois (voir `PageMessage::from_untrusted`).
    pub reader_asked: bool,
    /// Clé secrète de l'onglet, connue du seul script de Feather dans la page :
    /// elle distingue ses messages de ceux du site (voir `ui::split_key`).
    key: String,
    /// Hôte de la page, partagé avec les rappels de la webview (autorisations, traqueurs).
    host: Arc<RwLock<String>>,
    /// Historique et position de la page détruite, rendus au réveil (macOS 12 ou plus).
    souvenir: Option<Souvenir>,
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
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return Ok(());
        };
        if tab.webview.is_some() {
            return Ok(());
        }
        // Avec un souvenir, la page ne charge rien elle-même : WebKit rend
        // l'historique, puis rouvre la page au même endroit.
        let souvenir = tab.souvenir.take();
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
        let active = self.active;
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        if active == Some(id) || tab.audible || tab.private {
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
            if let Some(next) = neighbour {
                self.activate(next, visible);
            }
        }
    }

    /// Adresse du dernier onglet fermé, retirée de la liste.
    pub fn pop_closed(&mut self) -> Option<String> {
        self.closed.pop()
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
        for tab in &mut self.tabs {
            // L'onglet qu'on quitte compte comme vu à l'instant.
            if Some(tab.id) == self.active || tab.id == id {
                tab.last_active = now;
            }
            if let Some(w) = &tab.webview {
                // L'onglet qui revient retrouve sa mémoire normale et se dégèle.
                if tab.id == id {
                    softsleep::reveiller(w, tab.repos);
                    tab.repos = Repos::Eveille;
                }
                let _ = w.set_visible(visible && tab.id == id);
            }
        }
        self.active = Some(id);
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

    /// Montre ou cache l'onglet actif (caché quand un panneau de l'interface est ouvert).
    pub fn set_active_visible(&self, visible: bool) {
        if let Some(w) = self.active().and_then(Tab::webview) {
            let _ = w.set_visible(visible);
        }
    }

    pub fn set_bounds(&self, bounds: Rect) {
        for w in self.tabs.iter().filter_map(Tab::webview) {
            let _ = w.set_bounds(bounds);
            // Windows : la découpe arrondie suit la taille de la page.
            crate::layers::round_page(w);
        }
    }

    pub fn home_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter().filter(|t| network::is_home(&t.url))
    }

    /// Les onglets ouverts, pour les rouvrir au prochain démarrage (sans les onglets privés).
    pub fn session(&self) -> Session {
        let public: Vec<&Tab> = self.tabs.iter().filter(|t| !t.private).collect();
        let used: Vec<u32> = public.iter().filter_map(|t| t.group).collect();
        Session {
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
        if let Some(tab) = self.get_mut(id) {
            tab.pinned = pinned;
            if pinned {
                tab.group = None;
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
        if let Some(tab) = self.get_mut(id) {
            tab.group = group;
            if group.is_some() {
                tab.pinned = false;
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

/// Souvenir d'une page qu'on va détruire, sauf pour une page Feather (erreur,
/// lecture) : elle se recrée autrement (voir `App::load_failed`, `App::open_reader`).
fn remember(tab: &Tab, webview: &WebView) -> Option<Souvenir> {
    if tab.on_internal_page() {
        return None;
    }
    souvenir::garder(webview)
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
/// à la page avant de la détruire, on le rend à la nouvelle. Ailleurs, il n'y
/// en a jamais : WebView2 ne sait pas rendre un historique, et seule la
/// position est gardée, par la page elle-même (voir `Tab::scroll`).
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
        // a tous les droits (voir `PageMessage::from_other_frame`).
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
            let _ = on_download.send_event(UserEvent::DownloadStarted { url, path: path.clone(), private });
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
