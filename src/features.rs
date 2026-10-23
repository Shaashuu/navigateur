//! Fonctionnalités ajoutées autour du cœur de `main.rs` : zoom par site,
//! recherche dans la page, suggestions, commandes natives, autorisations,
//! poids et mise en veille des onglets, mode économie, pages d'erreur, fiche
//! du site, suivi des téléchargements.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::commands::{self, Contexte, Resultat};
use crate::adblock;
use crate::capture::{self, Target};
use crate::downloads::{self, Download, State};
use crate::images;
use crate::import;
use crate::errorpage::{self, ErrorKind};
use crate::network;
use crate::reader::{self, Article};
use crate::siteclear;
use crate::sitedata::Permission;
use crate::softsleep;
use crate::suggest;
use crate::tab_manager::TabId;
use crate::ui::{CosmeticRequest, ImageHow, PageMetrics, PermissionRequest};
use crate::update;
use crate::{App, UpdateState, UserEvent};

/// Niveaux de zoom proposés (Ctrl+plus / Ctrl+moins).
const ZOOMS: [f64; 13] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// Un onglet caché et lourd est mis en veille après ce délai, quel que soit le réglage.
const HEAVY_SLEEP: Duration = Duration::from_secs(5 * 60);
/// « Actualiser » recrée à neuf une page qui pèse au moins autant (Mo).
const FRESH_RELOAD_MB: u32 = 300;

/// Au-delà de ce nombre de questions en attente, un onglet essuie un refus
/// (une page ne peut pas noyer l'utilisateur sous les demandes).
const MAX_WAITING: usize = 3;

/// Une demande d'autorisation qui attend la réponse de l'utilisateur.
pub struct PendingPermission {
    pub tab: TabId,
    /// Numéro donné par la page, à lui rendre avec la réponse.
    page_request: u32,
    host: String,
    permission: Permission,
}

/// Texte JSON sûr à insérer dans un script.
fn js(value: impl serde::Serialize) -> String {
    serde_json::to_string(&value).unwrap_or_else(|_| "null".into())
}

impl App {
    // --- Zoom -----------------------------------------------------------------

    /// Applique à un onglet le zoom retenu pour son site.
    pub(crate) fn apply_zoom(&self, id: TabId) {
        let Some(tab) = self.tabs.get(id) else { return };
        let Some(w) = tab.webview() else { return };
        let zoom = self.sites.read().map(|s| s.zoom(&tab.host())).unwrap_or(1.0);
        let _ = w.zoom(zoom);
    }

    /// `step` : +1 agrandit, -1 réduit, 0 revient à 100 %. Retenu pour le site.
    pub(crate) fn zoom(&mut self, step: i32) {
        let Some(tab) = self.tabs.active() else { return };
        if tab.webview().is_none() {
            return;
        }
        let host = tab.host();
        let current = self.sites.read().map(|s| s.zoom(&host)).unwrap_or(1.0);
        // Position du niveau le plus proche dans la liste.
        let index = ZOOMS
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - current).abs().total_cmp(&(b.1 - current).abs()))
            .map_or(5, |(i, _)| i);
        let zoom = match step {
            0 => 1.0,
            s if s > 0 => ZOOMS[(index + 1).min(ZOOMS.len() - 1)],
            _ => ZOOMS[index.saturating_sub(1)],
        };
        if let Ok(mut sites) = self.sites.write() {
            sites.set_zoom(&host, zoom);
        }
        let id = tab.id;
        self.apply_zoom(id);
        if let Some(tab) = self.tabs.get(id) {
            let text = format!("Zoom {} %", (zoom * 100.0).round());
            tab.eval(&format!("window.__featherToast && __featherToast({})", js(text)));
        }
        self.schedule_persist();
    }

    // --- Recherche dans la page -----------------------------------------------

    /// Cherche `text` dans la page active (texte vide : efface la recherche).
    pub(crate) fn find(&self, text: &str, backwards: bool) {
        if let Some(tab) = self.tabs.active() {
            tab.eval(&format!(
                "window.__featherFind && __featherFind({}, {backwards})",
                js(text)
            ));
        }
    }

    // --- Suggestions et commandes de la barre ---------------------------------

    pub(crate) fn send_suggestions(&self, query: &str) {
        let items = match self.library.read() {
            Ok(lib) => json!(lib.suggest(query, 6)),
            Err(_) => json!([]),
        };
        let data = json!({ "query": query, "items": items });
        self.chrome_call(&format!("navigateur.setSuggestions({data})"));
        // Les suggestions du moteur suivent, si le réglage le permet et jamais
        // depuis un onglet privé.
        let private = self.tabs.active().is_some_and(|t| t.private);
        let (on, engine) = {
            let s = self.settings();
            (s.search_suggestions, s.search_engine)
        };
        if on && !private && suggest::worth_asking(query) {
            suggest::request(&self.proxy, engine, query.trim());
        }
    }

    /// Commande native de la barre : aperçu pendant la frappe, ou exécution.
    pub(crate) fn run_command(&mut self, input: &str, run: bool) {
        // Commandes qui agissent sur la page : traitées ici, pas dans `commands`.
        if let Some(reponse) = self.page_command(input, run) {
            self.chrome_call(&format!("navigateur.setCommandResult({reponse})"));
            return;
        }
        let mut ctx = Contexte { notes: &mut self.notes };
        let reponse = if run {
            match commands::executer(input, &mut ctx) {
                None => return,
                Some(Err(texte)) => json!({ "texte": texte, "erreur": true, "run": true }),
                Some(Ok(Resultat::Texte(texte))) => json!({ "texte": texte, "erreur": false, "run": true }),
                Some(Ok(Resultat::Liste(lignes))) => {
                    let texte = if lignes.is_empty() { "Aucune note".to_string() } else { format!("{} note(s)", lignes.len()) };
                    json!({ "texte": texte, "lignes": lignes, "erreur": false, "run": true })
                }
                Some(Ok(Resultat::Minuteur { secondes, libelle })) => {
                    let proxy = self.proxy.clone();
                    let label = libelle.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(secondes));
                        let _ = proxy.send_event(UserEvent::TimerDone { seconds: secondes, label });
                    });
                    let texte = format!("Minuteur lancé : {}", commands::duree_lisible(secondes));
                    json!({ "texte": texte, "erreur": false, "run": true })
                }
            }
        } else {
            match commands::apercu(input, &ctx) {
                Some(r) => json!({ "texte": r.texte, "erreur": r.erreur, "lignes": r.lignes, "run": false }),
                None => json!(null),
            }
        };
        self.chrome_call(&format!("navigateur.setCommandResult({reponse})"));
        if run {
            self.schedule_persist();
        }
    }

    /// Fin d'un minuteur : un avis dans le bandeau du bas et dans la page affichée.
    pub(crate) fn timer_done(&self, seconds: u64, label: &str) {
        let text = if label.is_empty() {
            format!("Minuteur terminé ({})", commands::duree_lisible(seconds))
        } else {
            format!("Minuteur terminé : {label}")
        };
        self.chrome_call(&format!("navigateur.showNotice({})", js(&text)));
        if let Some(tab) = self.tabs.active() {
            tab.eval(&format!("window.__featherToast && __featherToast({})", js(&text)));
        }
    }

    // --- Autorisations par site -----------------------------------------------

    /// Une page demande une autorisation : décision retenue, sinon question à
    /// l'utilisateur (posée quand l'onglet est affiché, voir `permission_prompt`).
    pub(crate) fn ask_permission(&mut self, id: TabId, request: PermissionRequest) {
        let Some(tab) = self.tabs.get(id) else { return };
        let host = tab.host();
        let stored = self.sites.read().ok().and_then(|s| s.permission(&host, request.permission));
        let waiting = self.permission_requests.values().filter(|p| p.tab == id).count();
        // Pas de site (page locale) ou trop de questions en attente : refus sans demander.
        let decision = if host.is_empty() || waiting >= MAX_WAITING { Some(false) } else { stored };
        if let Some(allow) = decision {
            tab.eval(&format!(
                "window.__featherPermissionAnswer && __featherPermissionAnswer({}, {allow})",
                request.id
            ));
            return;
        }
        let number = self.next_permission_request;
        self.next_permission_request += 1;
        self.permission_requests.insert(
            number,
            PendingPermission { tab: id, page_request: request.id, host, permission: request.permission },
        );
        self.push_state();
    }

    /// La question à afficher pour l'onglet actif (la plus ancienne), s'il y en a une.
    pub(crate) fn permission_prompt(&self) -> serde_json::Value {
        let active = self.tabs.active_id();
        self.permission_requests
            .iter()
            .filter(|(_, p)| Some(p.tab) == active)
            .min_by_key(|(n, _)| **n)
            .map_or(json!(null), |(n, p)| {
                json!({ "request": n, "host": p.host, "label": p.permission.label() })
            })
    }

    /// Oublie les questions des onglets fermés ou partis sur un autre site.
    pub(crate) fn drop_stale_permissions(&mut self) {
        let tabs = &self.tabs;
        let mut stale = Vec::new();
        self.permission_requests.retain(|_, p| {
            let keep = tabs.get(p.tab).is_some_and(|t| t.host() == p.host && !t.is_asleep());
            if !keep {
                stale.push((p.tab, p.page_request));
            }
            keep
        });
        for (tab, request) in stale {
            if let Some(tab) = self.tabs.get(tab) {
                tab.eval(&format!(
                    "window.__featherPermissionAnswer && __featherPermissionAnswer({request}, false)"
                ));
            }
        }
    }

    pub(crate) fn answer_permission(&mut self, request: u32, allow: bool, remember: bool) {
        let Some(pending) = self.permission_requests.remove(&request) else { return };
        // Un onglet privé ne retient rien sur le disque.
        let private = self.tabs.get(pending.tab).is_some_and(|t| t.private);
        if let Ok(mut sites) = self.sites.write() {
            if remember && !private {
                sites.set_permission(&pending.host, pending.permission, allow);
            } else {
                sites.set_temporary(&pending.host, pending.permission, allow);
            }
        }
        if let Some(tab) = self.tabs.get(pending.tab)
            && tab.host() == pending.host
        {
            tab.eval(&format!(
                "window.__featherPermissionAnswer && __featherPermissionAnswer({}, {allow})",
                pending.page_request
            ));
        }
        self.schedule_persist();
    }

    // --- Poids et veille des onglets ------------------------------------------

    pub(crate) fn page_metrics(&mut self, id: TabId, metrics: PageMetrics) {
        let Some(tab) = self.tabs.get_mut(id) else { return };
        tab.weight.lag_ms = metrics.lag_ms;
        // La mémoire mesurée par le système (macOS) est plus juste que celle du JavaScript.
        let before = tab.weight.memory_mb;
        if cfg!(not(target_os = "macos")) {
            tab.weight.memory_mb = metrics.heap_mb;
        }
        tab.weight.update_level();
        // Panneau de la mémoire ouvert : le nouveau chiffre s'affiche tout de suite.
        if self.memory_watched && tab.weight.memory_mb != before {
            self.push_state();
        }
    }

    /// Le panneau « Utilisation de la mémoire » s'ouvre ou se ferme. Ouvert, la
    /// mémoire est relevée toutes les deux secondes (au lieu de cinq) et
    /// envoyée à chaque changement ; fermé, rien de plus que d'habitude.
    pub(crate) fn watch_memory(&mut self, on: bool) {
        self.memory_watched = on;
        if on && self.sample_weights() {
            self.push_state();
        }
    }

    /// Le texte d'une page chargée rejoint la recherche dans l'historique.
    pub(crate) fn index_page(&mut self, id: TabId, text: &str) {
        if !self.settings().index_pages {
            return;
        }
        let Some(tab) = self.tabs.get(id) else { return };
        if tab.private || network::is_home(tab.url()) || tab.on_internal_page() {
            return;
        }
        // Messageries, banques, santé et sites exclus : rien n'est gardé.
        if crate::fulltext::excluded(&tab.host(), &self.settings().index_exclude) {
            return;
        }
        let (url, title) = (tab.url().to_string(), tab.title.clone());
        // Enregistré avec le reste du texte, une fois par minute (voir `tick`).
        self.fulltext.add(&url, &title, text);
    }

    /// Chaque seconde : progression des téléchargements ; toutes les cinq
    /// secondes : poids des onglets et mise en veille des onglets oubliés.
    pub(crate) fn tick(&mut self) {
        self.ticks += 1;
        let mut changed = false;
        for d in self.downloads.iter_mut().filter(|d| d.state == State::Progress) {
            let before = d.received;
            d.probe();
            changed |= d.received != before;
        }
        changed |= self.probe_update();
        // Nouvelles versions des extensions : deux minutes après le lancement, puis
        // toutes les heures (la recherche n'a lieu qu'une fois par semaine).
        if self.ticks % 3600 == 120 {
            self.check_extension_updates();
        }
        if self.ticks.is_multiple_of(60)
            && let Err(err) = self.fulltext.save()
        {
            eprintln!("texte des pages non enregistré : {err}");
        }
        if self.ticks.is_multiple_of(5) {
            self.probe_media();
            // Un bloqueur en extension qui a fini de se préparer prend le relais.
            let before = self.extensions.ad_blocker().is_some();
            self.sync_ad_blocker();
            changed |= self.extensions.ad_blocker().is_some() != before;
            changed |= self.sample_weights();
            self.soft_sleep();
            changed |= self.auto_sleep();
        } else if self.memory_watched && self.ticks.is_multiple_of(2) {
            changed |= self.sample_weights();
        }
        if changed {
            self.push_state();
        }
    }

    /// Mise à jour en cours de téléchargement : la taille du fichier reçu.
    fn probe_update(&mut self) -> bool {
        let UpdateState::Downloading { received, speed, .. } = &mut self.update else { return false };
        let Some(size) = update::download_path().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()) else {
            return false;
        };
        if size == *received {
            return false;
        }
        *speed = size.saturating_sub(*received);
        *received = size;
        true
    }

    /// Relève la mémoire et le processeur de chaque page. Vrai si l'interface
    /// doit être mise à jour : un niveau de poids a changé, ou une mémoire
    /// pendant que le panneau de la mémoire est ouvert.
    fn sample_weights(&mut self) -> bool {
        let watched = self.memory_watched;
        let mut changed = false;
        for tab in self.tabs.iter_mut() {
            let Some(w) = tab.webview() else { continue };
            let Some((memory, cpu)) = self.sampler.sample(w) else { continue };
            let before = tab.weight;
            tab.weight.memory_mb = Some(memory);
            tab.weight.cpu = cpu;
            tab.weight.update_level();
            changed |= tab.weight.level != before.level || (watched && tab.weight.memory_mb != before.memory_mb);
        }
        changed
    }

    /// Veille douce (Windows, macOS 12 ou plus) : allège puis gèle les onglets
    /// cachés, sans détruire leur page (voir `softsleep.rs`). Un onglet qui joue
    /// du son n'est pas touché. Les onglets privés en profitent aussi : la vraie veille les
    /// épargne, puisqu'elle effacerait leur session.
    fn soft_sleep(&mut self) {
        let active = self.tabs.active_id();
        let downloading = self.downloading();
        let sites = std::sync::Arc::clone(&self.sites);
        let sites = sites.read().ok();
        for tab in self.tabs.iter_mut().filter(|t| Some(t.id) != active && !t.sounds()) {
            let mut vise = softsleep::palier(tab.last_active.elapsed());
            // Une page qui travaille peut au plus rendre de la mémoire (Windows),
            // jamais être gelée.
            if keep_awake(tab, sites.as_deref(), downloading) {
                vise = vise.min(softsleep::PLAFOND_OCCUPE);
            }
            let actuel = tab.repos;
            if let Some(w) = tab.webview() {
                let atteint = softsleep::reposer(w, actuel, vise);
                tab.repos = atteint;
            }
        }
    }

    /// Un téléchargement est en cours (détruire une page pourrait l'arrêter).
    fn downloading(&self) -> bool {
        self.downloads.iter().any(|d| d.state == State::Progress)
    }

    /// Demande au moteur le son et la caméra de chaque page (voir `media.rs`).
    fn probe_media(&self) {
        for tab in self.tabs.iter() {
            if let Some(w) = tab.webview() {
                crate::media::probe(w, tab.id, &self.proxy);
            }
        }
    }

    /// Met en veille les onglets cachés depuis longtemps (réglage « veille »),
    /// et plus tôt ceux qui pèsent lourd. Réglage « jamais » : aucun onglet,
    /// même lourd (avant, un onglet lourd dormait quand même après 5 minutes).
    fn auto_sleep(&mut self) -> bool {
        // Le mode économie raccourcit le délai (voir `Settings::sleep_after`).
        let minutes = self.settings().sleep_after();
        if minutes == 0 {
            return false;
        }
        let active = self.tabs.active_id();
        let idle = Duration::from_secs(u64::from(minutes) * 60);
        let downloading = self.downloading();
        let sites = self.sites.read().ok();
        let candidates: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|t| Some(t.id) != active && !t.is_asleep() && !t.private)
            .filter(|t| !keep_awake(t, sites.as_deref(), downloading))
            .filter(|t| {
                let hidden = t.last_active.elapsed();
                hidden >= idle || (t.weight.level >= 2 && hidden >= HEAVY_SLEEP)
            })
            .map(|t| t.id)
            .collect();
        drop(sites);
        let mut slept = false;
        for id in candidates {
            slept |= self.tabs.sleep(id);
        }
        if slept {
            self.schedule_reap();
        }
        slept
    }

    // --- Mode économie ---------------------------------------------------------

    /// Prévient la page du mode économie (pause des vidéos cachées, lecture
    /// automatique bloquée : voir assets/page.js). Appelé à chaque chargement,
    /// car le script injecté garde la valeur du moment où la page a été créée.
    pub(crate) fn apply_economy(&self, id: TabId) {
        let economy = self.settings().economy;
        if let Some(tab) = self.tabs.get(id) {
            tab.eval(&format!("window.__featherSetEconomy && __featherSetEconomy({economy})"));
        }
    }

    // --- Pages d'erreur -------------------------------------------------------

    /// Un chargement a échoué : l'onglet affiche la page d'erreur Feather.
    pub(crate) fn load_failed(&mut self, id: TabId, url: String, kind: ErrorKind) {
        if !network::is_web(&url) {
            return;
        }
        let Some(tab) = self.tabs.get_mut(id) else { return };
        // Sous Windows, la fin du chargement a déjà ajouté la page à l'historique.
        if !tab.private
            && let Ok(mut lib) = self.library.write()
        {
            lib.forget_failed_visit(&url);
        }
        let page = errorpage::error_url(kind, &url);
        tab.failed = Some((kind, url));
        let Some(w) = tab.webview() else { return };
        // Déjà sur une page d'erreur (« Réessayer » a encore échoué) : on la
        // remplace, pour ne pas empiler les erreurs dans l'historique.
        if w.url().is_ok_and(|u| errorpage::is_error_page(&u)) {
            tab.eval(&format!("location.replace({})", js(&page)));
        } else {
            let _ = w.load_url(&page);
        }
    }

    /// Page précédente. Sous Windows, une page en échec garde sa place dans
    /// l'historique, juste avant la page d'erreur : on revient deux pages en
    /// arrière, sinon on retomberait sur l'échec (et sur une nouvelle erreur).
    pub(crate) fn go_back(&self) {
        let Some(tab) = self.tabs.active() else { return };
        if cfg!(windows) && tab.failed.is_some() {
            tab.eval("history.go(-2)");
        } else if let Some(w) = tab.webview() {
            let _ = w.go_back();
        }
    }

    /// Actualiser. Sur une page d'erreur, on retente l'adresse en échec à sa place.
    ///
    /// Une page lourde est recréée à neuf au lieu d'être rechargée : WebKit
    /// recharge dans le même processus, qui garde la mémoire de la page
    /// précédente (YouTube passait de 1 à 1,6 Go en quelques rechargements,
    /// alors que la veille la rendait). Sous macOS 12 ou plus, la nouvelle page
    /// reprend l'historique Précédent/Suivant et la position de l'ancienne (voir
    /// `tab_manager::Souvenir`) ; ailleurs, l'historique est perdu. Pas pour un
    /// onglet privé, qui perdrait sa session.
    pub(crate) fn reload(&mut self) {
        let Some(tab) = self.tabs.active() else { return };
        if let Some((_, url)) = &tab.failed {
            tab.eval(&format!("location.replace({})", js(url)));
        } else if !tab.private && tab.weight.memory_mb.is_some_and(|mb| mb >= FRESH_RELOAD_MB) {
            let id = tab.id;
            if self.tabs.detach(id) {
                self.schedule_reap();
                self.wake_active();
            }
        } else if let Some(w) = tab.webview() {
            let _ = w.reload();
        }
    }

    // --- Mode lecture ---------------------------------------------------------

    /// Bouton livre : extrait l'article de la page, ou quitte la lecture.
    pub(crate) fn toggle_reader(&mut self) {
        let Some(id) = self.tabs.active_id() else { return };
        let Some(tab) = self.tabs.get_mut(id) else { return };
        if tab.reading.is_some() {
            tab.eval("history.back()");
        } else if network::is_web(tab.url()) && !tab.on_internal_page() {
            tab.reader_asked = true;
            tab.eval(&reader::extraction_script());
        }
    }

    /// L'article extrait arrive : il est gardé pour l'onglet et affiché dans la
    /// page de lecture (la page d'origine est quittée, sa mémoire rendue).
    pub(crate) fn open_reader(&mut self, id: TabId, article: Article) {
        let Some(article) = reader::clean(article) else { return };
        let Some(tab) = self.tabs.get(id) else { return };
        let original = tab.url().to_string();
        if !network::is_web(&original) || tab.on_internal_page() {
            return;
        }
        if let Ok(mut store) = self.reader.write() {
            store.insert(id, (original.clone(), article));
        }
        if let Some(w) = tab.webview() {
            let _ = w.load_url(&reader::reader_url(id, &original));
        }
    }

    // --- Commandes de page ----------------------------------------------------

    /// `/capture`, `/capture copier` et `/lecture`. `None` : pas une de ces commandes.
    fn page_command(&mut self, input: &str, run: bool) -> Option<serde_json::Value> {
        let mut words = input.trim().trim_start_matches('/').split_whitespace();
        let name = words.next()?.to_lowercase();
        let arg = words.next().map(str::to_lowercase);
        let texte = match (name.as_str(), arg.as_deref()) {
            ("capture", None) => {
                if run {
                    self.capture(false);
                    "Capture en cours…".to_string()
                } else {
                    match self.capture_name() {
                        Some(n) => format!("Enregistrer la page visible : {n}"),
                        None => "Rien à capturer sur cette page".to_string(),
                    }
                }
            }
            ("capture", Some("copier" | "copie")) => {
                if run {
                    self.capture(true);
                    "Capture en cours…".to_string()
                } else {
                    "Copier la page visible dans le presse-papiers".to_string()
                }
            }
            ("lecture", None) => {
                if run {
                    self.toggle_reader();
                }
                "Afficher l'article en mode lecture".to_string()
            }
            _ => return None,
        };
        Some(json!({ "texte": texte, "erreur": false, "run": run }))
    }

    // --- Capture de la page ---------------------------------------------------

    /// Nom du fichier de capture de l'onglet actif (`None` : pas de page à capturer).
    fn capture_name(&self) -> Option<String> {
        let tab = self.tabs.active()?;
        tab.webview()?;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let host = if network::is_web(tab.url()) { tab.host() } else { "feather".to_string() };
        Some(capture::file_name(&host, secs))
    }

    /// Ctrl+Maj+S ou `/capture` : image de la partie visible de la page.
    pub(crate) fn capture(&mut self, copy: bool) {
        if !capture::SUPPORTED {
            self.chrome_call(&format!("navigateur.showNotice({})", js("La capture n'est pas encore possible sur ce système")));
            return;
        }
        let Some(name) = self.capture_name() else { return };
        let Some(w) = self.tabs.active().and_then(|t| t.webview()) else { return };
        let target = if copy {
            Target::Clipboard
        } else {
            let dir = self.settings().download_dir.clone();
            Target::File(downloads::destination(std::path::Path::new(&name), &dir))
        };
        capture::capture(w, target, self.proxy.clone());
    }

    /// La capture est faite : elle rejoint la liste des téléchargements.
    pub(crate) fn captured(&mut self, result: Result<Option<PathBuf>, String>) {
        let text = match result {
            Ok(Some(path)) => {
                let id = self.next_download_id;
                self.next_download_id += 1;
                let mut d = Download::new(id, String::new(), path);
                d.state = State::Done;
                d.probe();
                d.speed = None;
                let text = format!("Capture enregistrée : {}", d.name);
                self.downloads.push(d);
                self.push_state();
                text
            }
            Ok(None) => "Capture copiée dans le presse-papiers".to_string(),
            Err(e) => e,
        };
        self.chrome_call(&format!("navigateur.showNotice({})", js(&text)));
    }

    // --- Onglets épinglés -----------------------------------------------------

    /// Ctrl+W ou le bouton de fermeture : un onglet épinglé reste ouvert (on le
    /// ferme par son menu).
    pub(crate) fn close_unpinned(&mut self, id: TabId) {
        if self.tabs.get(id).is_some_and(|t| t.pinned) {
            self.chrome_call(&format!("navigateur.showNotice({})", js("Onglet épinglé : clic droit → Fermer")));
        } else {
            self.close_tab(id);
        }
    }

    // --- Import des favoris ---------------------------------------------------

    /// Les navigateurs installés dont on peut reprendre les favoris.
    pub(crate) fn send_import_sources(&self) {
        let sources = import::sources();
        self.chrome_call(&format!("navigateur.setImportSources({})", js(&sources)));
    }

    /// Importe les favoris des navigateurs choisis et des fichiers HTML lus par l'interface.
    pub(crate) fn import(&mut self, sources: &[String], files: &[String]) {
        let mut items = Vec::new();
        for id in sources {
            items.extend(import::from_source(id));
        }
        for text in files.iter().filter(|t| t.len() <= 20 * 1024 * 1024) {
            items.extend(import::parse_html(text));
        }
        let added = self.library.write().map(|mut lib| lib.import_bookmarks(&items)).unwrap_or(0);
        let text = match added {
            0 if items.is_empty() => "Aucun favori trouvé".to_string(),
            0 => "Ces favoris sont déjà dans Feather".to_string(),
            1 => "1 favori importé".to_string(),
            n => format!("{n} favoris importés"),
        };
        self.chrome_call(&format!("navigateur.showNotice({})", js(&text)));
        if added > 0 {
            self.reload_home_tabs();
            self.push_state();
            self.schedule_persist();
        }
    }

    // --- Bloqueur de publicités ----------------------------------------------

    /// Listes lues (ou seul leur état a changé).
    pub(crate) fn ad_filters(&mut self, filters: Option<Box<adblock::Filters>>, status: adblock::Status) {
        match filters {
            Some(filters) => {
                adblock::install(*filters, self.proxy.clone());
                self.adblock = status;
            }
            None => {
                self.adblock.updating = status.updating;
                self.adblock.error = status.error;
            }
        }
        self.push_state();
    }

    /// Pose de nouveau le bloqueur sur toutes les pages (réglage changé, listes neuves).
    pub(crate) fn reapply_ads(&mut self) {
        for tab in self.tabs.iter_mut() {
            let host = tab.host();
            if let Some(w) = tab.webview() {
                adblock::apply(w, &host, tab.private);
                tab.ads = Some(adblock::active_on(&host, tab.private));
            }
        }
    }

    /// Fiche du site : bloqueur coupé ou rétabli sur ce site, puis les pages du site rechargées.
    pub(crate) fn set_site_ads(&mut self, host: &str, on: bool) {
        if let Ok(mut sites) = self.sites.write() {
            sites.set_ads(host, on);
            adblock::set_sites_off(sites.ads_off());
        }
        self.reapply_ads();
        for tab in self.tabs.iter().filter(|t| t.host() == host) {
            if let Some(w) = tab.webview() {
                let _ = w.reload();
            }
        }
        self.schedule_persist();
        self.push_state();
    }

    /// La page envoie ses `#id` et `.classes` : on répond par les encarts à masquer.
    pub(crate) fn hide_ads(&self, id: TabId, request: CosmeticRequest) {
        let Some(tab) = self.tabs.get(id) else { return };
        // Une page très chargée n'envoie que ses premiers éléments.
        let tokens = &request.tokens[..request.tokens.len().min(20_000)];
        let answer = adblock::hide(&tab.host(), tab.private, tokens, request.first);
        tab.eval(&format!("window.__featherHide && __featherHide({})", json!(answer)));
    }

    // --- Fiche du site --------------------------------------------------------

    /// Ce que la fiche du site montre pour l'onglet actif (les autorisations
    /// viennent de la liste générale `permissions`).
    pub(crate) fn site_info(&self) -> serde_json::Value {
        let Some(tab) = self.tabs.active() else { return json!(null) };
        if !network::is_web(tab.url()) {
            return json!(null);
        }
        let host = tab.host();
        let zoom = self.sites.read().map(|s| s.zoom(&host)).unwrap_or(1.0);
        json!({
            "host": host,
            "zoom": (zoom * 100.0).round(),
            "trackers": self.settings().blocks_trackers(),
            // Bloqueur de publicités : réglage général, et choix pour ce site.
            "ads": self.settings().blocks_ads(),
            "ads_site": self.sites.read().is_ok_and(|s| !s.ads_off().contains(&host)),
            // Une extension bloque les publicités à la place du bloqueur intégré
            // (sauf dans un onglet privé, qui n'a pas d'extensions).
            "ads_by": if tab.private { None } else { self.extensions.ad_blocker() },
            "awake": self.sites.read().is_ok_and(|s| s.is_awake(&host)),
            "error": tab.failed.is_some(),
        })
    }

    /// Efface les cookies et le stockage du site de l'onglet actif, puis recharge la page.
    pub(crate) fn clear_site_data(&self) {
        let Some(tab) = self.tabs.active() else { return };
        let Some(w) = tab.webview() else { return };
        let Ok(url) = url::Url::parse(tab.url()) else { return };
        if !network::is_web(tab.url()) {
            return;
        }
        let host = url.host_str().unwrap_or_default().to_string();
        siteclear::clear(w, &url.origin().ascii_serialization(), &host);
        let text = format!("Données de {} effacées", host.strip_prefix("www.").unwrap_or(&host));
        self.chrome_call(&format!("navigateur.showNotice({})", js(&text)));
    }

    // --- Téléchargements ------------------------------------------------------

    pub(crate) fn download_started(&mut self, url: String, path: PathBuf, private: bool) {
        let id = self.next_download_id;
        self.next_download_id += 1;
        // La taille est demandée au serveur à part ; pas pour un onglet privé,
        // qui ne doit pas refaire de requête en dehors de sa session.
        if !private {
            let proxy = self.proxy.clone();
            let probe_url = url.clone();
            std::thread::spawn(move || {
                if let Some(total) = downloads::content_length(&probe_url) {
                    let _ = proxy.send_event(UserEvent::DownloadSize { id, total });
                }
            });
        }
        self.downloads.push(Download::new(id, url, path));
        self.push_state();
    }

    /// Menu d'une image : « Enregistrer l'image » passe par les téléchargements
    /// (avec sa progression) ; « Enregistrer dans Photos » (macOS) l'ajoute
    /// directement à la photothèque.
    pub(crate) fn save_image(&mut self, tab: TabId, url: String, how: ImageHow) {
        let (referer, private) = self.tabs.get(tab).map(|t| (t.url().to_string(), t.private)).unwrap_or_default();
        let proxy = self.proxy.clone();
        if how == ImageHow::Photos {
            #[cfg(target_os = "macos")]
            std::thread::spawn(move || {
                let _ = proxy.send_event(UserEvent::ImageSaved(images::save_to_photos(&url, &referer)));
            });
            return;
        }
        let dir = self.settings().download_dir.clone();
        let dest = downloads::destination(Path::new(&images::file_name(&url)), &dir);
        // Fichier vide réservé tout de suite : deux images du même nom ne se marchent pas dessus.
        let _ = std::fs::File::create(&dest);
        self.download_started(url.clone(), dest.clone(), private);
        std::thread::spawn(move || {
            let result = images::fetch(&url, &referer, &dest).and_then(|()| images::checked_name(&dest));
            let success = result.is_ok();
            let path = result.unwrap_or(dest);
            let _ = proxy.send_event(UserEvent::DownloadDone { url, path: Some(path), success });
        });
    }

    pub(crate) fn image_saved(&mut self, result: Result<(), String>) {
        let text = match result {
            Ok(()) => "Image ajoutée à Photos".to_string(),
            Err(e) => format!("Impossible d'enregistrer l'image : {e}"),
        };
        self.chrome_call(&format!("navigateur.showNotice({})", js(&text)));
    }

    pub(crate) fn download_done(&mut self, url: &str, path: Option<PathBuf>, success: bool) {
        // Deux téléchargements de la même adresse : on reconnaît d'abord le bon
        // par son chemin, et seulement à défaut par son adresse.
        let waiting = |d: &Download| matches!(d.state, State::Progress | State::Cancelled);
        let by_path = path
            .as_ref()
            .and_then(|p| self.downloads.iter().position(|d| waiting(d) && &d.path == p));
        let index = by_path.or_else(|| self.downloads.iter().rposition(|d| waiting(d) && d.url == url));
        if let Some(d) = index.map(|i| &mut self.downloads[i]) {
            if let Some(path) = path {
                // Une image sans extension la reçoit à la fin : le nom affiché suit.
                if path != d.path {
                    d.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| d.name.clone());
                }
                d.path = path;
            }
            if d.state == State::Cancelled {
                // Le moteur a fini d'écrire : on peut enfin supprimer le fichier.
                downloads::remove(&d.path);
            } else {
                d.state = if success { State::Done } else { State::Failed };
                if success {
                    downloads::quarantine(&d.path);
                }
                d.probe();
                d.speed = None;
            }
        }
        self.push_state();
    }

    /// Annule un téléchargement. Le moteur ne permet pas d'arrêter le transfert :
    /// le fichier est supprimé maintenant, puis de nouveau quand il se termine.
    pub(crate) fn cancel_download(&mut self, id: u32) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.id == id && d.state == State::Progress) {
            d.state = State::Cancelled;
            d.speed = None;
            downloads::remove(&d.path);
        }
    }

    // --- Historique -----------------------------------------------------------

    /// Historique pour la bibliothèque : visites dont le titre ou l'adresse
    /// correspond, puis pages dont le texte contient la recherche (avec extrait).
    pub(crate) fn history_items(&self, query: &str) -> serde_json::Value {
        let Ok(lib) = self.library.read() else { return json!([]) };
        let visits = lib.search_history(query, 300);
        let mut items: Vec<serde_json::Value> = visits.iter().map(|v| json!(v)).collect();
        if query.trim().chars().count() >= 3 {
            let known: std::collections::HashSet<&str> = visits.iter().map(|v| v.url.as_str()).collect();
            for hit in self.fulltext.search(query, 50) {
                if !known.contains(hit.url.as_str()) {
                    items.push(json!({
                        "url": hit.url,
                        "title": hit.title,
                        "time": hit.time,
                        "snippet": hit.snippet,
                    }));
                }
            }
        }
        json!(items)
    }
}

/// Un onglet qui ne doit pas être détruit par la veille (ni gelé) : il est
/// épinglé, il travaille (son, caméra, formulaire en cours, voir `Tab::busy`),
/// son site est « toujours actif » ou peut envoyer des notifications (messagerie,
/// agenda…), ou un téléchargement est en cours.
fn keep_awake(tab: &crate::tab_manager::Tab, sites: Option<&crate::sitedata::SiteData>, downloading: bool) -> bool {
    if tab.pinned || tab.busy() || downloading {
        return true;
    }
    let host = tab.host();
    sites.is_some_and(|s| s.is_awake(&host) || s.permission(&host, Permission::Notifications) == Some(true))
}
