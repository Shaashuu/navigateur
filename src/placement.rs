//! Taille et position de la fenêtre, retrouvées au lancement suivant.
//!
//! Au tout premier lancement, la fenêtre s'ouvre en grand (agrandie). Ensuite,
//! elle reprend la taille et la place qu'elle avait à la fermeture, et reste
//! agrandie si elle l'était. Une fenêtre en plein écran revient agrandie.

use serde::{Deserialize, Serialize};
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder};

use crate::storage;

const FILE: &str = "fenetre.json";
/// Taille minimale de la fenêtre (en points).
pub const MIN_WIDTH: f64 = 480.0;
pub const MIN_HEIGHT: f64 = 360.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Placement {
    /// Taille de la page entière hors bordures, en points (fenêtre non agrandie).
    pub width: f64,
    pub height: f64,
    /// Coin haut gauche de la fenêtre, en pixels de l'écran (fenêtre non agrandie).
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub maximized: bool,
}

impl Default for Placement {
    fn default() -> Self {
        Self { width: 1200.0, height: 800.0, x: None, y: None, maximized: true }
    }
}

impl Placement {
    /// La dernière place enregistrée, ou « agrandie » au premier lancement.
    pub fn load() -> Self {
        storage::read_json(FILE).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Err(err) = storage::write_json(FILE, self, false) {
            eprintln!("taille de la fenêtre non enregistrée : {err}");
        }
    }

    /// Applique la place à la fenêtre qui va être créée. La position n'est
    /// reprise que si elle tombe sur un écran encore branché.
    pub fn apply<T>(&self, builder: WindowBuilder, target: &EventLoopWindowTarget<T>) -> WindowBuilder {
        let width = self.width.max(MIN_WIDTH);
        let height = self.height.max(MIN_HEIGHT);
        let mut builder = builder.with_inner_size(LogicalSize::new(width, height)).with_maximized(self.maximized);
        if let (Some(x), Some(y)) = (self.x, self.y)
            && on_screen(target, x, y)
        {
            builder = builder.with_position(PhysicalPosition::new(x, y));
        }
        builder
    }

    /// Note la taille et la place actuelles. Agrandie ou en plein écran, la
    /// fenêtre garde la taille d'avant, pour y revenir quand on la rétrécit.
    pub fn track(&mut self, window: &Window) {
        if window.is_minimized() {
            return;
        }
        let big = window.is_maximized() || window.fullscreen().is_some();
        self.maximized = big;
        if big {
            return;
        }
        let size = window.inner_size().to_logical::<f64>(window.scale_factor());
        if size.width >= MIN_WIDTH && size.height >= MIN_HEIGHT {
            self.width = size.width;
            self.height = size.height;
        }
        if let Ok(pos) = window.outer_position() {
            self.x = Some(pos.x);
            self.y = Some(pos.y);
        }
    }
}

/// Le coin haut gauche (un peu décalé, pour pouvoir attraper la barre de
/// titre) est-il sur l'un des écrans ?
fn on_screen<T>(target: &EventLoopWindowTarget<T>, x: i32, y: i32) -> bool {
    let (x, y) = (i64::from(x) + 40, i64::from(y) + 20);
    target.available_monitors().any(|m| {
        let p = m.position();
        let s = m.size();
        x >= i64::from(p.x) && y >= i64::from(p.y) && x < i64::from(p.x) + i64::from(s.width) && y < i64::from(p.y) + i64::from(s.height)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premier_lancement_agrandi() {
        assert!(Placement::default().maximized);
    }

    #[test]
    fn fichier_incomplet() {
        let p: Placement = serde_json::from_str(r#"{"width": 900}"#).unwrap();
        assert_eq!(p.width, 900.0);
        assert_eq!(p.height, 800.0);
        assert!(p.maximized);
        assert_eq!(p.x, None);
    }
}
