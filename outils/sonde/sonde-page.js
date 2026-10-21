const api = globalThis.browser || globalThis.chrome;
api.storage.local.get('__bgerr').then((r) => { document.getElementById('out').textContent = 'BG:\n' + (r.__bgerr || '(rien)'); },
  (e) => { document.getElementById('out').textContent = 'stockage illisible : ' + e; });
