# Navigateur 0.1

Un navigateur léger écrit en Rust. Il n'embarque pas de moteur : il utilise la
webview du système (WebView2 sur Windows, WebKit sur macOS et Linux) grâce à
`tao` (la fenêtre) et `wry` (les webviews). L'exécutable fait environ 1 Mo.

![Accueil](captures/accueil.png)

Autres captures : [commandes](captures/commandes.png), [paramètres](captures/parametres.png), [une page ouverte](captures/page.png).

## Ce que fait la version 0.1

- **La page prend tout l'écran.** Pas de barre en haut.
- **Onglets en pastilles à gauche** : la longue pastille est l'onglet actif, un
  point par autre onglet, `+` pour en ouvrir un. Survolez une pastille pour voir
  son titre dans la barre du bas. Clic du milieu sur une pastille pour la fermer.
- **Une seule barre flottante en bas** pour l'adresse, la recherche et les
  commandes. Tapez une adresse (`wikipedia.org`) ou des mots (recherche avec le
  moteur choisi), ou `/` pour la liste des commandes :
  `/nouveau`, `/fermer`, `/accueil`, `/retour`, `/suivant`, `/actualiser`, `/parametres`.
- **Page d'accueil calme** : l'heure, la date et « Bonjour » avec votre prénom.
- **Paramètres** (engrenage ou `/parametres`) : prénom, moteur de recherche
  (DuckDuckGo, Qwant, Ecosia, Google, Bing), page d'accueil, thème clair, sombre
  ou automatique. Ils sont enregistrés dans `parametres.json`, dans le dossier
  de configuration du système (`%APPDATA%\Navigateur\Navigateur\config` sur Windows).

Raccourcis : `Ctrl+L` aller à la barre, `Ctrl+T` nouvel onglet, `Ctrl+W` fermer,
`Ctrl+Tab` / `Ctrl+Maj+Tab` changer d'onglet, `Ctrl+,` paramètres, `Échap` quitter la barre.

## Lancer le navigateur

1. Installez Rust : <https://rustup.rs> (sur Windows, acceptez l'installation
   des outils Visual Studio C++ proposée par rustup).
2. Dans ce dossier, lancez :

   ```
   cargo run --release
   ```

   L'exécutable se trouve ensuite dans `target/release/` (`navigateur.exe` sur Windows).

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
| `src/ui.rs` | Messages échangés avec l'interface, placement des zones, page d'accueil. |
| `src/network.rs` | Décide si la saisie est une adresse ou une recherche. |
| `assets/chrome.html` | L'interface (pastilles, barre, commandes, paramètres) en HTML/CSS. |
| `assets/home.html` | La page d'accueil. |

L'interface est une webview placée sous les pages : elle dessine la colonne de
gauche et la bande du bas, et la page de l'onglet actif occupe le reste.
L'interface et les pages parlent au code Rust par des messages JSON ; Rust
renvoie l'état (liste des onglets, paramètres) à l'interface.

**Mémoire** : le gestionnaire d'onglets possède les webviews. Fermer un onglet
le retire de la liste, ce qui détruit sa webview et libère sa mémoire. Il n'y a
ni `Rc` ni `RefCell` : les rappels des webviews ne gardent que le numéro de
leur onglet et envoient des messages.

## Limites connues de la 0.1

- Pas d'historique, de favoris, de téléchargements gérés ni de navigation privée.
- Les onglets ne sont pas restaurés au redémarrage.
- Sur Linux, seul X11 a été essayé (pas Wayland).

## Télécharger sans installer Rust

Le dépôt GitHub construit automatiquement un `.exe` (Windows) et un `.dmg`
(macOS, Apple Silicon et Intel) : onglet **Actions** puis la dernière
exécution, section *Artifacts*. Les versions étiquetées (`v0.1.0`, etc.) sont
publiées dans l'onglet **Releases**.

Les programmes ne sont pas signés : Windows peut afficher un avertissement
SmartScreen (« Informations complémentaires » puis « Exécuter quand même »), et
sur macOS il faut faire clic droit sur l'application puis « Ouvrir » la première fois.
