// Compléments de Feather pour les extensions écrites pour Chrome : WebKit n'a
// pas certains évènements, et un arrière-plan qui y touche s'arrête net
// (Proton Pass : runtime.onUpdateAvailable). Des évènements muets les remplacent.
(() => {
  const missing = {
    runtime: ['onUpdateAvailable', 'onRestartRequired', 'onSuspend', 'onSuspendCanceled', 'onConnectExternal', 'onMessageExternal'],
  };
  const note = (m) => { if (self.__note) self.__note('compléments : ' + m); };
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
  const roots = ['browser', 'chrome'];
  for (const [space, names] of Object.entries(missing)) {
    const extra = {};
    for (const name of names) extra[name] = event();
    for (const rootName of roots) {
      const root = globalThis[rootName];
      const target = root && root[space];
      if (!target) continue;
      const absent = names.filter((n) => !target[n]);
      if (absent.length === 0) continue;
      // 1. Ajout direct.
      for (const name of absent) {
        try { Object.defineProperty(target, name, { value: extra[name], configurable: true }); } catch (e) { note(rootName + '.' + space + '.' + name + ' : ' + e); }
      }
      if (absent.every((n) => target[n])) { note(rootName + '.' + space + ' : ajout direct'); continue; }
      // 2. L'espace remplacé par un mandataire.
      const p = proxied(target, extra);
      try { root[space] = p; } catch (e) {}
      if (root[space] !== p) try { Object.defineProperty(root, space, { value: p, configurable: true, writable: true }); } catch (e) { note(rootName + '.' + space + ' : ' + e); }
      if (root[space] === p) { note(rootName + '.' + space + ' : mandataire'); continue; }
      // 3. Toute l'interface remplacée par un mandataire.
      const q = new Proxy(root, {
        get: (t, k) => (k === space ? p : Reflect.get(t, k)),
      });
      try { globalThis[rootName] = q; } catch (e) {}
      if (globalThis[rootName] !== q) try { Object.defineProperty(globalThis, rootName, { value: q, configurable: true, writable: true }); } catch (e) { note(rootName + ' : ' + e); }
      note(rootName + ' : ' + (globalThis[rootName] === q ? 'interface remplacée' : 'échec'));
    }
  }
})();
