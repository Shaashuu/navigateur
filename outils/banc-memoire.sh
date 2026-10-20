#!/usr/bin/env bash
# Banc de mesure de la mémoire de Feather (macOS).
#
# Lance Feather deux fois : accueil seul, puis avec cinq sites ouverts, et
# additionne la mémoire résidente de Feather et des processus WebKit (pages,
# réseau, affichage). À lancer sans autre application WebKit ouverte (Safari,
# Mail…), qui partagerait ces processus et fausserait le total.
#
# Utilisation : outils/banc-memoire.sh chemin/vers/feather [secondes d'attente]
set -euo pipefail

programme=${1:?"chemin du programme Feather"}
attente=${2:-30}
sites=(
  "https://fr.wikipedia.org/wiki/Rust_(langage)"
  "https://www.lemonde.fr/"
  "https://github.com/Shaashuu/navigateur"
  "https://developer.mozilla.org/fr/"
  "https://www.youtube.com/"
)

# Mémoire (Mo) et nombre de processus : Feather et les processus WebKit.
mesurer() {
  local pid=$1
  ps -axo pid=,rss=,comm= | awk -v pid="$pid" '
    $1 == pid || $3 ~ /com\.apple\.WebKit/ { total += $2; n++ }
    END { printf "%d Mo|%d\n", total / 1024, n }'
}

scenario() {
  local nom=$1; shift
  "$programme" "$@" >/dev/null 2>&1 &
  local pid=$!
  sleep "$attente"
  local resultat
  resultat=$(mesurer "$pid")
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  sleep 3
  echo "| $nom | ${resultat%%|*} | ${resultat##*|} |"
}

{
  echo "### Mémoire de Feather (macOS, après ${attente} s)"
  echo
  echo "| Scénario | Mémoire totale | Processus |"
  echo "| --- | ---: | ---: |"
  scenario "Accueil seul"
  scenario "${#sites[@]} sites" "${sites[@]}"
} | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
