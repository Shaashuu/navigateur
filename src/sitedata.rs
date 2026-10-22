//! Réglages retenus par site : niveau de zoom et autorisations (caméra,
//! micro, position, notifications…). Rangés dans `sites.json`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wry::{PermissionKind, PermissionResponse};

use crate::storage;

const FICHIER: &str = "sites.json";

/// Les autorisations que Feather demande à l'utilisateur.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Camera,
    Microphone,
    Geolocation,
    Notifications,
    Screen,
    Clipboard,
}

impl Permission {
    /// Pour la question posée à l'utilisateur : « exemple.fr veut utiliser … ».
    pub fn label(self) -> &'static str {
        match self {
            Permission::Camera => "la caméra",
            Permission::Microphone => "le micro",
            Permission::Geolocation => "votre position",
            Permission::Notifications => "les notifications",
            Permission::Screen => "le partage d'écran",
            Permission::Clipboard => "le presse-papiers",
        }
    }

    fn from_wry(kind: PermissionKind) -> Option<Self> {
        Some(match kind {
            PermissionKind::Camera => Permission::Camera,
            PermissionKind::Microphone => Permission::Microphone,
            PermissionKind::Geolocation => Permission::Geolocation,
            PermissionKind::Notifications => Permission::Notifications,
            PermissionKind::DisplayCapture => Permission::Screen,
            PermissionKind::ClipboardRead => Permission::Clipboard,
            _ => return None,
        })
    }
}

/// Une décision retenue, pour la liste des paramètres.
#[derive(Debug, Serialize)]
pub struct Decision {
    pub host: String,
    pub permission: Permission,
    pub label: &'static str,
    pub allowed: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteData {
    zoom: BTreeMap<String, f64>,
    permissions: BTreeMap<String, BTreeMap<Permission, bool>>,
    /// Sites où le bloqueur de publicités est coupé.
    ads_off: BTreeSet<String>,
    /// Sites « toujours actifs » : leurs onglets ne sont jamais mis en veille.
    awake: BTreeSet<String>,
    /// Réponses « pour cette fois » : valables jusqu'à la fermeture de Feather.
    #[serde(skip)]
    temporary: BTreeMap<(String, Permission), bool>,
    #[serde(skip)]
    changed: bool,
}

impl SiteData {
    pub fn load() -> Self {
        storage::read_json(FICHIER).unwrap_or_default()
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        if self.changed {
            storage::write_json(FICHIER, self, false)?;
            self.changed = false;
        }
        Ok(())
    }

    // --- Zoom ---

    pub fn zoom(&self, host: &str) -> f64 {
        self.zoom.get(host).copied().unwrap_or(1.0)
    }

    /// Enregistre le zoom d'un site (100 % n'est pas gardé : c'est la valeur par défaut).
    pub fn set_zoom(&mut self, host: &str, zoom: f64) {
        if host.is_empty() {
            return;
        }
        if (zoom - 1.0).abs() < 0.01 {
            self.zoom.remove(host);
        } else {
            self.zoom.insert(host.to_string(), zoom);
        }
        self.changed = true;
    }

    // --- Bloqueur de publicités ---

    pub fn ads_off(&self) -> Vec<String> {
        self.ads_off.iter().cloned().collect()
    }

    /// Active (`true`) ou coupe le bloqueur sur ce site.
    pub fn set_ads(&mut self, host: &str, on: bool) {
        if host.is_empty() {
            return;
        }
        let changed = if on { self.ads_off.remove(host) } else { self.ads_off.insert(host.to_string()) };
        self.changed |= changed;
    }

    // --- Veille ---

    pub fn is_awake(&self, host: &str) -> bool {
        self.awake.contains(host)
    }

    /// « Toujours actif » (`true`) : les onglets de ce site ne dorment jamais.
    pub fn set_awake(&mut self, host: &str, on: bool) {
        if host.is_empty() {
            return;
        }
        let changed = if on { self.awake.insert(host.to_string()) } else { self.awake.remove(host) };
        self.changed |= changed;
    }

    // --- Autorisations ---

    /// Décision retenue pour ce site, sinon réponse donnée plus tôt dans la session.
    pub fn permission(&self, host: &str, permission: Permission) -> Option<bool> {
        self.permissions
            .get(host)
            .and_then(|site| site.get(&permission).copied())
            .or_else(|| self.temporary.get(&(host.to_string(), permission)).copied())
    }

    /// Réponse non retenue : vaut pour la session, sans être enregistrée. Le
    /// moteur, qui redemande parfois après le script de la page, reçoit la même.
    pub fn set_temporary(&mut self, host: &str, permission: Permission, allowed: bool) {
        if !host.is_empty() {
            self.temporary.insert((host.to_string(), permission), allowed);
        }
    }

    pub fn set_permission(&mut self, host: &str, permission: Permission, allowed: bool) {
        if host.is_empty() {
            return;
        }
        self.permissions.entry(host.to_string()).or_default().insert(permission, allowed);
        self.changed = true;
    }

    pub fn forget_permission(&mut self, host: &str, permission: Permission) {
        self.temporary.remove(&(host.to_string(), permission));
        if let Some(site) = self.permissions.get_mut(host) {
            site.remove(&permission);
            if site.is_empty() {
                self.permissions.remove(host);
            }
            self.changed = true;
        }
    }

    pub fn decisions(&self) -> Vec<Decision> {
        self.permissions
            .iter()
            .flat_map(|(host, site)| {
                site.iter().map(|(p, allowed)| Decision {
                    host: host.clone(),
                    permission: *p,
                    label: p.label(),
                    allowed: *allowed,
                })
            })
            .collect()
    }

    /// Réponse au moteur quand une page demande une autorisation : la décision
    /// retenue pour ce site, sinon le comportement par défaut. La question
    /// elle-même est posée avant, par le script injecté (voir `assets/page.js`).
    pub fn answer(&self, host: &str, kind: PermissionKind) -> PermissionResponse {
        match Permission::from_wry(kind).and_then(|p| self.permission(host, p)) {
            Some(true) => PermissionResponse::Allow,
            Some(false) => PermissionResponse::Deny,
            None => PermissionResponse::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toujours_actif() {
        let mut s = SiteData::default();
        assert!(!s.is_awake("mail.fr"));
        s.set_awake("mail.fr", true);
        assert!(s.is_awake("mail.fr") && s.changed);
        s.set_awake("mail.fr", false);
        assert!(!s.is_awake("mail.fr"));
        s.set_awake("", true);
        assert!(s.awake.is_empty());
    }

    #[test]
    fn zoom_et_autorisations() {
        let mut s = SiteData::default();
        assert_eq!(s.zoom("a.fr"), 1.0);
        s.set_zoom("a.fr", 1.25);
        assert_eq!(s.zoom("a.fr"), 1.25);
        s.set_zoom("a.fr", 1.0);
        assert!(s.zoom.is_empty());

        s.set_permission("visio.fr", Permission::Camera, true);
        s.set_permission("pub.fr", Permission::Notifications, false);
        assert!(matches!(s.answer("visio.fr", PermissionKind::Camera), PermissionResponse::Allow));
        assert!(matches!(s.answer("visio.fr", PermissionKind::Microphone), PermissionResponse::Default));
        assert!(matches!(s.answer("pub.fr", PermissionKind::Notifications), PermissionResponse::Deny));
        assert_eq!(s.decisions().len(), 2);
        s.set_temporary("carte.fr", Permission::Geolocation, true);
        assert_eq!(s.permission("carte.fr", Permission::Geolocation), Some(true));
        assert_eq!(s.decisions().len(), 2);
        s.forget_permission("visio.fr", Permission::Camera);
        assert_eq!(s.decisions().len(), 1);
        assert!(!s.permissions.contains_key("visio.fr"));
    }
}
