// Sonde : journal de l'arrière-plan, écrit dans le stockage (clé __bgerr) et lu par sonde-page.html.
(() => {
  const api = globalThis.browser || globalThis.chrome;
  const local = api.storage.local;
  const save = local.set.bind(local);
  const t0 = Date.now();
  const log = [];
  const note = (m) => {
    log.push(((Date.now() - t0) / 1000).toFixed(1) + ' s ' + String(m));
    try { save({ __bgerr: log.join('\n') }); } catch (e) {}
  };
  self.__note = note;
  self.addEventListener('error', (e) => note('error ' + e.message + ' @' + e.filename + ':' + e.lineno));
  self.addEventListener('unhandledrejection', (e) => note('rejet ' + (e.reason && (e.reason.stack || e.reason.message) || e.reason)));
  const c = globalThis.chrome || {};
  note('chrome=' + typeof globalThis.chrome + ' browser=' + typeof globalThis.browser + ' webRequest=' + typeof c.webRequest
    + ' onUpdateAvailable=' + typeof (c.runtime || {}).onUpdateAvailable + ' userScripts=' + typeof c.userScripts + ' offscreen=' + typeof c.offscreen);
  // Variante sans l'autorisation webRequest : écoutes factices.
  if (self.__SANS_WEBREQUEST && !c.webRequest) {
    const ev = () => ({ addListener() {}, removeListener() {}, hasListener() { return false; } });
    const fake = new Proxy({}, { get: (t, k) => (k in t ? t[k] : (t[k] = ev())) });
    for (const o of [globalThis.chrome, globalThis.browser]) {
      if (o) try { Object.defineProperty(o, 'webRequest', { value: fake, configurable: true }); } catch (e) { note('pose de webRequest : ' + e); }
    }
  }
  // Chaque appel lent, refusé ou sans réponse est noté.
  const spaces = ['declarativeNetRequest', 'scripting', 'storage.local', 'storage.session', 'storage.sync', 'tabs', 'windows', 'permissions', 'alarms', 'runtime', 'action', 'webNavigation', 'i18n', 'cookies', 'contextMenus'];
  for (const space of spaces) {
    for (const root of [globalThis.browser, globalThis.chrome]) {
      const o = root && space.split('.').reduce((a, k) => a && a[k], root);
      if (!o || o.__sonde) continue;
      try { Object.defineProperty(o, '__sonde', { value: true }); } catch (e) {}
      const keys = new Set();
      for (const k in o) keys.add(k);
      for (const k of Object.getOwnPropertyNames(Object.getPrototypeOf(o) || {})) keys.add(k);
      for (const k of keys) {
        let f;
        try { f = o[k]; } catch (e) { continue; }
        if (typeof f !== 'function' || k === 'constructor' || /^(getURL|getManifest|getMessage|getUILanguage)$/.test(k)) continue;
        const name = space + '.' + k;
        const w = function (...a) {
          let r;
          try { r = f.apply(o, a); } catch (e) { note(name + ' lève ' + e); throw e; }
          if (r && typeof r.then === 'function') {
            const s = Date.now();
            let done = false;
            setTimeout(() => { if (!done) note(name + ' toujours sans réponse après 5 s'); }, 5000);
            r.then(() => { done = true; const d = Date.now() - s; if (d > 800 || space === 'declarativeNetRequest') note(name + ' : ' + d + ' ms'); },
              (e) => { done = true; note(name + ' refusé : ' + e); });
          }
          return r;
        };
        try { o[k] = w; } catch (e) {}
        if (o[k] !== w) try { Object.defineProperty(o, k, { value: w, configurable: true, writable: true }); } catch (e) {}
      }
    }
  }
  note('sonde prête');
})();
