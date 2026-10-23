//! Mode « banc de mesure » (outils/banc-memoire.sh et .ps1), éteint d'habitude.
//!
//! - `FEATHER_BANC=fichier` : Feather écrit dans ce fichier, une ligne par
//!   événement, les étapes du démarrage comptées depuis le début de `main`
//!   (`rust <ms>` : fichiers lus et premier onglet créé ; `interface <ms>` : la
//!   barre est prête ; `demarrage <ms>` : la première page a fini de charger).
//! - `FEATHER_BANC_EXTENSIONS=id,id` (avec `FEATHER_BANC`) : installe ces
//!   extensions du Chrome Web Store sans demander l'accord, l'une après
//!   l'autre, puis ouvre la page de leur bouton dans un onglet et y cherche un
//!   message d'erreur (Proton Pass écrivait « difficulties activating » quand
//!   son arrière-plan ne répondait pas). Lignes écrites :
//!   `extension <id> preparee|chargee`, `extension <id> erreur <message>`,
//!   `fenetre <id> ok|erreur|vide <début du texte>`, `extensions indisponibles <raison>`,
//!   et `moteur <id> <message>` pour chaque erreur notée par le moteur (macOS :
//!   arrière-plan refusé, erreur de script…).
//!
//! - `FEATHER_BANC_CONNEXION=adresse,adresse` (avec les extensions) : une fois
//!   la page de Proton Pass lue, ouvre l'une après l'autre ces pages de
//!   connexion, clique dans le champ de l'identifiant et regarde si Proton Pass
//!   y a posé son icône et son menu. Lignes écrites : `connexion <n> champ …`
//!   (le champ cliqué), `connexion <n> page {…}` (ce que Proton a ajouté à la
//!   page), puis `journal <id> …`, les lignes du journal de Proton Pass qui
//!   parlent des scripts de la page.
//!
//! - `FEATHER_BANC_ONGLETS=1` (avec `FEATHER_BANC`) : après le démarrage, ouvre
//!   l'un après l'autre un onglet d'accueil puis deux onglets Netflix, comme un
//!   clic sur un favori, et écrit pour chacun `onglet-<nom>-creation <ms>` (le
//!   programme bloqué pendant la création de la page), `onglet-<nom>-debut <ms>`
//!   (la page commence à s'afficher) et `onglet-<nom>-charge <ms>` (chargement
//!   fini), comptés depuis l'ordre d'ouverture ; `?` si rien après 30 s.
//!
//! Réservé aux machines de compilation : les extensions installées ainsi
//! restent dans le profil, comme une installation normale.

use std::collections::{HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::App;
use crate::extensions::{self, support};
use crate::tab_manager::TabId;

/// Attente entre le chargement d'une extension et l'ouverture de sa page
/// (son arrière-plan démarre), puis avant de lire cette page.
const AVANT_OUVERTURE: Duration = Duration::from_secs(20);
const AVANT_LECTURE: Duration = Duration::from_secs(10);

/// Mots d'un message d'erreur dans la page d'une extension (en minuscules).
/// Onglets ouverts par `FEATHER_BANC_ONGLETS` : nom dans le fichier, adresse.
/// Netflix deux fois : le premier onglet d'un site, puis le même site déjà en cache.
const ONGLETS: [(&str, &str); 3] = [
    ("accueil", crate::network::HOME_URL),
    ("netflix1", "https://www.netflix.com/fr/"),
    ("netflix2", "https://www.netflix.com/fr/"),
];

/// Pause avant chaque ouverture d'onglet, et attente maximale de son chargement.
const AVANT_ONGLET: Duration = Duration::from_secs(5);
const ONGLET_MAX: Duration = Duration::from_secs(30);

/// Onglet en cours de mesure : son nom, son numéro et l'instant de l'ordre d'ouverture.
struct Mesure {
    nom: &'static str,
    onglet: TabId,
    ordre: Instant,
    debut_note: bool,
}

/// Proton Pass, dont le menu dans les champs des sites est essayé.
const PROTON: &str = "ghmbeldphafepmbegfdlkpapadhbakde";

/// Attente après l'ouverture d'une page de connexion (chargement, analyse de la
/// page par l'extension), puis entre le clic dans le champ et la lecture.
const AVANT_CLIC: Duration = Duration::from_secs(15);
const APRES_CLIC: Duration = Duration::from_secs(5);

/// Clique dans le champ de l'identifiant (le premier trouvé) et le décrit.
const CLIC: &str = r#"(() => {
  const f = document.querySelector('input[type=email], input[autocomplete~=username], input[name=login], input[name=username], input[type=text]');
  if (!f) return 'aucun';
  f.focus();
  f.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
  f.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  f.click();
  return `${f.name || f.id || f.type} visible=${document.visibilityState} focus=${document.hasFocus()}`;
})()"#;

/// Ce que Proton Pass a ajouté à la page : ses balises (icône, menu), leurs
/// rôles, les attributs posés sur les formulaires, les cadres de l'extension.
const LECTURE: &str = r#"(() => {
  const tous = [...document.querySelectorAll('*')];
  const balises = tous.map((e) => e.tagName.toLowerCase()).filter((t) => t.startsWith('protonpass'));
  const roles = [...document.querySelectorAll('[data-protonpass-role]')].map((e) => e.getAttribute('data-protonpass-role'));
  const attributs = new Set();
  for (const e of tous) for (const a of e.getAttributeNames()) if (a.includes('protonpass')) attributs.add(a);
  const cadres = [...document.querySelectorAll('iframe')].filter((f) => (f.src || '').startsWith('webkit-extension')).length;
  return JSON.stringify({ balises, roles, attributs: [...attributs], cadres, visible: document.visibilityState });
})()"#;

/// Demande le journal de l'extension (depuis l'une de ses pages) ; relu ensuite
/// par `JOURNAL_LU`, le stockage ne répondant qu'après coup.
const JOURNAL: &str = r#"(() => {
  globalThis.featherJournal = 'attente';
  chrome.storage.local.get('logs').then((r) => {
    let lignes = [];
    try { lignes = JSON.parse(r.logs || '[]'); } catch (e) { lignes = [String(r.logs)]; }
    globalThis.featherJournal = `${lignes.length} lignes : ` + lignes
      .filter((l) => /content|inject|client|orchestr|frame|register|element|error|erreur/i.test(l))
      .slice(0, 40).join(' ¶ ');
  }, (e) => { globalThis.featherJournal = 'refus ' + e; });
  return 'demande';
})()"#;
const JOURNAL_LU: &str = "String(globalThis.featherJournal)";

/// Une action prévue de l'essai des pages de connexion.
enum Action {
    Ouvrir(String),
    /// Script dans le dernier onglet ouvert, ou dans un onglet donné ; son
    /// résultat est écrit sous ce nom (rien si le nom est vide).
    Executer { onglet: Option<TabId>, nom: String, script: &'static str },
}

const ERREURS: [&str; 6] = ["difficult", "went wrong", "try again", "réessay", "erreur", "failed"];

struct Etat {
    fichier: PathBuf,
    debut: Instant,
    demarrage_note: bool,
    /// Extensions encore à installer, et celle en cours de préparation.
    a_installer: VecDeque<String>,
    en_cours: Option<String>,
    /// Pages d'extension à ouvrir (identifiant, moment), puis à lire (onglet).
    a_ouvrir: Vec<(String, Instant)>,
    a_lire: Vec<(String, TabId, Instant)>,
    /// Erreurs du moteur déjà écrites (« id message »).
    erreurs_vues: HashSet<String>,
    /// Onglets encore à mesurer, moment du prochain, et celui en cours.
    onglets: VecDeque<(&'static str, &'static str)>,
    onglet_prochain: Option<Instant>,
    mesure: Option<Mesure>,
    /// Pages de connexion à essayer, les actions prévues et le dernier onglet ouvert.
    connexions: Vec<String>,
    actions: Vec<(Instant, Action)>,
    dernier: Option<TabId>,
}

static ETAT: OnceLock<Option<Mutex<Etat>>> = OnceLock::new();

/// À appeler tout au début de `main` : c'est le zéro du temps de démarrage.
pub fn init() {
    let debut = Instant::now();
    ETAT.get_or_init(|| {
        let fichier = PathBuf::from(std::env::var_os("FEATHER_BANC")?);
        let a_installer = std::env::var("FEATHER_BANC_EXTENSIONS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(String::from)
            .collect();
        let onglets = if std::env::var_os("FEATHER_BANC_ONGLETS").is_some() { ONGLETS.into() } else { VecDeque::new() };
        let connexions = std::env::var("FEATHER_BANC_CONNEXION")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(String::from)
            .collect();
        Some(Mutex::new(Etat {
            fichier,
            debut,
            demarrage_note: false,
            a_installer,
            en_cours: None,
            a_ouvrir: Vec::new(),
            a_lire: Vec::new(),
            erreurs_vues: HashSet::new(),
            onglets,
            onglet_prochain: None,
            mesure: None,
            connexions,
            actions: Vec::new(),
            dernier: None,
        }))
    });
}

fn avec<R>(f: impl FnOnce(&mut Etat) -> R) -> Option<R> {
    let etat = ETAT.get()?.as_ref()?;
    etat.lock().ok().map(|mut e| f(&mut e))
}

fn ecrire(fichier: &PathBuf, ligne: &str) {
    let ok = OpenOptions::new().create(true).append(true).open(fichier).and_then(|mut f| writeln!(f, "{ligne}"));
    if let Err(err) = ok {
        eprintln!("banc : écriture impossible : {err}");
    }
}

fn noter(ligne: &str) {
    avec(|e| ecrire(&e.fichier, ligne));
}

/// Texte sur une ligne, d'au plus `max` caractères.
fn sur_une_ligne(texte: &str, max: usize) -> String {
    texte.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

/// Texte sur une ligne, raccourci (pour le fichier du banc).
fn resume(texte: &str) -> String {
    sur_une_ligne(texte, 160)
}

/// Une étape du démarrage (`rust`, `interface`) : le temps écoulé depuis `init`.
pub fn etape(nom: &str) {
    avec(|e| ecrire(&e.fichier, &format!("{nom} {}", e.debut.elapsed().as_millis())));
}

/// Une page commence (`loading`) ou finit de charger : la première finie donne
/// le temps de démarrage ; celle de l'onglet mesuré, ses temps d'ouverture.
pub fn chargement(onglet: TabId, loading: bool) {
    avec(|e| {
        if !loading && !e.demarrage_note {
            e.demarrage_note = true;
            ecrire(&e.fichier, &format!("demarrage {}", e.debut.elapsed().as_millis()));
            if !e.onglets.is_empty() {
                e.onglet_prochain = Some(Instant::now() + AVANT_ONGLET);
            }
        }
        let Some(m) = e.mesure.as_mut().filter(|m| m.onglet == onglet) else { return };
        let ms = m.ordre.elapsed().as_millis();
        if loading {
            if !m.debut_note {
                m.debut_note = true;
                let ligne = format!("onglet-{}-debut {ms}", m.nom);
                ecrire(&e.fichier, &ligne);
            }
        } else if m.debut_note {
            let ligne = format!("onglet-{}-charge {ms}", m.nom);
            ecrire(&e.fichier, &ligne);
            e.mesure = None;
            if !e.onglets.is_empty() {
                e.onglet_prochain = Some(Instant::now() + AVANT_ONGLET);
            }
        }
    });
}

/// Réponse du moteur pour une extension : sa page sera ouverte un peu plus tard.
pub fn extension_chargee(id: &str, result: &Result<Option<String>, String>) {
    avec(|e| match result {
        Ok(_) => {
            ecrire(&e.fichier, &format!("extension {id} chargee"));
            if !e.a_ouvrir.iter().any(|(i, _)| i == id) {
                e.a_ouvrir.push((id.to_string(), Instant::now() + AVANT_OUVERTURE));
            }
        }
        Err(err) => ecrire(&e.fichier, &format!("extension {id} erreur {}", resume(err))),
    });
}

/// Erreurs notées par le moteur pour une extension : chacune écrite une fois.
pub fn erreurs_moteur(id: &str, erreurs: &[String]) {
    avec(|e| {
        for erreur in erreurs {
            // Plus long que les autres lignes : le fichier et la ligne fautive sont à la fin.
            let ligne = format!("moteur {id} {}", sur_une_ligne(erreur, 400));
            if e.erreurs_vues.insert(ligne.clone()) {
                ecrire(&e.fichier, &ligne);
            }
        }
    });
}

/// Analyse du texte de la page d'une extension (rendu par le moteur en JSON).
fn verdict(brut: &str) -> (&'static str, String) {
    // `null` : la page n'a pas de corps ; texte brut si le moteur ne l'a pas mis en JSON.
    let texte = serde_json::from_str::<Option<String>>(brut).map_or_else(|_| brut.to_string(), Option::unwrap_or_default);
    let texte = resume(&texte);
    let bas = texte.to_lowercase();
    let etat = if texte.is_empty() {
        "vide"
    } else if ERREURS.iter().any(|mot| bas.contains(mot)) {
        "erreur"
    } else {
        "ok"
    };
    (etat, texte)
}

/// Prévoit l'essai des pages de connexion, puis la lecture du journal de
/// Proton Pass dans `page` (sa page, déjà ouverte par le banc).
fn prevoir_connexions(e: &mut Etat, page: TabId) {
    if e.connexions.is_empty() {
        return;
    }
    let mut t = Instant::now() + Duration::from_secs(2);
    for (n, url) in e.connexions.iter().enumerate() {
        let n = n + 1;
        e.actions.push((t, Action::Ouvrir(url.clone())));
        t += AVANT_CLIC;
        e.actions.push((t, Action::Executer { onglet: None, nom: format!("connexion {n} champ"), script: CLIC }));
        t += APRES_CLIC;
        e.actions.push((t, Action::Executer { onglet: None, nom: format!("connexion {n} page"), script: LECTURE }));
        t += Duration::from_secs(2);
    }
    // Proton Pass écrit son journal toutes les 5 s.
    t += Duration::from_secs(5);
    e.actions.push((t, Action::Executer { onglet: Some(page), nom: String::new(), script: JOURNAL }));
    t += Duration::from_secs(3);
    e.actions.push((t, Action::Executer { onglet: Some(page), nom: format!("journal {PROTON}"), script: JOURNAL_LU }));
}

impl App {
    /// Chaque seconde, en mode banc : installe la prochaine extension demandée,
    /// ouvre et lit les pages d'extension dont c'est le moment.
    pub(crate) fn banc_tick(&mut self) {
        let Some(etat) = ETAT.get().and_then(Option::as_ref) else { return };
        let maintenant = Instant::now();
        let (suivante, a_ouvrir, a_lire, onglet, actions) = {
            let Ok(mut e) = etat.lock() else { return };
            if !e.a_installer.is_empty()
                && let Err(raison) = support()
            {
                e.a_installer.clear();
                ecrire(&e.fichier, &format!("extensions indisponibles {raison}"));
            }
            let libre = e.en_cours.is_none() && self.extensions.busy.is_none() && self.extensions.pending.is_none();
            let suivante = if libre { e.a_installer.pop_front() } else { None };
            if let Some(id) = &suivante {
                e.en_cours = Some(id.clone());
            }
            let (dues, autres): (Vec<_>, Vec<_>) = e.a_ouvrir.drain(..).partition(|(_, t)| *t <= maintenant);
            e.a_ouvrir = autres;
            let (lues, autres): (Vec<_>, Vec<_>) = e.a_lire.drain(..).partition(|(_, _, t)| *t <= maintenant);
            e.a_lire = autres;
            // Onglet mesuré resté sans fin de chargement : noté inconnu, on passe au suivant.
            if let Some(m) = e.mesure.take_if(|m| maintenant - m.ordre >= ONGLET_MAX) {
                let fin = if m.debut_note { "charge" } else { "debut" };
                ecrire(&e.fichier, &format!("onglet-{}-{fin} ?", m.nom));
                if !e.onglets.is_empty() {
                    e.onglet_prochain = Some(maintenant);
                }
            }
            let onglet = if e.mesure.is_none() && e.onglet_prochain.is_some_and(|t| t <= maintenant) {
                e.onglet_prochain = None;
                e.onglets.pop_front()
            } else {
                None
            };
            let (actions, autres): (Vec<_>, Vec<_>) = e.actions.drain(..).partition(|(t, _)| *t <= maintenant);
            e.actions = autres;
            (suivante, dues, lues, onglet, actions)
        };
        if let Some((nom, url)) = onglet {
            self.banc_onglet(nom, url);
        }
        if let Some(id) = suivante {
            self.install_extension(&id);
            // Refus immédiat (lien illisible…) : on passe à la suivante.
            if let Some(err) = self.extensions.error.take() {
                noter(&format!("extension {id} erreur {}", resume(&err)));
                avec(|e| e.en_cours = None);
            }
        }
        for (id, _) in a_ouvrir {
            self.banc_ouvrir(&id);
        }
        for (id, tab, _) in a_lire {
            let Some(webview) = self.tabs.get(tab).and_then(|t| t.webview()) else {
                noter(&format!("fenetre {id} vide onglet fermé"));
                continue;
            };
            let script = "document.body ? document.body.innerText : ''";
            let pour = id.clone();
            let lu = webview.evaluate_script_with_callback(script, move |brut| {
                let (etat, texte) = verdict(&brut);
                noter(&format!("fenetre {pour} {etat} {texte}"));
            });
            if let Err(err) = lu {
                noter(&format!("fenetre {id} vide {}", resume(&err.to_string())));
            }
            if id == PROTON {
                avec(|e| prevoir_connexions(e, tab));
            }
        }
        for (_, action) in actions {
            self.banc_action(action);
        }
    }

    /// Une action de l'essai des pages de connexion.
    fn banc_action(&mut self, action: Action) {
        match action {
            Action::Ouvrir(url) => {
                let avant: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
                self.open_tab(&url, false);
                let nouveau = self.tabs.iter().map(|t| t.id).find(|t| !avant.contains(t));
                avec(|e| e.dernier = nouveau);
            }
            Action::Executer { onglet, nom, script } => {
                let onglet = onglet.or_else(|| avec(|e| e.dernier).flatten());
                let Some(webview) = onglet.and_then(|t| self.tabs.get(t)).and_then(|t| t.webview()) else {
                    if !nom.is_empty() {
                        noter(&format!("{nom} onglet fermé"));
                    }
                    return;
                };
                let lu = webview.evaluate_script_with_callback(script, move |brut| {
                    if nom.is_empty() {
                        return;
                    }
                    let texte = serde_json::from_str::<String>(&brut).unwrap_or(brut);
                    noter(&format!("{nom} {}", sur_une_ligne(&texte, 4000)));
                });
                if let Err(err) = lu {
                    noter(&format!("script refusé {}", resume(&err.to_string())));
                }
            }
        }
    }

    /// Ouvre un onglet comme un clic sur un favori, et chronomètre sa création ;
    /// la suite (début et fin du chargement) est notée par `chargement`.
    fn banc_onglet(&mut self, nom: &'static str, url: &str) {
        let avant: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        let ordre = Instant::now();
        self.open_tab(url, false);
        let creation = ordre.elapsed().as_millis();
        noter(&format!("onglet-{nom}-creation {creation}"));
        match self.tabs.iter().map(|t| t.id).find(|t| !avant.contains(t)) {
            Some(onglet) => {
                avec(|e| e.mesure = Some(Mesure { nom, onglet, ordre, debut_note: false }));
            }
            None => {
                noter(&format!("onglet-{nom}-debut ?"));
                avec(|e| e.onglet_prochain = Some(Instant::now() + AVANT_ONGLET));
            }
        }
    }

    /// Ouvre la page du bouton (ou des options) de l'extension dans un onglet caché.
    fn banc_ouvrir(&mut self, id: &str) {
        let Some(ext) = self.extensions.get(id) else { return };
        let url = ext
            .popup
            .as_deref()
            .and_then(|page| extensions::page_url(ext, page))
            .or_else(|| extensions::options_url(ext));
        let Some(url) = url else {
            noter(&format!("fenetre {id} vide aucune page"));
            return;
        };
        let avant: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        self.open_tab_in_background(&url);
        if let Some(tab) = self.tabs.iter().map(|t| t.id).find(|t| !avant.contains(t)) {
            avec(|e| e.a_lire.push((id.to_string(), tab, Instant::now() + AVANT_LECTURE)));
        } else {
            noter(&format!("fenetre {id} vide onglet refusé"));
        }
    }

    /// Une extension demandée est téléchargée : elle est acceptée sans fiche.
    pub(crate) fn banc_extension_preparee(&mut self) {
        let Some(id) = avec(|e| e.en_cours.take()).flatten() else { return };
        if let Some(err) = self.extensions.error.clone() {
            noter(&format!("extension {id} erreur {}", resume(&err)));
        } else if self.extensions.pending.is_some() {
            noter(&format!("extension {id} preparee"));
            self.confirm_extension(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_repere_les_erreurs() {
        assert_eq!(verdict("\"Proton Pass is having difficulties activating\"").0, "erreur");
        assert_eq!(verdict("\"Se connecter\\n avec Proton\"").1, "Se connecter avec Proton");
        assert_eq!(verdict("\"Se connecter\"").0, "ok");
        assert_eq!(verdict("\"  \"").0, "vide");
        assert_eq!(verdict("null").0, "vide");
    }
}
