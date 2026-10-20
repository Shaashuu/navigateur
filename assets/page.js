(() => {
  const post = (msg) => window.ipc.postMessage(JSON.stringify(msg));
  const send = (shortcut) => post({ shortcut });
  window.addEventListener('keydown', (e) => {
    const k = e.key.toLowerCase();
    const ctrl = (e.ctrlKey || e.metaKey) && !e.altKey;
    let a = null;
    // Ctrl+Maj+T et Ctrl+Maj+N passent avant Ctrl+T (même touche).
    if (ctrl && e.shiftKey && k === 't') a = 'reopen_tab';
    else if (ctrl && e.shiftKey && k === 'n') a = 'new_private_tab';
    else if (ctrl && !e.shiftKey && k === 'f') a = 'find';
    else if (ctrl && !e.shiftKey && k === 'p') a = 'print';
    else if (ctrl && (k === '+' || k === '=')) a = 'zoom_in';
    else if (ctrl && (k === '-' || k === '_')) a = 'zoom_out';
    else if (ctrl && k === '0') a = 'zoom_reset';
    else if (e.altKey && !e.ctrlKey && !e.metaKey && k === 'arrowleft') a = 'back';
    else if (e.altKey && !e.ctrlKey && !e.metaKey && k === 'arrowright') a = 'forward';
    else if (e.metaKey && !e.ctrlKey && !e.altKey && (k === '[' || k === ']')) a = k === '[' ? 'back' : 'forward';
    else if (k === 'f5' || (ctrl && k === 'r')) a = 'reload';
    else if (ctrl && (k === 'l' || k === 'k')) a = 'focus_bar';
    else if (ctrl && k === 't') a = 'new_tab';
    else if (ctrl && k === 'w') a = 'close_tab';
    else if (e.ctrlKey && !e.metaKey && !e.altKey && k === 'm') a = 'toggle_mute';
    else if (ctrl && k === 'tab') a = e.shiftKey ? 'previous_tab' : 'next_tab';
    else if (ctrl && k === ',') a = 'settings';
    else if (ctrl && k === 'd') a = 'toggle_bookmark';
    if (a) { e.preventDefault(); e.stopPropagation(); send(a); }
  }, true);

  // Boutons latéraux de la souris : précédent et suivant (aussi dans les cadres).
  for (const type of ['mousedown', 'mouseup', 'auxclick']) {
    window.addEventListener(type, (e) => {
      if (e.button !== 3 && e.button !== 4) return;
      e.preventDefault();
      if (type === 'mouseup') send(e.button === 3 ? 'back' : 'forward');
    }, true);
  }

  // Clic de molette sur un lien : ouvre la page dans un nouvel onglet (aussi dans les cadres).
  const linkOf = (e) => {
    const a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    return a && /^https?:/i.test(a.href) ? a.href : null;
  };
  window.addEventListener('mousedown', (e) => { if (e.button === 1 && linkOf(e)) e.preventDefault(); }, true);
  window.addEventListener('auxclick', (e) => {
    if (e.button !== 1) return;
    const url = linkOf(e);
    if (!url) return;
    e.preventDefault();
    e.stopPropagation();
    post({ open_tab: url });
  }, true);

  // Menu d'un lien (clic droit) : ouvrir dans un nouvel onglet, en arrière-plan,
  // dans un onglet privé, ou copier l'adresse. Maj+clic droit garde le menu du système.
  // Le menu vit dans un « shadow DOM » fermé : le site ne peut ni le lire ni le styler.
  let menuHost = null;
  const closeMenu = () => { if (menuHost) { menuHost.remove(); menuHost = null; } };
  const copy = (text) => {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).catch(() => {});
      return;
    }
    const t = document.createElement('textarea');
    t.value = text;
    document.documentElement.appendChild(t);
    t.select();
    try { document.execCommand('copy'); } catch (_) {}
    t.remove();
  };
  window.addEventListener('contextmenu', (e) => {
    closeMenu();
    if (e.shiftKey) return;
    const url = linkOf(e);
    if (!url) return;
    e.preventDefault();
    e.stopPropagation();
    menuHost = document.createElement('feather-menu');
    const root = menuHost.attachShadow({ mode: 'closed' });
    const dark = matchMedia('(prefers-color-scheme: dark)').matches;
    root.innerHTML = `<style>
      :host { all: initial; position: fixed; z-index: 2147483647; left: 0; top: 0; }
      .m { position: fixed; min-width: 230px; padding: 5px; border-radius: 10px;
        font: 13px -apple-system, "Segoe UI", system-ui, sans-serif;
        background: ${dark ? '#26262b' : '#ffffff'}; color: ${dark ? '#eeeef0' : '#1d1d1f'};
        border: 1px solid ${dark ? '#3a3a40' : '#d8d8dc'}; box-shadow: 0 8px 28px rgba(0,0,0,.22); }
      button { all: unset; display: block; box-sizing: border-box; width: 100%; padding: 7px 10px;
        border-radius: 6px; cursor: default; }
      button:hover, button:focus { background: ${dark ? '#3b3b43' : '#ececf0'}; }
      hr { border: 0; border-top: 1px solid ${dark ? '#3a3a40' : '#e4e4e8'}; margin: 4px 2px; }
    </style><div class="m" role="menu">
      <button data-how="tab">Ouvrir dans un nouvel onglet</button>
      <button data-how="background">Ouvrir en arrière-plan</button>
      <button data-how="private">Ouvrir dans un onglet privé</button>
      <hr><button data-how="copy">Copier l'adresse du lien</button></div>`;
    const menu = root.querySelector('.m');
    menu.addEventListener('mousedown', (ev) => ev.stopPropagation());
    menu.addEventListener('click', (ev) => {
      const b = ev.target.closest('button');
      if (!b) return;
      if (b.dataset.how === 'copy') copy(url);
      else post({ link: { url, how: b.dataset.how } });
      closeMenu();
    });
    document.documentElement.appendChild(menuHost);
    // On garde le menu dans la fenêtre.
    const r = menu.getBoundingClientRect();
    menu.style.left = Math.max(4, Math.min(e.clientX, innerWidth - r.width - 4)) + 'px';
    menu.style.top = Math.max(4, Math.min(e.clientY, innerHeight - r.height - 4)) + 'px';
  }, true);
  window.addEventListener('mousedown', closeMenu, true);
  window.addEventListener('blur', closeMenu);
  window.addEventListener('scroll', closeMenu, true);
  window.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeMenu(); }, true);

  // Le reste ne concerne que la page elle-même, pas ses cadres (publicités, lecteurs
  // intégrés…) : sous Windows, ce script est injecté dans chacun d'eux.
  if (window.top !== window) return;

  // YouTube : l'aperçu vidéo qui couvre la miniature au survol avale le clic dans
  // le moteur de macOS ; sans lui, la miniature redevient un simple lien.
  if (/(^|\.)youtube\.com$/.test(location.hostname)) {
    const css = document.createElement('style');
    css.textContent = 'ytd-video-preview,#video-preview{display:none!important}';
    const add = () => { const at = document.head || document.documentElement; if (at) at.appendChild(css); };
    add();
    document.addEventListener('DOMContentLoaded', add);
  }

  // Son : prévient l'interface quand une vidéo ou une musique joue, et
  // permet de couper le son de l'onglet.
  let muted = false;
  let last = false;
  const media = () => Array.from(document.querySelectorAll('video, audio'));
  const audible = (m) => !m.paused && !m.ended && (m.muted ? m.__featherMuted === true : m.volume > 0);
  const report = () => {
    const now = media().some(audible);
    if (now !== last) { last = now; post({ audible: now }); }
  };
  const silence = (m) => { if (!m.muted) { m.muted = true; m.__featherMuted = true; } };
  for (const type of ['play', 'playing', 'pause', 'ended', 'volumechange', 'emptied']) {
    document.addEventListener(type, (e) => {
      if (muted && type === 'play' && e.target instanceof HTMLMediaElement) silence(e.target);
      report();
    }, true);
  }
  // Icône et nom du site, pour la pastille de l'onglet. On passe par l'icône que le
  // site indique lui-même (ou /favicon.ico), jamais par un service tiers.
  let lastSite = '';
  const reportSite = () => {
    if (location.protocol !== 'http:' && location.protocol !== 'https:') return;
    let icon = null;
    for (const l of document.querySelectorAll('link[rel~="icon"]')) {
      if (l.href) { icon = l.href; break; }
    }
    if (!icon) icon = location.origin + '/favicon.ico';
    const meta = (sel) => (document.querySelector(sel) || {}).content;
    const host = location.hostname.replace(/^www\./, '');
    const name = (meta('meta[property="og:site_name"]') || meta('meta[name="application-name"]') || host).trim();
    const key = icon + '|' + name;
    if (key === lastSite) return;
    lastSite = key;
    post({ site: { icon, name } });
  };
  document.addEventListener('DOMContentLoaded', () => {
    reportSite();
    // Icône ajoutée ou changée après coup.
    let timer = 0;
    new MutationObserver(() => { clearTimeout(timer); timer = setTimeout(reportSite, 300); })
      .observe(document.head || document.documentElement, { childList: true, subtree: true, attributes: true, attributeFilter: ['href', 'rel'] });
  });
  window.addEventListener('load', reportSite);

  window.__featherSetMuted = (on) => {
    muted = on;
    for (const m of media()) {
      if (on) silence(m);
      else if (m.__featherMuted) { m.muted = false; m.__featherMuted = false; }
    }
    report();
  };

  // Petit message en bas de la page (niveau de zoom, fin d'un minuteur).
  let toastTimer = 0;
  let toastHost = null;
  window.__featherToast = (text) => {
    if (!toastHost) {
      toastHost = document.createElement('feather-toast');
      const root = toastHost.attachShadow({ mode: 'closed' });
      root.innerHTML = `<style>
        :host { all: initial; }
        div { position: fixed; z-index: 2147483647; left: 50%; bottom: 24px; transform: translateX(-50%);
          padding: 8px 16px; border-radius: 999px; background: rgba(28,28,32,.88); color: #fff;
          font: 13px -apple-system, "Segoe UI", system-ui, sans-serif; pointer-events: none;
          transition: opacity .25s; }
      </style><div></div>`;
      toastHost.box = root.querySelector('div');
    }
    if (!toastHost.isConnected) document.documentElement.appendChild(toastHost);
    toastHost.box.textContent = text;
    toastHost.box.style.opacity = '1';
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toastHost.box.style.opacity = '0'; }, 1600);
  };

  // Recherche dans la page (Ctrl+F). Les occurrences sont repérées dans les nœuds
  // de texte visibles ; la courante est sélectionnée (partout) et toutes sont
  // surlignées quand le moteur connaît l'API « CSS Custom Highlight ».
  const find = { text: '', ranges: [], index: -1 };
  const canHighlight = typeof Highlight === 'function' && window.CSS && CSS.highlights;
  if (canHighlight) {
    const css = document.createElement('style');
    css.textContent = '::highlight(feather-find){background:#ffe36e;color:#000}::highlight(feather-find-current){background:#ff9632;color:#000}';
    // Le script s'exécute parfois avant que le document n'existe.
    const add = () => { const at = document.head || document.documentElement; if (at) at.appendChild(css); };
    add();
    document.addEventListener('DOMContentLoaded', add);
  }
  const collect = (text) => {
    const ranges = [];
    const needle = text.toLocaleLowerCase();
    const walker = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) => {
        const p = n.parentElement;
        if (!p || /^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE|TEXTAREA)$/.test(p.tagName)) return NodeFilter.FILTER_REJECT;
        return n.data.length ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
      },
    });
    for (let n = walker.nextNode(); n && ranges.length < 1000; n = walker.nextNode()) {
      const hay = n.data.toLocaleLowerCase();
      for (let i = hay.indexOf(needle); i !== -1 && ranges.length < 1000; i = hay.indexOf(needle, i + needle.length)) {
        const r = document.createRange();
        r.setStart(n, i);
        r.setEnd(n, i + needle.length);
        // Texte caché (menus repliés, display:none) : ignoré.
        if (r.getClientRects().length) ranges.push(r);
      }
    }
    return ranges;
  };
  const paint = () => {
    if (canHighlight) {
      CSS.highlights.delete('feather-find');
      CSS.highlights.delete('feather-find-current');
      if (find.ranges.length) {
        CSS.highlights.set('feather-find', new Highlight(...find.ranges));
        CSS.highlights.set('feather-find-current', new Highlight(find.ranges[find.index]));
      }
    }
    const sel = getSelection();
    if (find.index >= 0) {
      const r = find.ranges[find.index];
      sel.removeAllRanges();
      sel.addRange(r);
      const box = r.getBoundingClientRect();
      if (box.top < 60 || box.bottom > innerHeight - 60) {
        scrollBy({ top: box.top - innerHeight / 3 });
      }
    }
  };
  window.__featherFind = (text, backwards) => {
    if (!text) {
      if (find.index >= 0) getSelection().removeAllRanges();
      find.text = ''; find.ranges = []; find.index = -1;
      paint();
      return;
    }
    const same = text === find.text;
    find.ranges = collect(text);
    const total = find.ranges.length;
    if (!total) find.index = -1;
    else if (!same) find.index = 0;
    else find.index = (Math.max(find.index, 0) + (backwards ? total - 1 : 1)) % total;
    find.text = text;
    paint();
    post({ find_result: { current: find.index + 1, total } });
  };

  // Poids de l'onglet : retard de la boucle d'événements (la page calcule-t-elle
  // sans arrêt ?) et, quand le moteur le donne, mémoire du JavaScript. Une page
  // cachée est ralentie volontairement par le moteur : on n'y mesure pas le retard.
  let lagSum = 0, lagCount = 0, expected = performance.now() + 200;
  const beat = () => {
    const now = performance.now();
    if (document.visibilityState === 'visible') { lagSum += Math.max(0, now - expected); lagCount++; }
    expected = now + 200;
    setTimeout(beat, 200);
  };
  setTimeout(beat, 200);
  setInterval(() => {
    const visible = document.visibilityState === 'visible';
    const lag = visible && lagCount ? Math.round(lagSum / lagCount) : null;
    lagSum = 0; lagCount = 0;
    const mem = performance.memory && performance.memory.usedJSHeapSize;
    post({ metrics: { lag_ms: lag, heap_mb: mem ? Math.round(mem / 1048576) : null } });
  }, 5000);

  // Texte de la page, pour retrouver une page de l'historique par son contenu.
  // Envoyé une fois, deux secondes après le chargement (Feather décide s'il le garde).
  window.addEventListener('load', () => setTimeout(() => {
    if (location.protocol !== 'http:' && location.protocol !== 'https:') return;
    const body = document.body;
    if (!body) return;
    const text = (body.innerText || '').replace(/\s+/g, ' ').trim().slice(0, 8000);
    if (text.length > 40) post({ text });
  }, 2000));

  // Autorisations : avant que le moteur ne demande caméra, micro, position,
  // notifications ou presse-papiers, Feather pose la question (ou applique la
  // décision retenue pour ce site). La réponse revient par __featherPermissionAnswer.
  // Numéros tirés au hasard : une réponse destinée à la page précédente ne
  // tombe pas sur une question de la nouvelle.
  let askId = Math.floor(Math.random() * 1e9);
  const waiting = new Map();
  const ask = (permission) => new Promise((resolve) => {
    const id = ++askId;
    waiting.set(id, resolve);
    post({ permission: { id, permission } });
  });
  window.__featherPermissionAnswer = (id, allow) => {
    const resolve = waiting.get(id);
    if (resolve) { waiting.delete(id); resolve(allow === true); }
  };
  const denied = () => new DOMException('Autorisation refusée', 'NotAllowedError');
  const askAll = async (list) => {
    for (const p of list) if (!(await ask(p))) return false;
    return true;
  };
  const md = navigator.mediaDevices;
  if (md && md.getUserMedia) {
    const original = md.getUserMedia.bind(md);
    md.getUserMedia = async (c) => {
      const list = [];
      if (c && c.video) list.push('camera');
      if (c && c.audio) list.push('microphone');
      if (!(await askAll(list))) throw denied();
      return original(c);
    };
  }
  if (md && md.getDisplayMedia) {
    const original = md.getDisplayMedia.bind(md);
    md.getDisplayMedia = async (c) => {
      if (!(await ask('screen'))) throw denied();
      return original(c);
    };
  }
  const geo = navigator.geolocation;
  if (geo) {
    const geoError = { code: 1, message: 'Autorisation refusée', PERMISSION_DENIED: 1, POSITION_UNAVAILABLE: 2, TIMEOUT: 3 };
    const get = geo.getCurrentPosition.bind(geo);
    const watch = geo.watchPosition.bind(geo);
    const clear = geo.clearWatch.bind(geo);
    let watchId = 0;
    const watches = new Map();
    geo.getCurrentPosition = (ok, err, opts) => {
      ask('geolocation').then((allow) => allow ? get(ok, err, opts) : err && err(geoError));
    };
    // watchPosition doit rendre un numéro tout de suite : on le relie ensuite au vrai.
    geo.watchPosition = (ok, err, opts) => {
      const id = ++watchId;
      watches.set(id, null);
      ask('geolocation').then((allow) => {
        if (!watches.has(id)) return;
        if (allow) watches.set(id, watch(ok, err, opts));
        else { watches.delete(id); if (err) err(geoError); }
      });
      return id;
    };
    geo.clearWatch = (id) => {
      const real = watches.get(id);
      watches.delete(id);
      if (real != null) clear(real);
    };
  }
  if (window.Notification && Notification.requestPermission) {
    const original = Notification.requestPermission.bind(Notification);
    Notification.requestPermission = (callback) => {
      const done = ask('notifications').then((allow) => allow ? original() : 'denied');
      if (typeof callback === 'function') done.then(callback);
      return done;
    };
  }
  const clip = navigator.clipboard;
  if (clip) {
    for (const name of ['read', 'readText']) {
      if (!clip[name]) continue;
      const original = clip[name].bind(clip);
      clip[name] = async () => {
        if (!(await ask('clipboard'))) throw denied();
        return original();
      };
    }
  }

  // Effet de verre sur le fond de la page : le fond de la page devient transparent
  // et chaque bloc de couleur unie devient translucide. Le texte, les images, les
  // vidéos et les dessins (canvas) ne changent pas. `alpha` : opacité des blocs
  // (0 à 1) ; `null` : on remet la page comme elle était.
  const glass = { alpha: null, style: null, observer: null, timer: 0, touched: new Set() };
  const SKIP = /^(IMG|VIDEO|CANVAS|IFRAME|SVG|PICTURE|OBJECT|EMBED|INPUT|SELECT|TEXTAREA|BUTTON|FEATHER-MENU|FEATHER-TOAST)$/;
  const glassify = (root) => {
    if (glass.alpha == null || !root || root.nodeType !== 1) return;
    const all = [root, ...root.querySelectorAll('*')];
    for (const el of all) {
      if (SKIP.test(el.tagName) || el === document.documentElement || el === document.body) continue;
      if (el.__featherGlass !== undefined) continue;
      const cs = getComputedStyle(el);
      const m = cs.backgroundColor.match(/rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?\)/);
      if (!m) continue;
      const a = m[4] === undefined ? 1 : Number(m[4]);
      // Seuls les fonds pleins deviennent du verre : les fonds déjà translucides
      // (survols, ombres) gardent leur aspect.
      if (a < 0.9) continue;
      el.__featherGlass = el.style.getPropertyValue('background-color');
      el.__featherGlassPriority = el.style.getPropertyPriority('background-color');
      el.style.setProperty('background-color', `rgba(${m[1]}, ${m[2]}, ${m[3]}, ${glass.alpha})`, 'important');
      glass.touched.add(el);
    }
  };
  const unglass = () => {
    for (const el of glass.touched) {
      if (el.__featherGlass) el.style.setProperty('background-color', el.__featherGlass, el.__featherGlassPriority);
      else el.style.removeProperty('background-color');
      delete el.__featherGlass;
      delete el.__featherGlassPriority;
    }
    glass.touched.clear();
  };
  window.__featherGlass = (alpha) => {
    unglass();
    if (glass.observer) { glass.observer.disconnect(); glass.observer = null; }
    if (glass.style) { glass.style.remove(); glass.style = null; }
    glass.alpha = typeof alpha === 'number' ? alpha : null;
    if (glass.alpha == null) return;
    glass.style = document.createElement('style');
    glass.style.textContent = 'html,body{background-color:transparent!important}';
    (document.head || document.documentElement).appendChild(glass.style);
    glassify(document.body);
    // Blocs ajoutés plus tard (menus, articles chargés en défilant), regroupés.
    const pending = new Set();
    glass.observer = new MutationObserver((list) => {
      for (const r of list) for (const n of r.addedNodes) if (n.nodeType === 1) pending.add(n);
      clearTimeout(glass.timer);
      glass.timer = setTimeout(() => {
        for (const n of pending) if (n.isConnected) glassify(n);
        pending.clear();
      }, 150);
    });
    glass.observer.observe(document.documentElement, { childList: true, subtree: true });
  };
  // La page est prête : Feather répond avec son réglage de verre.
  const askGlass = () => post({ glass: true });
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', askGlass);
  else askGlass();
})();
