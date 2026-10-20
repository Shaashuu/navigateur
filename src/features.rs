//! Fonctionnalités ajoutées autour du cœur de `main.rs` : zoom par site,
//! recherche dans la page, suggestions, commandes natives, autorisations,
//! poids et mise en veille des onglets, suivi des téléchargements.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use crate::commands::{self, Contexte, Resultat};
use crate::downloads::{self, Download, State};
use crate::network;
use crate::sitedata::Permission;
use crate::softsleep;
use crate::tab_manager::TabId;
use crate::ui::{PageMetrics, PermissionRequest};
use crate::{App, UserEvent};

/// Niveaux de zoom proposés (Ctrl+plus / Ctrl+moins).
const ZOOMS: [f64; 13] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// Un onglet caché et lourd est mis en veille après ce délai, quel que soit le réglage.
const HEAVY_SLEEP: Duration = Duration::from_secs(5 * 60);

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
    }

    /// Commande native de la barre : aperçu pendant la frappe, ou exécution.
    pub(crate) fn run_command(&mut self, input: &str, run: bool) {
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
        if cfg!(not(target_os = "macos")) {
            tab.weight.memory_mb = metrics.heap_mb;
        }
        tab.weight.update_level();
    }

    /// Le texte d'une page chargée rejoint la recherche dans l'historique.
    pub(crate) fn index_page(&mut self, id: TabId, text: &str) {
        if !self.read_settings().index_pages {
            return;
        }
        let Some(tab) = self.tabs.get(id) else { return };
        if tab.private || network::is_home(tab.url()) {
            return;
        }
        let (url, title) = (tab.url().to_string(), tab.title.clone());
        self.fulltext.add(&url, &title, text);
        self.schedule_persist();
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
        if self.ticks.is_multiple_of(5) {
            changed |= self.sample_weights();
            self.soft_sleep();
            changed |= self.auto_sleep();
        }
        if changed {
            self.push_state();
        }
    }

    fn sample_weights(&mut self) -> bool {
        let mut changed = false;
        for tab in self.tabs.iter_mut() {
            let Some(w) = tab.webview() else { continue };
            let Some((memory, cpu)) = self.sampler.sample(w) else { continue };
            let before = tab.weight;
            tab.weight.memory_mb = Some(memory);
            tab.weight.cpu = cpu;
            tab.weight.update_level();
            changed |= tab.weight.level != before.level;
        }
        changed
    }

    /// Veille douce (Windows) : allège puis gèle les onglets cachés, sans
    /// détruire leur page (voir `softsleep.rs`). Un onglet qui joue du son n'est
    /// pas touché. Les onglets privés en profitent aussi : la vraie veille les
    /// épargne, puisqu'elle effacerait leur session.
    fn soft_sleep(&mut self) {
        let active = self.tabs.active_id();
        for tab in self.tabs.iter_mut().filter(|t| Some(t.id) != active && !t.audible) {
            let vise = softsleep::palier(tab.last_active.elapsed());
            let actuel = tab.repos;
            if let Some(w) = tab.webview() {
                let atteint = softsleep::reposer(w, actuel, vise);
                tab.repos = atteint;
            }
        }
    }

    /// Met en veille les onglets cachés depuis longtemps (réglage « veille »),
    /// et plus tôt ceux qui pèsent lourd.
    fn auto_sleep(&mut self) -> bool {
        let minutes = self.read_settings().sleep_minutes;
        let active = self.tabs.active_id();
        let idle = Duration::from_secs(u64::from(minutes) * 60);
        let candidates: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|t| Some(t.id) != active && !t.is_asleep() && !t.audible && !t.private)
            .filter(|t| {
                let hidden = t.last_active.elapsed();
                (minutes > 0 && hidden >= idle) || (t.weight.level >= 2 && hidden >= HEAVY_SLEEP)
            })
            .map(|t| t.id)
            .collect();
        let mut slept = false;
        for id in candidates {
            slept |= self.tabs.sleep(id);
        }
        if slept {
            self.schedule_reap();
        }
        slept
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
                d.path = path;
            }
            if d.state == State::Cancelled {
                // Le moteur a fini d'écrire : on peut enfin supprimer le fichier.
                downloads::remove(&d.path);
            } else {
                d.state = if success { State::Done } else { State::Failed };
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
