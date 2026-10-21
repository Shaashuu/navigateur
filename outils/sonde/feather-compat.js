// Compléments de Feather pour les extensions écrites pour Chrome, chargés avant
// leur arrière-plan sur Mac. WebKit n'a pas certains évènements de Chrome, et un
// arrière-plan qui y touche s'arrête net (Proton Pass : runtime.onUpdateAvailable).
// Des évènements muets les remplacent.
(() => {
  const missing = {
    runtime: ['onUpdateAvailable', 'onRestartRequired', 'onSuspend', 'onSuspendCanceled', 'onConnectExternal', 'onMessageExternal'],
  };
  const event = () => {
    const listeners = new Set();
    return {
      addListener: (f) => void listeners.add(f),
      removeListener: (f) => void listeners.delete(f),
      hasListener: (f) => listeners.has(f),
      hasListeners: () => listeners.size > 0,
    };
  };
  // Un espace (runtime…) vu à travers un mandataire qui ajoute ce qui manque.
  const proxied = (target, extra) => new Proxy(target, {
    get: (t, k) => {
      if (k in extra && !(k in t)) return extra[k];
      const v = Reflect.get(t, k);
      return typeof v === 'function' ? v.bind(t) : v;
    },
    has: (t, k) => k in extra || k in t,
  });
  for (const [space, names] of Object.entries(missing)) {
    const extra = {};
    for (const name of names) extra[name] = event();
    for (const rootName of ['browser', 'chrome']) {
      const root = globalThis[rootName];
      const target = root && root[space];
      if (!target) continue;
      const absent = names.filter((n) => !target[n]);
      if (absent.length === 0) continue;
      // 1. Ajout direct (ce qui marche avec WebKit sur macOS 15).
      for (const name of absent) {
        try { Object.defineProperty(target, name, { value: extra[name], configurable: true }); } catch (e) {}
      }
      if (absent.every((n) => target[n])) continue;
      // 2. Sinon l'espace remplacé par un mandataire.
      const p = proxied(target, extra);
      try { root[space] = p; } catch (e) {}
      if (root[space] !== p) try { Object.defineProperty(root, space, { value: p, configurable: true, writable: true }); } catch (e) {}
    }
  }
})();
