# Feather

Feather est un navigateur léger écrit en Rust. Il n'embarque pas de moteur : il utilise la
webview du système (WebView2 sur Windows, WebKit sur macOS et Linux) grâce à
`tao` (la fenêtre) et `wry` (les webviews). L'exécutable fait environ 1 Mo.

<img src="packaging/feather-1024.png" width="96" alt="Icône de Feather">

![Accueil](captures/accueil.png)

Autres captures : [commandes](captures/commandes.png), [paramètres](captures/parametres.png), [une page ouverte](captures/page.png), [la croix de fermeture](captures/onglets.png), [le son coupé](captures/son.png), [les icônes des sites](captures/icones.png).

## Ce que fait la version 0.1

- **La page prend tout l'écran**, à côté d'une fine colonne à gauche.
- **Page d'accueil** : l'heure, la date, « Bonjour » avec votre prénom, et la
  barre de recherche au milieu. Tapez une adresse (`wikipedia.org`), des mots
  (recherche avec le moteur choisi), ou `/` pour la liste des commandes :
  `/nouveau`, `/fermer`, `/accueil`, `/retour`, `/suivant`, `/actualiser`, `/parametres`.
- **Pastille de recherche** : dès qu'on quitte l'accueil, la recherche se range
  dans un ovale avec une loupe, au-dessus des onglets. Un clic (ou `Ctrl+L`)
  ouvre la barre par-dessus la page ; `Échap` y revient.
- **En haut à gauche** : l'engrenage des paramètres, puis page précédente,
  page suivante et le bouton maison (son adresse se règle dans les paramètres).
- **Onglets en pastilles à gauche** : la longue pastille est l'onglet actif, un
  rond par autre onglet, `+` pour en ouvrir un. Chaque pastille montre l'icône
  du site (un point gris s'il n'en a pas, ou sur l'accueil). Au survol, le nom
  du site s'affiche
  et une petite croix rouge apparaît en haut de la pastille pour la fermer.
  Maintenez le clic sur un onglet pour le monter ou le descendre.
- **Son** : la pastille d'un onglet qui joue une vidéo ou une musique porte un
  petit haut-parleur (barré quand le son est coupé). Un clic sur la pastille
  change toujours d'onglet ; pour couper ou remettre le son, utilisez le petit
  bouton discret qui apparaît en bas de l'onglet au survol, le clic droit sur
  l'onglet, ou `Ctrl+M`.
- **Paramètres** (engrenage en haut à gauche) : prénom, moteur de recherche
  (DuckDuckGo, Qwant, Ecosia, Google, Bing), adresse du bouton maison (ouverte
  aussi au démarrage ; les nouveaux onglets ouvrent l'accueil Feather), thème clair, sombre
  ou automatique. Ils sont enregistrés dans `parametres.json`, dans le dossier
  de configuration du système (`%APPDATA%\Feather\Feather\config` sur Windows).
- **Favoris, historique, téléchargements** (nouveau en 0.2) : trois boutons sous
  la maison (tout à droite quand la barre est en haut) ouvrent chacun son
  panneau, avec une recherche. Sous macOS et Windows, les panneaux flottent
  par-dessus la page, qui reste visible. L'étoile (dans la barre de recherche, ou dans l'onglet
  actif quand la barre est en haut) ou `Ctrl+D` ajoute la page aux favoris, qui
  s'affichent aussi sur l'accueil. L'historique est rangé par jour et oublié
  après la durée choisie dans les paramètres. Les fichiers téléchargés vont
  dans le dossier Téléchargements, ou dans celui indiqué dans les paramètres.
- **Au démarrage** : ouvrir l'accueil, ou rouvrir les onglets de la dernière fois.
  Historique, favoris et onglets sont enregistrés dans `historique.json`,
  `favoris.json` et `session.json`, à côté de `parametres.json`, et restent sur
  l'ordinateur.
- **Nouveau en 0.3** :
  - **Chercher dans la page** (`Ctrl+F`) : un bandeau en bas de la fenêtre,
    `Entrée` / `Maj+Entrée` pour l'occurrence suivante ou précédente.
  - **Zoom retenu par site** (`Ctrl++`, `Ctrl+-`, `Ctrl+0`), rangé dans `sites.json`.
  - **Rouvrir l'onglet fermé** (`Ctrl+Maj+T`, les dix derniers) et **imprimer** (`Ctrl+P`).
  - **Cadenas** dans la barre : chiffré (https) ou non (http, signalé en rouge
    sur la pastille de recherche et dans l'onglet).
  - **Suggestions** sous la barre : favoris puis historique, au fil de la frappe.
  - **Menu des liens** (clic droit) : nouvel onglet, arrière-plan, onglet privé,
    copier l'adresse. `Maj+clic droit` garde le menu habituel.
  - **Onglets privés** (`Ctrl+Maj+N`, ou `/prive`) : pastille en pointillés,
    rien dans l'historique ni sur le disque ; chaque onglet privé a sa propre
    session, oubliée à sa fermeture.
  - **Téléchargements** : progression, vitesse et bouton Annuler.
  - **Veille des onglets** : un onglet caché depuis longtemps (30 minutes par
    défaut, réglable) libère sa mémoire et se recharge au clic. Au démarrage,
    seuls les onglets affichés sont chargés. Aussi : clic droit → Veille.
  - **Blocage des traqueurs** (macOS et Windows) : une liste courte de domaines
    de mesure d'audience et de publicité, désactivable dans les paramètres.
  - **Autorisations par site** : caméra, micro, position, notifications,
    partage d'écran, presse-papiers. Feather demande dans le bandeau du bas, et
    retient le choix si on le souhaite (liste et « Oublier » dans les paramètres).
  - **Commandes natives** dans la barre : `/calcul (12+3)*7`,
    `/convertir 5 km en miles`, `/minuteur 10 min thé`, `/note acheter du pain`,
    `/notes` (et `/notes effacer`). L'aperçu s'affiche pendant la frappe.
  - **Poids des onglets** : un anneau orange (chargé) ou rouge (lourd) autour de
    la pastille ; la bulle donne la mémoire et le processeur. Un onglet caché et
    lourd s'endort au bout de cinq minutes.
  - **Barre latérale à la largeur voulue** : à gauche ou à droite, on tire sur
    son bord (côté page) pour l'élargir ; le titre des pages apparaît peu à peu
    à côté des pastilles. Double-clic sur le bord : taille d'origine.
  - **Recherche dans le texte des pages** : l'historique retrouve une page par
    un mot de son contenu, avec un extrait (`contenu.json`, désactivable).

Raccourcis : `Ctrl+L` rechercher, `Ctrl+T` nouvel onglet, `Ctrl+W` fermer,
`Ctrl+Tab` / `Ctrl+Maj+Tab` changer d'onglet, `Alt+←` / `Alt+→` page précédente
ou suivante, `F5` ou `Ctrl+R` actualiser, `Ctrl+M` couper le son de l'onglet,
`Ctrl+D` ajouter aux favoris, `Ctrl+,` paramètres, `Ctrl+F` chercher dans la
page, `Ctrl++` / `Ctrl+-` / `Ctrl+0` zoom, `Ctrl+Maj+T` rouvrir l'onglet fermé,
`Ctrl+Maj+N` onglet privé, `Ctrl+P` imprimer.

## Lancer le navigateur

1. Installez Rust : <https://rustup.rs> (sur Windows, acceptez l'installation
   des outils Visual Studio C++ proposée par rustup).
2. Dans ce dossier, lancez :

   ```
   cargo run --release
   ```

   L'exécutable se trouve ensuite dans `target/release/` (`feather.exe` sur Windows).

Selon le système :

- **Windows 10 et 11** : rien d'autre à installer, WebView2 est déjà présent.
- **macOS** : rien d'autre à installer.
- **Linux (Debian, Ubuntu)** : installez d'abord
  `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev`.

Pour lancer les tests : `cargo test`.

## Organisation du code

| Fichier | Rôle |
| --- | --- |
| `src/main.rs` | Création de la fenêtre, boucle d'événements, réaction aux messages. |
| `src/tab_manager.rs` | Les onglets : une webview par onglet, ouverture, changement, fermeture. |
| `src/settings.rs` | Lecture et écriture des paramètres en JSON (serde). |
| `src/ui.rs` | Messages échangés avec l'interface et les pages, placement des zones, page d'accueil. |
| `src/network.rs` | Décide si la saisie est une adresse ou une recherche. |
| `src/library.rs` | Historique, favoris et onglets de la dernière session (fichiers JSON). |
| `src/downloads.rs` | Où ranger les téléchargements, leur progression, les montrer dans le dossier. |
| `src/features.rs` | Zoom, recherche dans la page, suggestions, commandes, autorisations, veille, téléchargements. |
| `src/commands.rs` | Commandes natives de la barre (calcul, conversion, minuteur, notes). |
| `src/fulltext.rs` | Texte des pages visitées, pour chercher dans l'historique. |
| `src/sitedata.rs` | Zoom et autorisations retenus par site (`sites.json`). |
| `src/trackers.rs` | Liste des traqueurs et blocage (WebKit sous macOS, WebView2 sous Windows). |
| `src/weight.rs` | Poids d'un onglet (mémoire, processeur, page ralentie). |
| `src/layers.rs` | Panneaux par-dessus la page : l'interface passe devant, découpée à leur forme. |
| `assets/page.js` | Script injecté dans chaque page : raccourcis, son, recherche, menu des liens, autorisations. |
| `assets/chrome.html` | L'interface (pastilles, barre, commandes, paramètres) en HTML/CSS. |
| `assets/home.html` | La page d'accueil. |

L'interface est une webview placée sous les pages : elle dessine la colonne de
gauche, et la page de l'onglet actif occupe le reste. Quand un panneau s'ouvre
(barre de recherche, paramètres, favoris…), l'interface passe devant la page
(macOS, Windows ; voir `layers.rs`) ou la masque (Linux). Un nouveau panneau
n'a qu'à porter la classe `card` et ouvrir un mode avec `setMode`.
L'interface et les pages parlent au code Rust par des messages JSON ; Rust
renvoie l'état (liste des onglets, paramètres) à l'interface.

**Mémoire** : le gestionnaire d'onglets possède les webviews. Fermer un onglet
le retire de la liste, ce qui détruit sa webview et libère sa mémoire. Il n'y a
ni `Rc` ni `RefCell` : les rappels des webviews ne gardent que le numéro de
leur onglet et envoient des messages.

## Limites connues

- Navigation privée : des onglets privés dans la même fenêtre, pas une fenêtre à part.
- Téléchargements : la progression est mesurée sur le fichier en cours
  d'écriture (le moteur ne la donne pas). « Annuler » ne peut pas arrêter le
  transfert : Feather supprime le fichier tout de suite, puis de nouveau quand
  le moteur a fini. Pas de choix du dossier à chaque fichier, et la liste se
  vide au redémarrage.
- Blocage des traqueurs : pas sous Linux. Sous Windows, les requêtes bloquées
  reçoivent une réponse 403.
- Poids des onglets : mémoire et processeur mesurés par le système sous macOS
  seulement ; ailleurs, mémoire du JavaScript (Windows) et lenteur de la page.
- Autorisations : Feather répond avant le moteur pour les fonctions que la page
  appelle directement ; une demande faite depuis un cadre (iframe) reste
  traitée par le moteur.
- Suggestions sous la barre : seulement avec la barre à gauche ou à droite.
- Panneaux par-dessus la page : pas sous Linux, où la page est cachée pendant
  qu'un panneau est ouvert.
- Icônes : Feather télécharge l'icône que le site indique lui-même (ou
  `/favicon.ico`), jamais via un service tiers, sans envoyer l'adresse de la
  page d'origine. Elle n'est gardée que pour les onglets ouverts.
- Sur Linux, seul X11 a été essayé (pas Wayland).
- L'icône de son repère les balises vidéo et audio de la page, pas les sons
  produits par Web Audio ni ceux des vidéos intégrées dans un cadre (iframe).

## Télécharger sans installer Rust

Le dépôt GitHub construit automatiquement un `.exe` (Windows) et un `.dmg`
(macOS, Apple Silicon et Intel) : onglet **Actions** puis la dernière
exécution, section *Artifacts*. Les versions étiquetées (`v0.1.0`, etc.) sont
publiées dans l'onglet **Releases**.

Les programmes ne sont pas signés : Windows peut afficher un avertissement
SmartScreen (« Informations complémentaires » puis « Exécuter quand même »), et
sur macOS il faut faire clic droit sur l'application puis « Ouvrir » la première fois.

## Mise à jour intégrée

Dans les paramètres, la section « Mise à jour » montre la version actuelle et,
s'il y en a une, la nouvelle. Feather cherche au démarrage (désactivable) la
dernière release du dépôt public `Shaashuu/feather-releases`. Le bouton
« Télécharger » récupère le fichier de votre système, vérifie son empreinte
SHA-256, puis remplace le programme ; le bouton devient alors
« Redémarrer pour mettre à jour ». Rien ne change tant que vous n'avez pas cliqué.

Le workflow `build.yml` publie la release dans ce dépôt si le secret
`RELEASES_TOKEN` (jeton avec accès en écriture au contenu de `feather-releases`)
existe dans le dépôt du code. Les fichiers ne sont pas signés : la mise à jour
automatique n'enlève pas l'avertissement de Windows ou de macOS à la première installation.
