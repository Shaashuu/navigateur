// Compléments de Feather pour les extensions écrites pour Chrome : WebKit n'a
// pas certains évènements, et un arrière-plan qui y touche s'arrête net
// (Proton Pass : runtime.onUpdateAvailable). Des évènements muets les remplacent.
(() => {
  const missing = {
    runtime: ['onUpdateAvailable', 'onRestartRequired', 'onSuspend', 'onSuspendCanceled', 'onConnectExternal', 'onMessageExternal', 'onUserScriptMessage'],
    commands: ['onChanged'],
    permissions: ['onAdded', 'onRemoved'],
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
  for (const api of [globalThis.browser, globalThis.chrome]) {
    if (!api) continue;
    for (const [space, names] of Object.entries(missing)) {
      const target = api[space];
      if (!target) continue;
      for (const name of names) {
        if (target[name]) continue;
        try {
          Object.defineProperty(target, name, { value: event(), configurable: true });
        } catch (e) {}
      }
    }
  }
})();
