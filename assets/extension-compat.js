// Compléments de Feather pour les extensions écrites pour Chrome, sur Mac.
// Chargés avant leur arrière-plan, et en tête de leur fenêtre (popup) et de leur
// page d'options. WebKit n'a pas certains évènements ou fonctions de Chrome, et
// un arrière-plan qui y touche au démarrage s'arrête net : il ne répond plus à
// rien, et Proton Pass affiche « Votre navigateur ne parvient pas à activer
// Proton Pass » puis se relance en boucle.
(() => {
  const roots = [...new Set(['browser', 'chrome'].map((n) => globalThis[n]).filter(Boolean))];
  if (roots.length === 0) return;

  // Un évènement muet : il garde ses écouteurs, sans jamais les appeler.
  const event = () => {
    const listeners = new Set();
    return {
      addListener: (f) => void listeners.add(f),
      removeListener: (f) => void listeners.delete(f),
      hasListener: (f) => listeners.has(f),
      hasListeners: () => listeners.size > 0,
    };
  };

  // Pose `value` sous le nom `name` ; vrai si c'est bien ce qu'on relit.
  const define = (target, name, value) => {
    try { Object.defineProperty(target, name, { value, configurable: true, writable: true }); } catch (e) {}
    return target[name] === value;
  };

  // Ajoute à un espace (runtime…) ce qui lui manque. Directement si WebKit le
  // permet, sinon l'espace est remplacé par un mandataire qui le complète.
  const complete = (space, extra) => {
    for (const root of roots) {
      const target = root[space];
      if (!target) continue;
      const absent = Object.keys(extra).filter((n) => !(n in target) || target[n] == null);
      if (absent.length === 0) continue;
      for (const name of absent) define(target, name, extra[name]);
      if (absent.every((n) => target[n] != null)) continue;
      const p = new Proxy(target, {
        get: (t, k) => {
          if (k in extra && t[k] == null) return extra[k];
          const v = Reflect.get(t, k);
          return typeof v === 'function' ? v.bind(t) : v;
        },
        has: (t, k) => k in extra || k in t,
      });
      try { root[space] = p; } catch (e) {}
      if (root[space] !== p) define(root, space, p);
    }
  };

  // 1. Évènements et fonctions de Chrome absents de WebKit.
  const runtimeEvents = ['onUpdateAvailable', 'onRestartRequired', 'onSuspend', 'onSuspendCanceled', 'onConnectExternal', 'onMessageExternal'];
  const runtime = Object.fromEntries(runtimeEvents.map((n) => [n, event()]));
  // Les mises à jour passent par Feather : jamais rien à proposer.
  runtime.requestUpdateCheck = () => Promise.resolve({ status: 'no_update' });
  complete('runtime', runtime);

  // 2. webRequest : Chrome accepte des options (`requestBody`, `extraHeaders`…)
  // que WebKit peut refuser en levant une exception, et l'espace peut manquer
  // tout à fait. Dans les deux cas l'arrière-plan s'arrêtait à cette ligne
  // (Proton Pass l'appelle en démarrant). Un écouteur refusé est réessayé avec
  // moins d'options, puis ignoré.
  const webRequestEvents = ['onBeforeRequest', 'onBeforeSendHeaders', 'onSendHeaders', 'onHeadersReceived', 'onAuthRequired',
    'onResponseStarted', 'onBeforeRedirect', 'onCompleted', 'onErrorOccurred'];
  for (const root of roots) {
    const space = root.webRequest;
    if (!space) {
      define(root, 'webRequest', Object.fromEntries(webRequestEvents.map((n) => [n, event()])));
      continue;
    }
    for (const name of webRequestEvents) {
      const ev = space[name];
      if (!ev || typeof ev.addListener !== 'function' || ev.addListener.featherTolerant) continue;
      const original = ev.addListener;
      const tolerant = function (callback, filter, extra) {
        const attempts = [[callback, filter, extra], [callback, filter], [callback]];
        for (const args of attempts) {
          try { return original.apply(ev, args); } catch (e) {}
        }
      };
      tolerant.featherTolerant = true;
      define(ev, 'addListener', tolerant);
    }
  }

  // 3. Dans la fenêtre de l'extension (popup). Chrome y répond `undefined` à
  // tabs.getCurrent(), WebKit donne l'onglet derrière : l'extension se croit
  // alors ouverte en grand dans un onglet et quitte la taille qu'elle demande
  // (Proton Pass : 600 × 430 dans popup.html, remplacés par 100 %, d'où une
  // fenêtre mal dimensionnée).
  if (typeof window === 'undefined' || typeof document === 'undefined') return;
  const inPopup = () => {
    try {
      const ext = roots.find((r) => r.extension && typeof r.extension.getViews === 'function');
      return !!ext && ext.extension.getViews({ type: 'popup' }).includes(window);
    } catch (e) {
      return false;
    }
  };
  for (const root of roots) {
    const tabs = root.tabs;
    if (!tabs || typeof tabs.getCurrent !== 'function' || tabs.getCurrent.featherPopup) continue;
    const original = tabs.getCurrent;
    const getCurrent = function (callback) {
      if (inPopup()) {
        if (typeof callback === 'function') {
          callback(undefined);
          return;
        }
        return Promise.resolve(undefined);
      }
      return original.apply(tabs, arguments);
    };
    getCurrent.featherPopup = true;
    define(tabs, 'getCurrent', getCurrent);
  }
  // Certaines extensions mesurent la fenêtre dès le chargement pour figer leur
  // taille (variables --popup-width / --popup-height de Proton Pass). Dans la
  // bulle de WebKit la page est mesurée avant d'avoir sa taille finale : la
  // valeur figée était trop petite. Dans la bulle, la taille écrite dans la
  // page par l'extension est gardée.
  const style = CSSStyleDeclaration.prototype;
  const setProperty = style.setProperty;
  style.setProperty = function (name, value, priority) {
    if ((name === '--popup-width' || name === '--popup-height') && this === document.documentElement.style && inPopup()) return;
    return setProperty.call(this, name, value, priority);
  };
})();
