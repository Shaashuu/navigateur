// Calcul des raccourcis clavier, commun à page.js et chrome.html : Rust le pose
// dans les deux, suivi de la table de ce système (voir src/shortcuts.rs). Le
// résultat : une fonction qui donne le nom du raccourci d'une touche, ou null.
// La première ligne de la table qui convient gagne.
((table) => {
  // Une lettre par touche de modification (Ctrl, ⌘, Alt, Maj) : « 1 » appuyée,
  // « 0 » relâchée, « . » indifférente ; « pp » : Ctrl ou ⌘.
  const held = (want, on) => want === '.' || (want === '1') === on;
  const mods = (m, e) => (m[0] === 'p' ? e.ctrlKey || e.metaKey : held(m[0], e.ctrlKey) && held(m[1], e.metaKey))
    && held(m[2], e.altKey) && held(m[3], e.shiftKey);
  // Saisie dans un champ de texte (aussi dans un Shadow DOM).
  const typing = (e) => {
    const t = (e.composedPath && e.composedPath()[0]) || e.target;
    return !!t && (t.isContentEditable === true || /^(INPUT|TEXTAREA)$/.test(t.tagName || ''));
  };
  return (e) => {
    const key = String(e.key || '').toLowerCase();
    for (const b of table) {
      if (!mods(b.m, e) || (b.t === false && typing(e))) continue;
      if (b.d) {
        const digit = /^Digit([1-9])$/.exec(e.code || '');
        if (digit) return b.a + digit[1];
      } else if ((b.c ? e.code : key) === b.k) {
        return b.a;
      }
    }
    return null;
  };
})
