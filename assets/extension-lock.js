// Verrou de Feather pour les extensions, sur Mac : `browser` et `chrome`
// restent les objets de WebKit. Chargé avant tout le reste dans l'arrière-plan,
// dans chaque page de l'extension et devant ses scripts de contenu.
//
// À chaque évènement (runtime.onMessage, onConnect, tabs, storage…), WebKit
// retrouve l'extension en relisant `browser`, sinon `chrome`, sur l'objet
// global de l'arrière-plan et de chaque page. Proton Pass (version Chrome) les
// remplace juste après son démarrage par un mandataire (Proxy), pour les cacher
// à la console de développement : WebKit ne reconnaissait plus l'extension,
// aucun message n'arrivait à son arrière-plan, et sa fenêtre affichait
// « difficulties activating » puis se relançait en boucle. Le remplacement est
// désormais ignoré ; l'extension garde les références qu'elle a prises avant,
// rien ne change pour elle.
//
// S'y ajoute requestIdleCallback, que WebKit n'a pas (Chrome l'a) : la version
// Chrome de Proton Pass attend ce rappel pour analyser la page, et ne le
// remplace que dans sa version Safari. Sans lui, l'analyse ne finissait jamais :
// aucune icône ni aucun menu dans les champs des pages de connexion. Posé ici,
// il reste dans le monde de l'extension, la page ne le voit pas.
(() => {
  if (typeof globalThis.requestIdleCallback !== 'function') {
    // Comme Proton Pass dans Safari : le rappel au prochain tour, avec 50 ms de marge.
    globalThis.requestIdleCallback = (callback) => {
      const start = Date.now();
      return setTimeout(() => callback({ didTimeout: false, timeRemaining: () => Math.max(0, 50 - (Date.now() - start)) }), 1);
    };
    globalThis.cancelIdleCallback = (id) => clearTimeout(id);
  }

  for (const name of ['browser', 'chrome']) {
    const native = globalThis[name];
    if (!native) continue;
    try {
      Object.defineProperty(globalThis, name, { get: () => native, set: () => {}, configurable: true, enumerable: true });
    } catch (e) {}
  }
})();
