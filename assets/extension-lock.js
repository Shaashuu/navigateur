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
(() => {
  for (const name of ['browser', 'chrome']) {
    const native = globalThis[name];
    if (!native) continue;
    try {
      Object.defineProperty(globalThis, name, { get: () => native, set: () => {}, configurable: true, enumerable: true });
    } catch (e) {}
  }
})();
