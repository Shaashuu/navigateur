//! Le « poids » d'un onglet : mémoire et processeur utilisés par sa page,
//! affiché par un anneau autour de sa pastille.
//!
//! - macOS : chaque page WebKit tourne dans son propre processus ; on lit sa
//!   empreinte mémoire (`proc_pid_rusage`) et son temps de calcul (`proc_pidinfo`).
//! - Partout : la page mesure elle-même le retard de sa boucle d'événements
//!   (signe qu'elle calcule beaucoup) et, sous Windows, la mémoire de son
//!   JavaScript (voir `assets/page.js`).

use serde::Serialize;

/// Mesures d'un onglet ; `None` quand le système ne les donne pas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Weight {
    /// Mémoire, en mégaoctets.
    pub memory_mb: Option<u32>,
    /// Processeur, en pourcentage d'un cœur.
    pub cpu: Option<u32>,
    /// Retard moyen de la boucle d'événements de la page, en millisecondes.
    pub lag_ms: Option<u32>,
    /// 0 : léger, 1 : moyen, 2 : lourd.
    pub level: u8,
}

impl Weight {
    /// Recalcule `level` à partir des mesures connues.
    pub fn update_level(&mut self) {
        let mem = self.memory_mb.unwrap_or(0);
        let cpu = self.cpu.unwrap_or(0);
        let lag = self.lag_ms.unwrap_or(0);
        self.level = if mem >= 700 || cpu >= 60 || lag >= 300 {
            2
        } else if mem >= 300 || cpu >= 20 || lag >= 100 {
            1
        } else {
            0
        };
    }
}

/// Mesure le processus d'une page (macOS seulement). Garde la mesure
/// précédente de chaque processus pour en déduire l'usage du processeur.
#[derive(Default)]
pub struct Sampler {
    #[cfg(target_os = "macos")]
    previous: std::collections::HashMap<i32, (u64, std::time::Instant)>,
}

impl Sampler {
    /// Mémoire (Mo) et processeur (%) de la page, si le système les donne.
    #[cfg(target_os = "macos")]
    pub fn sample(&mut self, webview: &wry::WebView) -> Option<(u32, Option<u32>)> {
        use wry::WebViewExtMacOS;
        let wv = webview.webview();
        let pid = mac::process_id(&wv)?;
        let (resident, cpu_ns) = mac::task_info(pid)?;
        let now = std::time::Instant::now();
        let cpu = self.previous.insert(pid, (cpu_ns, now)).and_then(|(before, at)| {
            let wall = now.duration_since(at).as_nanos() as f64;
            (wall > 0.0).then(|| ((cpu_ns.saturating_sub(before)) as f64 / wall * 100.0).round() as u32)
        });
        // Les processus plus relevés depuis une minute (onglet fermé ou endormi)
        // sortent de la table : avant, elle gardait une ligne par page disparue.
        self.previous.retain(|_, (_, at)| now.duration_since(*at) < std::time::Duration::from_secs(60));
        Some(((resident / 1_048_576) as u32, cpu))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn sample(&mut self, _webview: &wry::WebView) -> Option<(u32, Option<u32>)> {
        None
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::NSObjectProtocol;
    use objc2::{msg_send, sel};
    use wry::WryWebView;

    /// Numéro du processus WebKit de la page. `_webProcessIdentifier` n'est pas
    /// documenté mais existe depuis des années ; on vérifie qu'il répond.
    pub fn process_id(wv: &Retained<WryWebView>) -> Option<i32> {
        if !wv.respondsToSelector(sel!(_webProcessIdentifier)) {
            return None;
        }
        // SAFETY : le sélecteur existe (vérifié ci-dessus) et renvoie un pid_t.
        let pid: i32 = unsafe { msg_send![&**wv, _webProcessIdentifier] };
        (pid > 0).then_some(pid)
    }

    /// Mémoire (octets, voir `footprint`) et temps de calcul total (nanosecondes).
    // `mach_timebase_info` est marqué obsolète dans libc (au profit de `mach2`),
    // mais reste la fonction du système : on évite une dépendance de plus.
    #[allow(deprecated)]
    pub fn task_info(pid: i32) -> Option<(u64, u64)> {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        // SAFETY : `info` est un tampon de la bonne taille pour PROC_PIDTASKINFO.
        let read = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size)
        };
        if read != size {
            return None;
        }
        // Les temps sont en unités « mach » : il faut les convertir en nanosecondes
        // (le rapport vaut 1 sur Intel, pas sur Apple Silicon).
        let mut base = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY : simple lecture dans une structure locale.
        unsafe {
            libc::mach_timebase_info(&raw mut base)
        };
        let ticks = info.pti_total_user + info.pti_total_system;
        let ns = if base.denom == 0 { ticks } else { ticks * u64::from(base.numer) / u64::from(base.denom) };
        Some((footprint(pid).unwrap_or(info.pti_resident_size), ns))
    }

    /// Empreinte physique (octets) : la mémoire propre au processus, celle
    /// qu'affiche le Moniteur d'activité. La mémoire résidente comptait aussi
    /// les bibliothèques du système partagées entre tous les processus WebKit,
    /// et gonflait le chiffre.
    fn footprint(pid: i32) -> Option<u64> {
        let mut usage: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY : `usage` est un tampon de la bonne taille pour RUSAGE_INFO_V4.
        let read = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, (&raw mut usage).cast()) };
        (read == 0 && usage.ri_phys_footprint > 0).then_some(usage.ri_phys_footprint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn niveaux() {
        let mut w = Weight::default();
        w.update_level();
        assert_eq!(w.level, 0);
        w.memory_mb = Some(450);
        w.update_level();
        assert_eq!(w.level, 1);
        w.lag_ms = Some(500);
        w.update_level();
        assert_eq!(w.level, 2);
    }
}
