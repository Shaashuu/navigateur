//! Commandes natives de la barre « / » : calcul, conversion d'unités,
//! minuteur et notes. Elles s'exécutent en Rust, sans charger de page ni
//! appeler de service en ligne.
//!
//! Chaque commande implémente le trait `Commande`. Pour en ajouter une, il
//! suffit de l'écrire et de l'ajouter à `COMMANDES`.

use serde::{Deserialize, Serialize};

use crate::library::now;
use crate::storage;

/// Une commande de la barre, par exemple `/calcul 12*7`.
pub trait Commande: Sync {
    /// Nom tapé après « / ».
    fn nom(&self) -> &'static str;
    /// Phrase affichée dans la liste des commandes.
    fn description(&self) -> &'static str;
    /// Ce que la commande ferait, montré pendant la frappe (sans rien modifier).
    fn apercu(&self, args: &str, ctx: &Contexte) -> Result<String, String>;
    /// Exécute la commande (touche Entrée).
    fn executer(&self, args: &str, ctx: &mut Contexte) -> Result<Resultat, String>;
}

/// Ce dont les commandes ont besoin : pour l'instant, les notes.
pub struct Contexte<'a> {
    pub notes: &'a mut Notes,
}

/// Résultat d'une commande exécutée.
#[derive(Debug, PartialEq)]
pub enum Resultat {
    /// Un texte à afficher dans la barre (calcul, conversion, note enregistrée).
    Texte(String),
    /// Une liste de lignes (les notes).
    Liste(Vec<String>),
    /// Un minuteur à lancer : durée en secondes et texte à afficher à la fin.
    Minuteur { secondes: u64, libelle: String },
}

/// Ce que l'interface reçoit pour une commande (aperçu ou résultat).
#[derive(Debug, Serialize)]
pub struct Reponse {
    pub nom: String,
    pub texte: String,
    pub erreur: bool,
    pub lignes: Vec<String>,
}

static COMMANDES: [&dyn Commande; 5] = [&Calcul, &Convertir, &Minuteur, &AjouterNote, &ListerNotes];

/// Nom et description de chaque commande native, pour la liste de l'interface.
pub fn liste() -> Vec<(&'static str, &'static str)> {
    COMMANDES.iter().map(|c| (c.nom(), c.description())).collect()
}

/// Sépare « /calcul 12*7 » en commande et arguments. `None` si ce n'est pas
/// une commande native.
fn trouver(saisie: &str) -> Option<(&'static dyn Commande, &str)> {
    let saisie = saisie.trim().strip_prefix('/')?;
    let (nom, args) = saisie.split_once(char::is_whitespace).unwrap_or((saisie, ""));
    let nom = nom.to_lowercase();
    COMMANDES
        .iter()
        .find(|c| c.nom() == nom)
        .map(|c| (*c, args.trim()))
}

/// Aperçu pendant la frappe. `None` si la saisie n'est pas une commande native.
pub fn apercu(saisie: &str, ctx: &Contexte) -> Option<Reponse> {
    let (commande, args) = trouver(saisie)?;
    let (texte, erreur) = match commande.apercu(args, ctx) {
        Ok(t) => (t, false),
        Err(e) => (e, true),
    };
    Some(Reponse { nom: commande.nom().to_string(), texte, erreur, lignes: Vec::new() })
}

/// Exécute la saisie. `None` si ce n'est pas une commande native.
pub fn executer(saisie: &str, ctx: &mut Contexte) -> Option<Result<Resultat, String>> {
    let (commande, args) = trouver(saisie)?;
    Some(commande.executer(args, ctx))
}

// --- Calcul -----------------------------------------------------------------

struct Calcul;

impl Commande for Calcul {
    fn nom(&self) -> &'static str {
        "calcul"
    }
    fn description(&self) -> &'static str {
        "Calculer, par exemple /calcul (12+3)*7"
    }
    fn apercu(&self, args: &str, _ctx: &Contexte) -> Result<String, String> {
        if args.is_empty() {
            return Err("Tapez une opération : + − × ÷ ^ % et des parenthèses".into());
        }
        calculer(args).map(|v| format!("= {}", nombre(v)))
    }
    fn executer(&self, args: &str, ctx: &mut Contexte) -> Result<Resultat, String> {
        self.apercu(args, ctx).map(Resultat::Texte)
    }
}

/// Évalue une expression : + - * / ^ %, parenthèses, `pi`, `racine(…)`.
/// La virgule est acceptée comme séparateur décimal.
pub fn calculer(texte: &str) -> Result<f64, String> {
    let mut lecteur = Lecteur { car: texte.chars().filter(|c| !c.is_whitespace()).collect(), pos: 0 };
    let valeur = lecteur.somme()?;
    if lecteur.pos < lecteur.car.len() {
        return Err(format!("« {} » n'est pas compris", lecteur.car[lecteur.pos..].iter().collect::<String>()));
    }
    if !valeur.is_finite() {
        return Err("Résultat impossible (division par zéro ?)".into());
    }
    Ok(valeur)
}

/// Analyse descendante récursive, une fonction par niveau de priorité.
struct Lecteur {
    car: Vec<char>,
    pos: usize,
}

impl Lecteur {
    fn voir(&self) -> Option<char> {
        self.car.get(self.pos).copied()
    }

    fn somme(&mut self) -> Result<f64, String> {
        let mut v = self.produit()?;
        while let Some(c) = self.voir() {
            match c {
                '+' => {
                    self.pos += 1;
                    v += self.produit()?;
                }
                '-' | '−' => {
                    self.pos += 1;
                    v -= self.produit()?;
                }
                _ => break,
            }
        }
        Ok(v)
    }

    fn produit(&mut self) -> Result<f64, String> {
        let mut v = self.puissance()?;
        while let Some(c) = self.voir() {
            match c {
                '*' | '×' | 'x' => {
                    self.pos += 1;
                    v *= self.puissance()?;
                }
                '/' | '÷' | ':' => {
                    self.pos += 1;
                    let d = self.puissance()?;
                    if d == 0.0 {
                        return Err("Division par zéro".into());
                    }
                    v /= d;
                }
                '%' => {
                    self.pos += 1;
                    // « 20% » seul : pourcentage ; « 7 % 3 » : reste de la division.
                    match self.voir() {
                        Some(n) if n.is_ascii_digit() || n == '(' => {
                            let d = self.puissance()?;
                            if d == 0.0 {
                                return Err("Division par zéro".into());
                            }
                            v %= d;
                        }
                        _ => v /= 100.0,
                    }
                }
                _ => break,
            }
        }
        Ok(v)
    }

    fn puissance(&mut self) -> Result<f64, String> {
        let base = self.unaire()?;
        if self.voir() == Some('^') {
            self.pos += 1;
            // Associative à droite : 2^3^2 = 2^9.
            let exp = self.puissance()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn unaire(&mut self) -> Result<f64, String> {
        match self.voir() {
            Some('-' | '−') => {
                self.pos += 1;
                Ok(-self.unaire()?)
            }
            Some('+') => {
                self.pos += 1;
                self.unaire()
            }
            _ => self.primaire(),
        }
    }

    fn primaire(&mut self) -> Result<f64, String> {
        match self.voir() {
            Some('(') => {
                self.pos += 1;
                let v = self.somme()?;
                if self.voir() != Some(')') {
                    return Err("Parenthèse fermante manquante".into());
                }
                self.pos += 1;
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() || c == '.' || c == ',' => {
                let debut = self.pos;
                while self.voir().is_some_and(|c| c.is_ascii_digit() || c == '.' || c == ',') {
                    self.pos += 1;
                }
                let texte: String = self.car[debut..self.pos].iter().collect();
                texte
                    .replace(',', ".")
                    .parse::<f64>()
                    .map_err(|_| format!("Nombre incompris : {texte}"))
            }
            Some(c) if c.is_alphabetic() => {
                let debut = self.pos;
                while self.voir().is_some_and(char::is_alphabetic) {
                    self.pos += 1;
                }
                let mot: String = self.car[debut..self.pos].iter().collect::<String>().to_lowercase();
                match mot.as_str() {
                    "pi" | "π" => Ok(std::f64::consts::PI),
                    "racine" | "sqrt" => {
                        let v = self.primaire()?;
                        if v < 0.0 {
                            return Err("Racine d'un nombre négatif".into());
                        }
                        Ok(v.sqrt())
                    }
                    _ => Err(format!("« {mot} » n'est pas compris")),
                }
            }
            Some(c) => Err(format!("« {c} » n'est pas compris")),
            None => Err("Opération incomplète".into()),
        }
    }
}

/// Écrit un nombre à la française : virgule décimale, au plus 10 décimales.
pub fn nombre(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v }; // pas de « -0 »
    let mut texte = format!("{:.10}", v);
    if texte.contains('.') {
        texte = texte.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    texte.replace('.', ",")
}

// --- Conversion d'unités ----------------------------------------------------

struct Convertir;

/// Une unité : ses noms, sa famille et son facteur vers l'unité de base.
struct Unite {
    noms: &'static [&'static str],
    famille: &'static str,
    facteur: f64,
}

const UNITES: &[Unite] = &[
    // Longueurs (base : mètre)
    Unite { noms: &["m", "mètre", "metre", "mètres", "metres"], famille: "longueur", facteur: 1.0 },
    Unite { noms: &["km", "kilomètre", "kilometre", "kilomètres"], famille: "longueur", facteur: 1000.0 },
    Unite { noms: &["cm", "centimètre", "centimètres"], famille: "longueur", facteur: 0.01 },
    Unite { noms: &["mm", "millimètre", "millimètres"], famille: "longueur", facteur: 0.001 },
    Unite { noms: &["mi", "mile", "miles"], famille: "longueur", facteur: 1609.344 },
    Unite { noms: &["yd", "yard", "yards"], famille: "longueur", facteur: 0.9144 },
    Unite { noms: &["ft", "pied", "pieds", "feet"], famille: "longueur", facteur: 0.3048 },
    Unite { noms: &["in", "pouce", "pouces", "inch"], famille: "longueur", facteur: 0.0254 },
    // Masses (base : gramme)
    Unite { noms: &["g", "gramme", "grammes"], famille: "masse", facteur: 1.0 },
    Unite { noms: &["kg", "kilo", "kilos", "kilogramme", "kilogrammes"], famille: "masse", facteur: 1000.0 },
    Unite { noms: &["mg", "milligramme", "milligrammes"], famille: "masse", facteur: 0.001 },
    Unite { noms: &["t", "tonne", "tonnes"], famille: "masse", facteur: 1_000_000.0 },
    Unite { noms: &["lb", "lbs", "livre", "livres"], famille: "masse", facteur: 453.592_37 },
    Unite { noms: &["oz", "once", "onces"], famille: "masse", facteur: 28.349_523_125 },
    // Volumes (base : litre)
    Unite { noms: &["l", "litre", "litres"], famille: "volume", facteur: 1.0 },
    Unite { noms: &["ml", "millilitre", "millilitres"], famille: "volume", facteur: 0.001 },
    Unite { noms: &["cl", "centilitre", "centilitres"], famille: "volume", facteur: 0.01 },
    Unite { noms: &["dl", "décilitre", "décilitres"], famille: "volume", facteur: 0.1 },
    Unite { noms: &["gal", "gallon", "gallons"], famille: "volume", facteur: 3.785_411_784 },
    Unite { noms: &["tasse", "tasses", "cup", "cups"], famille: "volume", facteur: 0.25 },
    // Vitesses (base : mètre par seconde)
    Unite { noms: &["km/h", "kmh"], famille: "vitesse", facteur: 1.0 / 3.6 },
    Unite { noms: &["m/s", "ms"], famille: "vitesse", facteur: 1.0 },
    Unite { noms: &["mph"], famille: "vitesse", facteur: 0.447_04 },
    Unite { noms: &["noeud", "noeuds", "nœud", "nœuds", "kn"], famille: "vitesse", facteur: 0.514_444 },
    // Durées (base : seconde)
    Unite { noms: &["s", "seconde", "secondes"], famille: "durée", facteur: 1.0 },
    Unite { noms: &["min", "minute", "minutes"], famille: "durée", facteur: 60.0 },
    Unite { noms: &["h", "heure", "heures"], famille: "durée", facteur: 3600.0 },
    Unite { noms: &["j", "jour", "jours"], famille: "durée", facteur: 86_400.0 },
    Unite { noms: &["sem", "semaine", "semaines"], famille: "durée", facteur: 604_800.0 },
    // Données (base : octet, préfixes en puissances de 1000)
    Unite { noms: &["o", "octet", "octets"], famille: "données", facteur: 1.0 },
    Unite { noms: &["ko", "kb"], famille: "données", facteur: 1e3 },
    Unite { noms: &["mo", "mb"], famille: "données", facteur: 1e6 },
    Unite { noms: &["go", "gb"], famille: "données", facteur: 1e9 },
    Unite { noms: &["to", "tb"], famille: "données", facteur: 1e12 },
];

/// Les températures ne se convertissent pas par un simple facteur.
fn temperature(nom: &str) -> Option<&'static str> {
    match nom {
        "c" | "°c" | "celsius" => Some("°C"),
        "f" | "°f" | "fahrenheit" => Some("°F"),
        "k" | "kelvin" => Some("K"),
        _ => None,
    }
}

fn vers_kelvin(v: f64, unite: &str) -> f64 {
    match unite {
        "°C" => v + 273.15,
        "°F" => (v - 32.0) * 5.0 / 9.0 + 273.15,
        _ => v,
    }
}

fn depuis_kelvin(k: f64, unite: &str) -> f64 {
    match unite {
        "°C" => k - 273.15,
        "°F" => (k - 273.15) * 9.0 / 5.0 + 32.0,
        _ => k,
    }
}

fn unite(nom: &str) -> Option<&'static Unite> {
    UNITES.iter().find(|u| u.noms.contains(&nom))
}

/// « 10 km mi », « 10km en mi », « 20 °C to F » → (valeur, de, vers).
fn lire_conversion(args: &str) -> Result<(f64, String, String), String> {
    let args = args.to_lowercase();
    let fin_nombre = args
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',' || c == '-' || c == ' '))
        .ok_or("Exemple : /convertir 10 km mi")?;
    let valeur = calculer(args[..fin_nombre].trim()).map_err(|_| "Commencez par un nombre, par exemple 10 km mi")?;
    let mots: Vec<&str> = args[fin_nombre..]
        .split_whitespace()
        .filter(|m| !matches!(*m, "en" | "in" | "to" | "vers" | "=" | "->"))
        .collect();
    match mots.as_slice() {
        [de, vers] => Ok((valeur, de.to_string(), vers.to_string())),
        _ => Err("Indiquez deux unités, par exemple 10 km mi".into()),
    }
}

/// Convertit et renvoie un texte comme « 10 km = 6,2137119224 mi ».
pub fn convertir(args: &str) -> Result<String, String> {
    let (valeur, de, vers) = lire_conversion(args)?;
    if let (Some(a), Some(b)) = (temperature(&de), temperature(&vers)) {
        let resultat = depuis_kelvin(vers_kelvin(valeur, a), b);
        return Ok(format!("{} {a} = {} {b}", nombre(valeur), nombre(arrondi(resultat))));
    }
    let a = unite(&de).ok_or(format!("Unité inconnue : {de}"))?;
    let b = unite(&vers).ok_or(format!("Unité inconnue : {vers}"))?;
    if a.famille != b.famille {
        return Err(format!("Impossible : {} et {} ne mesurent pas la même chose", a.famille, b.famille));
    }
    let resultat = valeur * a.facteur / b.facteur;
    Ok(format!("{} {} = {} {}", nombre(valeur), a.noms[0], nombre(arrondi(resultat)), b.noms[0]))
}

/// Six chiffres significatifs suffisent pour une conversion.
fn arrondi(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let chiffres = 6 - v.abs().log10().floor() as i32 - 1;
    let p = 10f64.powi(chiffres.clamp(-10, 10));
    (v * p).round() / p
}

impl Commande for Convertir {
    fn nom(&self) -> &'static str {
        "convertir"
    }
    fn description(&self) -> &'static str {
        "Convertir une unité, par exemple /convertir 10 km mi"
    }
    fn apercu(&self, args: &str, _ctx: &Contexte) -> Result<String, String> {
        convertir(args)
    }
    fn executer(&self, args: &str, _ctx: &mut Contexte) -> Result<Resultat, String> {
        convertir(args).map(Resultat::Texte)
    }
}

// --- Minuteur ---------------------------------------------------------------

struct Minuteur;

/// « 25 » (minutes), « 90s », « 1h30 », « 2 min thé » → (secondes, libellé).
pub fn lire_duree(args: &str) -> Result<(u64, String), String> {
    let mut mots = args.split_whitespace();
    let premier = mots.next().ok_or("Exemple : /minuteur 25 (minutes) ou /minuteur 90s")?.to_lowercase();
    let mut reste: Vec<&str> = mots.collect();
    // « 2 min thé » : l'unité peut être le mot suivant.
    let mut duree = premier.clone();
    if premier.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.')
        && let Some(u) = reste.first()
        && let Some(canonique) = match u.to_lowercase().as_str() {
            "s" | "sec" | "seconde" | "secondes" => Some("s"),
            "min" | "minute" | "minutes" => Some("min"),
            "h" | "heure" | "heures" => Some("h"),
            _ => None,
        }
    {
        duree.push_str(canonique);
        reste.remove(0);
    }
    let secondes = secondes_de(&duree).ok_or(format!("Durée incomprise : {premier}"))?;
    if secondes == 0 || secondes > 24 * 3600 {
        return Err("Choisissez une durée entre 1 seconde et 24 heures".into());
    }
    Ok((secondes, reste.join(" ")))
}

fn secondes_de(texte: &str) -> Option<u64> {
    let texte = texte.replace(',', ".");
    let nombre = |t: &str| t.parse::<f64>().ok().filter(|v| *v >= 0.0);
    if let Some((h, m)) = texte.split_once('h') {
        let h = nombre(h)?;
        let m = if m.is_empty() { 0.0 } else { nombre(m.trim_end_matches("min"))? };
        return Some((h * 3600.0 + m * 60.0).round() as u64);
    }
    // Les suffixes longs d'abord : « minutes » finit aussi par « s ».
    for (suffixe, facteur) in [("minutes", 60.0), ("secondes", 1.0), ("min", 60.0), ("sec", 1.0), ("s", 1.0), ("m", 60.0)] {
        if let Some(v) = texte.strip_suffix(suffixe) {
            return nombre(v).map(|v| (v * facteur).round() as u64);
        }
    }
    nombre(&texte).map(|v| (v * 60.0).round() as u64)
}

/// « 1 h 30 min », « 25 min », « 45 s ».
pub fn duree_lisible(secondes: u64) -> String {
    let (h, m, s) = (secondes / 3600, secondes % 3600 / 60, secondes % 60);
    let mut parties = Vec::new();
    if h > 0 {
        parties.push(format!("{h} h"));
    }
    if m > 0 {
        parties.push(format!("{m} min"));
    }
    if s > 0 || parties.is_empty() {
        parties.push(format!("{s} s"));
    }
    parties.join(" ")
}

impl Commande for Minuteur {
    fn nom(&self) -> &'static str {
        "minuteur"
    }
    fn description(&self) -> &'static str {
        "Lancer un minuteur, par exemple /minuteur 25 ou /minuteur 90s thé"
    }
    fn apercu(&self, args: &str, _ctx: &Contexte) -> Result<String, String> {
        let (secondes, libelle) = lire_duree(args)?;
        let pour = if libelle.is_empty() { String::new() } else { format!(" pour « {libelle} »") };
        Ok(format!("Entrée : minuteur de {}{pour}", duree_lisible(secondes)))
    }
    fn executer(&self, args: &str, _ctx: &mut Contexte) -> Result<Resultat, String> {
        let (secondes, libelle) = lire_duree(args)?;
        Ok(Resultat::Minuteur { secondes, libelle })
    }
}

// --- Notes ------------------------------------------------------------------

const FICHIER_NOTES: &str = "notes.json";
/// Au-delà, les notes les plus anciennes sont oubliées.
const MAX_NOTES: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub texte: String,
    pub heure: u64,
}

#[derive(Default)]
pub struct Notes {
    notes: Vec<Note>,
    changed: bool,
}

impl Notes {
    pub fn load() -> Self {
        Self { notes: storage::read_json(FICHIER_NOTES).unwrap_or_default(), changed: false }
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        if self.changed {
            storage::write_json(FICHIER_NOTES, &self.notes, false)?;
            self.changed = false;
        }
        Ok(())
    }

    fn ajouter(&mut self, texte: &str) {
        self.notes.push(Note { texte: texte.chars().take(2000).collect(), heure: now() });
        if self.notes.len() > MAX_NOTES {
            self.notes.remove(0);
        }
        self.changed = true;
    }

    fn effacer(&mut self) {
        self.notes.clear();
        self.changed = true;
    }
}

struct AjouterNote;

impl Commande for AjouterNote {
    fn nom(&self) -> &'static str {
        "note"
    }
    fn description(&self) -> &'static str {
        "Garder une note, par exemple /note appeler le garage"
    }
    fn apercu(&self, args: &str, _ctx: &Contexte) -> Result<String, String> {
        if args.is_empty() {
            Err("Tapez le texte de la note après /note".into())
        } else {
            Ok("Entrée : enregistrer la note (/notes pour les relire)".into())
        }
    }
    fn executer(&self, args: &str, ctx: &mut Contexte) -> Result<Resultat, String> {
        if args.is_empty() {
            return Err("Tapez le texte de la note après /note".into());
        }
        ctx.notes.ajouter(args);
        Ok(Resultat::Texte("Note enregistrée.".into()))
    }
}

struct ListerNotes;

impl Commande for ListerNotes {
    fn nom(&self) -> &'static str {
        "notes"
    }
    fn description(&self) -> &'static str {
        "Relire ses notes (/notes effacer pour tout supprimer)"
    }
    fn apercu(&self, args: &str, ctx: &Contexte) -> Result<String, String> {
        let n = ctx.notes.notes.len();
        if args == "effacer" {
            return Ok(format!("Entrée : supprimer les {n} notes"));
        }
        Ok(match n {
            0 => "Aucune note pour l'instant".into(),
            1 => "Entrée : afficher la note".into(),
            _ => format!("Entrée : afficher les {n} notes"),
        })
    }
    fn executer(&self, args: &str, ctx: &mut Contexte) -> Result<Resultat, String> {
        if args == "effacer" {
            ctx.notes.effacer();
            return Ok(Resultat::Texte("Notes supprimées.".into()));
        }
        if ctx.notes.notes.is_empty() {
            return Ok(Resultat::Texte("Aucune note pour l'instant.".into()));
        }
        Ok(Resultat::Liste(ctx.notes.notes.iter().rev().take(30).map(|n| n.texte.clone()).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculs() {
        assert_eq!(calculer("12*7").unwrap(), 84.0);
        assert_eq!(calculer("(12 + 3) × 7").unwrap(), 105.0);
        assert_eq!(calculer("2^3^2").unwrap(), 512.0);
        assert_eq!(calculer("-3 + 5").unwrap(), 2.0);
        assert_eq!(calculer("1,5 * 2").unwrap(), 3.0);
        assert_eq!(calculer("200 * 20%").unwrap(), 40.0);
        assert_eq!(calculer("7 % 3").unwrap(), 1.0);
        assert_eq!(calculer("racine(16)").unwrap(), 4.0);
        assert!(calculer("1/0").is_err());
        assert!(calculer("(1+2").is_err());
        assert!(calculer("2 + abc").is_err());
        assert_eq!(nombre(0.1 + 0.2), "0,3");
        assert_eq!(nombre(84.0), "84");
        assert_eq!(nombre(-0.0), "0");
    }

    #[test]
    fn conversions() {
        assert_eq!(convertir("10 km mi").unwrap(), "10 km = 6,21371 mi");
        assert_eq!(convertir("100 °C en f").unwrap(), "100 °C = 212 °F");
        assert_eq!(convertir("2 kg to lb").unwrap(), "2 kg = 4,40925 lb");
        assert_eq!(convertir("90 km/h m/s").unwrap(), "90 km/h = 25 m/s");
        assert!(convertir("10 km kg").unwrap_err().contains("même chose"));
        assert!(convertir("10 km").is_err());
        assert!(convertir("km mi").is_err());
    }

    #[test]
    fn durees() {
        assert_eq!(lire_duree("25").unwrap(), (1500, String::new()));
        assert_eq!(lire_duree("90s").unwrap(), (90, String::new()));
        assert_eq!(lire_duree("1h30").unwrap(), (5400, String::new()));
        assert_eq!(lire_duree("2 min thé vert").unwrap(), (120, "thé vert".into()));
        assert_eq!(lire_duree("2minutes").unwrap().0, 120);
        assert_eq!(lire_duree("1 heure").unwrap().0, 3600);
        assert!(lire_duree("").is_err());
        assert!(lire_duree("0").is_err());
        assert!(lire_duree("bientôt").is_err());
        assert_eq!(duree_lisible(5400), "1 h 30 min");
        assert_eq!(duree_lisible(45), "45 s");
    }

    #[test]
    fn notes_et_commandes() {
        let mut notes = Notes::default();
        let mut ctx = Contexte { notes: &mut notes };
        assert!(executer("/inconnue", &mut ctx).is_none());
        assert!(executer("pas une commande", &mut ctx).is_none());
        assert_eq!(
            executer("/note acheter du pain", &mut ctx).unwrap().unwrap(),
            Resultat::Texte("Note enregistrée.".into())
        );
        executer("/note appeler Léa", &mut ctx);
        assert_eq!(
            executer("/notes", &mut ctx).unwrap().unwrap(),
            Resultat::Liste(vec!["appeler Léa".into(), "acheter du pain".into()])
        );
        let rep = apercu("/CALCUL 6*7", &ctx).unwrap();
        assert_eq!((rep.nom.as_str(), rep.texte.as_str(), rep.erreur), ("calcul", "= 42", false));
        executer("/notes effacer", &mut ctx);
        assert!(notes.notes.is_empty());
    }
}
