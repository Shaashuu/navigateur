//! macOS : compléments chargés avant les scripts des extensions, pour ce que
//! WebKit fait autrement que Chrome (voir assets/extension-compat.js et
//! assets/extension-lock.js). Ailleurs, seulement pour les tests.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{read_json, short_hash};

/// Compléments chargés avant l'arrière-plan des extensions (voir le fichier).
pub(super) const COMPAT_JS: &str = include_str!("../../assets/extension-compat.js");
pub(super) const COMPAT_FILE: &str = "feather-compat.js";
/// Verrou de `browser` et `chrome` (voir le fichier) : en tête des
/// compléments, et seul devant les scripts de contenu.
pub(super) const LOCK_JS: &str = include_str!("../../assets/extension-lock.js");
pub(super) const LOCK_FILE: &str = "feather-lock.js";
/// Arrière-plan de remplacement : les compléments, puis celui de l'extension.
pub(super) const WRAPPER_FILE: &str = "feather-background.js";

/// Contenu de `feather-compat.js` : le verrou, puis les compléments.
pub(super) fn compat_js() -> String {
    format!("{LOCK_JS}\n{COMPAT_JS}")
}

/// Sur Mac, fait passer l'arrière-plan de l'extension après les compléments de
/// Feather. Le manifeste désigne alors `feather-background.js`, qui charge
/// `feather-compat.js` puis le fichier d'origine. Ce fichier est réécrit à
/// chaque chargement (une nouvelle version de Feather y met la sienne) ; le
/// manifeste, lui, n'est modifié que s'il lui manque quelque chose.
///
/// Chaque page de l'extension charge aussi les compléments en premier, et ses
/// scripts de contenu le verrou (`feather-lock.js`) : Proton Pass remplace
/// `browser` et `chrome` partout où il tourne.
pub(super) fn add_compat(dir: &Path) -> Result<(), String> {
    let path = dir.join("manifest.json");
    let Some(Value::Object(mut manifest)) = read_json(&path) else {
        return Err("manifest.json illisible".into());
    };
    fs::write(dir.join(COMPAT_FILE), compat_js()).map_err(|e| e.to_string())?;
    fs::write(dir.join(LOCK_FILE), LOCK_JS).map_err(|e| e.to_string())?;
    // Toutes les pages : fenêtre (popup), options, et les autres (Proton Pass
    // en a sept, dont le menu affiché dans les champs des sites). Une page qui
    // ne peut pas être modifiée reste telle quelle, le reste continue.
    for page in html_pages(dir) {
        if let Err(err) = compat_page(&page) {
            eprintln!("compléments non ajoutés à {} : {err}", page.display());
        }
    }
    let mut changed = false;
    if let Some(Value::Object(background)) = manifest.get_mut("background") {
        if let Some(worker) = background.get("service_worker").and_then(Value::as_str) {
            let worker = worker.trim_start_matches(['.', '/']);
            // Déjà enveloppé à un lancement précédent : l'original est relu dans l'enveloppe.
            let wrapped = worker == WRAPPER_FILE;
            let original = if wrapped {
                let text = fs::read_to_string(dir.join(WRAPPER_FILE)).map_err(|e| e.to_string())?;
                wrapped_original(&text).ok_or("arrière-plan d'origine introuvable dans feather-background.js")?
            } else {
                format!("/{worker}")
            };
            let module = background.get("type").and_then(Value::as_str) == Some("module");
            fs::write(dir.join(WRAPPER_FILE), wrapper_js(&original, module)?).map_err(|e| e.to_string())?;
            if !wrapped {
                background.insert("service_worker".into(), Value::String(WRAPPER_FILE.into()));
                changed = true;
            }
        } else if let Some(Value::Array(scripts)) = background.get_mut("scripts") {
            // Manifeste v2 (ou arrière-plan de Firefox) : les compléments en premier.
            if scripts.first().and_then(Value::as_str) != Some(COMPAT_FILE) {
                scripts.insert(0, Value::String(COMPAT_FILE.into()));
                changed = true;
            }
        }
    }
    // Scripts de contenu : le verrou en premier, dans le monde isolé de
    // l'extension (ceux qu'elle y injecte ensuite par `scripting` en profitent).
    // Ceux du monde de la page (`MAIN`) n'ont ni `browser` ni `chrome`.
    if let Some(Value::Array(entries)) = manifest.get_mut("content_scripts") {
        for entry in entries.iter_mut().filter_map(Value::as_object_mut) {
            if entry.get("world").and_then(Value::as_str).is_some_and(|w| w.eq_ignore_ascii_case("main")) {
                continue;
            }
            let Some(Value::Array(js)) = entry.get_mut("js") else { continue };
            if js.is_empty() || js.first().and_then(Value::as_str) == Some(LOCK_FILE) {
                continue;
            }
            js.insert(0, Value::String(LOCK_FILE.into()));
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

/// Les pages HTML de l'extension, sous-dossiers compris (sans suivre les liens).
fn html_pages(dir: &Path) -> Vec<PathBuf> {
    let mut pages = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() && depth < 8 {
                stack.push((path, depth + 1));
            } else if kind.is_file()
                && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
            {
                pages.push(path);
            }
        }
    }
    pages
}

/// Contenu de `feather-background.js` : les compléments, puis l'arrière-plan
/// d'origine (`original` : chemin depuis la racine de l'extension).
///
/// Arrière-plan classique (Proton Pass) : une erreur au premier niveau du
/// fichier d'origine ne l'empêche plus de s'installer. WebKit le refusait
/// entier, et l'extension ne répondait plus à rien (« difficulties
/// activating ») ; ce qui a été préparé avant l'erreur marche désormais, et
/// l'erreur reste signalée (console, liste des erreurs du moteur, panneau).
/// Un module ne peut pas être chargé ainsi : `import()` est interdit dans un
/// service worker.
pub(super) fn wrapper_js(original: &str, module: bool) -> Result<String, String> {
    // Échappé pour JavaScript ; premier guillemet double du fichier (voir `wrapped_original`).
    let original = serde_json::to_string(original).map_err(|e| e.to_string())?;
    // WebKit garde avec l'arrière-plan les fichiers qu'il charge : l'empreinte
    // des compléments change ce fichier-ci quand ils changent, et l'arrière-plan
    // est alors réinstallé avec les nouveaux.
    let version = short_hash(&compat_js());
    Ok(if module {
        format!("// Compléments de Feather {version}\nimport '/{COMPAT_FILE}';\nimport {original};\n")
    } else {
        format!(
            r"// Compléments de Feather {version}
importScripts('/{COMPAT_FILE}');
try {{
  importScripts({original});
}} catch (e) {{
  // Filet de Feather : l'arrière-plan s'installe avec ce qu'il a préparé avant l'erreur.
  if (typeof reportError === 'function') reportError(e); else console.error(e);
}}
"
        )
    })
}

/// L'arrière-plan d'origine noté dans `feather-background.js` : le premier
/// texte entre guillemets doubles, dans toutes les versions de l'enveloppe.
pub(super) fn wrapped_original(wrapper: &str) -> Option<String> {
    let start = wrapper.find('"')?;
    serde_json::Deserializer::from_str(&wrapper[start..]).into_iter::<String>().next()?.ok()
}

/// Balise qui charge les compléments en tête d'une page de l'extension.
pub(super) const COMPAT_TAG: &str = "<script src=\"/feather-compat.js\"></script>";

/// Ajoute les compléments en premier script d'une page : juste après `<head>`,
/// sinon après `<html>` ou `<!doctype>` (avant, la page passerait en mode de
/// compatibilité), sinon au tout début. Sans effet la deuxième fois.
fn compat_page(path: &Path) -> Result<(), String> {
    let html = fs::read_to_string(path).map_err(|e| e.to_string())?;
    if html.contains(COMPAT_TAG) {
        return Ok(());
    }
    let lower = html.to_ascii_lowercase();
    let at = ["<head", "<html", "<!doctype"].iter().find_map(|tag| after_tag(&lower, tag)).unwrap_or(0);
    let text = format!("{}{COMPAT_TAG}{}", &html[..at], &html[at..]);
    fs::write(path, text).map_err(|e| e.to_string())
}

/// Fin de la première balise `<nom>` ou `<nom …>` (pas `<header>` pour `<head`).
fn after_tag(lower: &str, tag: &str) -> Option<usize> {
    lower
        .match_indices(tag)
        .find(|(i, _)| matches!(lower.as_bytes().get(i + tag.len()), Some(b'>' | b' ' | b'\t' | b'\n' | b'\r')))
        .and_then(|(i, _)| lower[i..].find('>').map(|end| i + end + 1))
}
