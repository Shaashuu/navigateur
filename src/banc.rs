//! Mode « banc de mesure » (outils/banc-memoire.sh et .ps1), éteint d'habitude.
//!
//! - `FEATHER_BANC=fichier` : Feather écrit dans ce fichier, une ligne par
//!   événement, le temps de démarrage (`interface <ms>` : la barre est prête ;
//!   `demarrage <ms>` : la première page a fini de charger), comptés depuis le
//!   début de `main`.
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
        Some(Mutex::new(Etat {
            fichier,
            debut,
            demarrage_note: false,
            a_installer,
            en_cours: None,
            a_ouvrir: Vec::new(),
            a_lire: Vec::new(),
            erreurs_vues: HashSet::new(),
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

/// La barre de Feather est prête.
pub fn interface_prete() {
    avec(|e| ecrire(&e.fichier, &format!("interface {}", e.debut.elapsed().as_millis())));
}

/// Une page a fini de charger : la première donne le temps de démarrage.
pub fn page_chargee() {
    avec(|e| {
        if !e.demarrage_note {
            e.demarrage_note = true;
            ecrire(&e.fichier, &format!("demarrage {}", e.debut.elapsed().as_millis()));
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

impl App {
    /// Chaque seconde, en mode banc : installe la prochaine extension demandée,
    /// ouvre et lit les pages d'extension dont c'est le moment.
    pub(crate) fn banc_tick(&mut self) {
        let Some(etat) = ETAT.get().and_then(Option::as_ref) else { return };
        let maintenant = Instant::now();
        let (suivante, a_ouvrir, a_lire) = {
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
            (suivante, dues, lues)
        };
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
