//! Horloge unique de Feather : les réveils prévus (enregistrement, onglets
//! fermés à détruire, titres, minuteurs de `/minuteur`, relevés périodiques)
//! sont rangés ici, et la boucle d'événements dort jusqu'au plus proche
//! (`ControlFlow::WaitUntil`, voir `main`). Avant, chacun avait son fil
//! système qui dormait puis envoyait un événement, et un fil réveillait le
//! programme chaque seconde, fenêtre inactive comprise.

use std::time::{Duration, Instant};

/// Ce qu'il faut faire à l'heure dite (voir `App::run_timers`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Timer {
    /// Toutes les cinq secondes : poids, son et veille des onglets (voir `App::survey`).
    Survey,
    /// Chaque seconde pendant un téléchargement ou une mise à jour : la progression.
    Progress,
    /// Toutes les deux secondes, panneau « Utilisation de la mémoire » ouvert.
    Memory,
    /// Chaque seconde en mode banc (voir `banc.rs`).
    Banc,
    /// Relire le plein écran, juste après un redimensionnement.
    Fullscreen,
    /// Les onglets fermés ou endormis peuvent être détruits.
    Reap,
    /// Enregistrer l'historique, les favoris et la session.
    Persist,
    /// Des changements de titre retenus partent vers l'interface.
    Titles,
    /// Un minuteur de `/minuteur` est arrivé à son terme.
    Alert { seconds: u64, label: String },
}

/// Les réveils prévus, dans l'ordre où ils ont été demandés (il y en a peu).
#[derive(Default)]
pub struct Clock {
    due: Vec<(Instant, Timer)>,
}

impl Clock {
    /// Prévoit `timer` dans `delay`, en plus de ceux déjà prévus.
    pub fn after(&mut self, delay: Duration, timer: Timer) {
        // Une durée démesurée ne déborde pas : le réveil n'arrivera jamais.
        if let Some(at) = Instant::now().checked_add(delay) {
            self.due.push((at, timer));
        }
    }

    /// Prévoit `timer` dans `delay`, sauf s'il l'est déjà : au plus un à la fois.
    pub fn once(&mut self, delay: Duration, timer: Timer) {
        if !self.has(&timer) {
            self.after(delay, timer);
        }
    }

    /// `timer` est déjà prévu.
    pub fn has(&self, timer: &Timer) -> bool {
        self.due.iter().any(|(_, t)| t == timer)
    }

    /// Le prochain réveil.
    pub fn next(&self) -> Option<Instant> {
        self.due.iter().map(|(at, _)| *at).min()
    }

    /// Retire et renvoie les réveils arrivés à l'heure `now`, du plus ancien au
    /// plus récent.
    pub fn take_due(&mut self, now: Instant) -> Vec<Timer> {
        let mut due: Vec<(Instant, Timer)> = self.due.extract_if(.., |(at, _)| *at <= now).collect();
        due.sort_by_key(|(at, _)| *at);
        due.into_iter().map(|(_, timer)| timer).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveils_dans_l_ordre() {
        let mut clock = Clock::default();
        assert_eq!(clock.next(), None);
        clock.after(Duration::from_secs(60), Timer::Persist);
        clock.after(Duration::ZERO, Timer::Reap);
        clock.after(Duration::ZERO, Timer::Reap);
        clock.after(Duration::from_millis(1), Timer::Titles);
        let later = Instant::now() + Duration::from_secs(1);
        // Deux destructions prévues : deux réveils (chaque onglet attend ses trois secondes).
        assert_eq!(clock.take_due(later), vec![Timer::Reap, Timer::Reap, Timer::Titles]);
        assert!(clock.take_due(later).is_empty());
        assert!(clock.next().is_some_and(|at| at > later));
        assert_eq!(clock.take_due(later + Duration::from_secs(60)), vec![Timer::Persist]);
        assert_eq!(clock.next(), None);
    }

    #[test]
    fn un_seul_a_la_fois() {
        let mut clock = Clock::default();
        clock.once(Duration::from_secs(2), Timer::Persist);
        clock.once(Duration::ZERO, Timer::Persist);
        assert!(clock.has(&Timer::Persist));
        // Le premier demandé reste : l'enregistrement n'est pas avancé.
        assert!(clock.take_due(Instant::now()).is_empty());
        // Deux minuteurs de `/minuteur` différents se suivent sans se gêner.
        clock.once(Duration::ZERO, Timer::Alert { seconds: 60, label: "thé".into() });
        clock.once(Duration::ZERO, Timer::Alert { seconds: 90, label: String::new() });
        assert_eq!(clock.take_due(Instant::now()).len(), 2);
        // Durée démesurée : aucun réveil, aucune panique.
        clock.after(Duration::MAX, Timer::Titles);
        assert!(!clock.has(&Timer::Titles));
    }
}
