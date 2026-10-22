#!/usr/bin/env bash
# Banc de mesure de Feather (macOS) : mémoire, temps de démarrage, extensions.
#
# Lance Feather deux fois : accueil seul, puis avec cinq sites ouverts, et
# relève l'empreinte mémoire de Feather et de chaque sorte de processus WebKit
# (pages, affichage, réseau). L'empreinte est le chiffre du Moniteur
# d'activité (colonne MEM de `top`) : la mémoire résidente (`ps`) comptait
# aussi les bibliothèques du système partagées entre tous ces processus, une
# fois par processus. À lancer sans autre application WebKit ouverte (Safari,
# Mail…), qui partagerait ces processus et fausserait le total.
#
# Le temps de démarrage est écrit par Feather lui-même (mode banc, voir
# src/banc.rs). Chaque chiffre est comparé à outils/banc-reference.txt : un
# écart au-delà de la marge donne une alerte dans la compilation, jamais un échec.
#
# Avec --extensions, un troisième lancement installe Proton Pass et uBlock
# Origin Lite, puis vérifie que leur page s'affiche sans message d'erreur ; un
# quatrième relance Feather avec ces extensions (mémoire et démarrage).
# Réservé aux machines de compilation : elles restent installées ensuite.
#
# Utilisation : outils/banc-memoire.sh [--extensions] chemin/vers/feather [secondes d'attente]
set -euo pipefail

extensions=0
if [ "${1:-}" = "--extensions" ]; then extensions=1; shift; fi
programme=${1:?"chemin du programme Feather"}
attente=${2:-30}
dossier=$(cd "$(dirname "$0")" && pwd)
temp=$(mktemp -d)
# Alertes à afficher dans la compilation, après le tableau.
alertes="$temp/alertes"
: > "$alertes"
# Extensions testées : identifiant du Chrome Web Store et nom.
extensions_testees=(
  "ghmbeldphafepmbegfdlkpapadhbakde Proton Pass"
  "ddkjiahejlhfcafbddmgiahcphecmpfh uBlock Origin Lite"
)
sites=(
  "https://fr.wikipedia.org/wiki/Rust_(langage)"
  "https://www.lemonde.fr/"
  "https://github.com/Shaashuu/navigateur"
  "https://developer.mozilla.org/fr/"
  "https://www.youtube.com/"
)

# Processus mesurés, un par ligne : « numéro sorte ». La sorte vient du nom du
# programme : Feather, ou WebContent, GPU, Networking pour WebKit.
processus() {
  local pid=$1
  ps -axo pid=,comm= | awk -v pid="$pid" '
    $1 == pid { print $1, "Feather"; next }
    $2 ~ /com\.apple\.WebKit\./ { n = split($2, part, "."); print $1, part[n] }'
}

# Empreinte (Ko) de chaque processus, une ligne « numéro Ko », lue par `top`
# (qui la donne pour tous les processus de l'utilisateur, WebKit compris).
empreintes() {
  local args=()
  local pid
  for pid in "$@"; do args+=(-pid "$pid"); done
  top -l 1 -stats pid,mem "${args[@]}" 2>/dev/null | awk '
    $1 ~ /^[0-9]+$/ {
      v = $2; sub(/[+-]$/, "", v)
      unit = substr(v, length(v)); n = substr(v, 1, length(v) - 1) + 0
      if (unit == "G") n *= 1048576; else if (unit == "M") n *= 1024
      else if (unit == "B") n /= 1024; else if (unit != "K") n = v + 0
      printf "%s %d\n", $1, n
    }'
}

# Ligne du tableau : total, détail par sorte et nombre de processus.
mesurer() {
  local liste
  liste=$(processus "$1")
  [ -n "$liste" ] || { echo "? | ? | 0"; return; }
  # La liste passe par l'environnement : l'awk de macOS refuse un saut de
  # ligne dans une valeur donnée par -v (« newline in string »).
  # shellcheck disable=SC2046
  empreintes $(echo "$liste" | awk '{ print $1 }') | LISTE="$liste" awk '
    BEGIN {
      n = split(ENVIRON["LISTE"], lignes, "\n")
      for (i = 1; i <= n; i++) { split(lignes[i], c, " "); sorte[c[1]] = c[2] }
    }
    ($1 in sorte) { ko[sorte[$1]] += $2; total += $2; compte++ }
    END {
      detail = ""
      for (s in ko) detail = detail sprintf("%s%s %d Mo", detail == "" ? "" : ", ", s, ko[s] / 1024)
      printf "%d Mo | %s | %d\n", total / 1024, detail, compte
    }'
}

# Valeur d'une ligne « clé valeur » du fichier écrit par Feather (vide sinon).
lu() {
  awk -v cle="$2" '$1 == cle { print $2; exit }' "$1" 2>/dev/null || true
}

# Compare un chiffre à la référence ; une alerte si l'écart dépasse la marge.
comparer() {
  local cle=$1 valeur=$2 unite=$3 libelle=$4
  local ref marge
  ref=$(awk -v cle="$cle" '$1 == cle { print $2; exit }' "$dossier/banc-reference.txt")
  marge=$(awk -v cle="$cle" '$1 == cle { print $3; exit }' "$dossier/banc-reference.txt")
  case "$valeur" in ''|*[!0-9]*) return ;; esac
  case "$ref" in ''|*[!0-9]*) return ;; esac
  awk -v v="$valeur" -v r="$ref" -v m="${marge:-25}" -v u="$unite" -v l="$libelle" 'BEGIN {
    if (r > 0 && (ecart = (v - r) * 100 / r) > m) printf "%s : %d %s, référence %d %s (+%d %%, marge %d %%)\n", l, v, u, r, u, ecart, m
  }' >> "$alertes"
}

fermer() {
  kill "$1" 2>/dev/null || true
  wait "$1" 2>/dev/null || true
  sleep 3
}

scenario() {
  local nom=$1 cle=$2; shift 2
  local banc="$temp/$cle.txt"
  # Mode banc (src/banc.rs) : Feather écrit son temps de démarrage dans $banc.
  FEATHER_BANC=$banc "$programme" "$@" >/dev/null 2>&1 &
  local pid=$!
  sleep "$attente"
  local resultat
  resultat=$(mesurer "$pid" || echo "? | ? | ?")
  fermer "$pid"
  echo "| $nom | $resultat |"
  comparer "macos-$cle" "${resultat%% Mo*}" Mo "macOS, $nom"
}

# Troisième lancement : les extensions s'installent, se chargent, et leur page
# est lue par Feather (lignes « extension … » et « fenetre … » du fichier).
scenario_extensions() {
  local banc="$temp/extensions.txt"
  local ids="" e
  for e in "${extensions_testees[@]}"; do ids="$ids${ids:+,}${e%% *}"; done
  FEATHER_BANC_EXTENSIONS=$ids FEATHER_BANC=$banc "$programme" >/dev/null 2>&1 &
  local pid=$!
  # Jusqu'à 3 minutes : téléchargement, chargement, 30 s avant la lecture de la page.
  local debut=$SECONDS fini
  while [ $((SECONDS - debut)) -lt 180 ]; do
    sleep 5
    grep -q '^extensions indisponibles' "$banc" 2>/dev/null && break
    fini=1
    for e in "${extensions_testees[@]}"; do
      grep -Eq "^(fenetre ${e%% *} |extension ${e%% *} erreur)" "$banc" 2>/dev/null || fini=0
    done
    [ "$fini" = 1 ] && break
  done
  fermer "$pid"
  echo
  echo "| Extension | Installée | Chargée | Page | Texte de la page |"
  echo "| --- | :---: | :---: | :---: | --- |"
  local indispo
  indispo=$(sed -n 's/^extensions indisponibles //p' "$banc" 2>/dev/null | head -1)
  for e in "${extensions_testees[@]}"; do
    local id=${e%% *} nom=${e#* }
    if [ -n "$indispo" ]; then
      echo "| $nom | — | — | — | non testée : $indispo |"
      continue
    fi
    local installee=non chargee=non page="?" texte="" erreur
    grep -q "^extension $id preparee" "$banc" 2>/dev/null && installee=oui
    grep -q "^extension $id chargee" "$banc" 2>/dev/null && chargee=oui
    erreur=$(sed -n "s/^extension $id erreur //p" "$banc" 2>/dev/null | head -1)
    local ligne
    ligne=$(grep "^fenetre $id " "$banc" 2>/dev/null | head -1 || true)
    if [ -n "$ligne" ]; then
      page=$(echo "$ligne" | awk '{ print $3 }')
      texte=$(echo "$ligne" | cut -d' ' -f4- | tr '|' '/' | cut -c1-100)
    fi
    [ -n "$erreur" ] && texte="erreur : $(echo "$erreur" | tr '|' '/')"
    echo "| $nom | $installee | $chargee | $page | $texte |"
    if [ "$chargee" != oui ] || [ "$page" != ok ]; then
      echo "Extension $nom (macOS) : installée $installee, chargée $chargee, page $page. ${texte}" >> "$alertes"
    fi
  done
  # Relance avec les extensions déjà installées : c'est le démarrage de tous
  # les jours pour qui en a (piste 8 : elles ne doivent pas le retarder).
  if [ -n "$indispo" ]; then return; fi
  local relance="$temp/relance.txt" memoire
  FEATHER_BANC=$relance "$programme" >/dev/null 2>&1 &
  pid=$!
  sleep "$attente"
  memoire=$(mesurer "$pid" || echo "? | ? | ?")
  fermer "$pid"
  echo
  echo "Relance avec les deux extensions installées : ${memoire%% |*} après ${attente} s ; $(etapes "$relance")."
  comparer macos-relance "${memoire%% Mo*}" Mo "macOS, relance avec extensions"
  comparer macos-demarrage-extensions "$(lu "$relance" demarrage)" ms "macOS, démarrage avec extensions"
}

# Les étapes du démarrage écrites par Feather, en une phrase.
etapes() {
  echo "Rust prêt en $(lu "$1" rust || true) ms, barre prête en $(lu "$1" interface) ms, accueil chargé en $(lu "$1" demarrage) ms" | sed 's/en  ms/en ? ms/g'
}

{
  echo "### Banc de Feather (macOS, empreinte après ${attente} s)"
  echo
  echo "| Scénario | Total | Détail | Processus |"
  echo "| --- | ---: | --- | ---: |"
  scenario "Accueil seul" accueil
  scenario "${#sites[@]} sites" sites "${sites[@]}"
  echo
  echo "Démarrage : $(etapes "$temp/accueil.txt")."
  comparer macos-demarrage "$(lu "$temp/accueil.txt" demarrage)" ms "macOS, démarrage"
  if [ "$extensions" = 1 ]; then scenario_extensions; fi
  if [ -s "$alertes" ]; then
    echo
    echo "**Alertes**"
    echo
    sed 's/^/- /' "$alertes"
  fi
} | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"

# Annotations de la compilation : visibles sans ouvrir le résumé, jamais bloquantes.
while IFS= read -r ligne; do
  echo "::warning title=Banc de Feather::$ligne"
done < "$alertes"
rm -rf "$temp"
