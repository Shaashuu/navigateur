#!/usr/bin/env bash
# Banc de mesure de la mémoire de Feather (macOS).
#
# Lance Feather deux fois : accueil seul, puis avec cinq sites ouverts, et
# relève l'empreinte mémoire de Feather et de chaque sorte de processus WebKit
# (pages, affichage, réseau). L'empreinte est le chiffre du Moniteur
# d'activité (colonne MEM de `top`) : la mémoire résidente (`ps`) comptait
# aussi les bibliothèques du système partagées entre tous ces processus, une
# fois par processus. À lancer sans autre application WebKit ouverte (Safari,
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
  # shellcheck disable=SC2046
  empreintes $(echo "$liste" | awk '{ print $1 }') | awk -v liste="$liste" '
    BEGIN {
      n = split(liste, lignes, "\n")
      for (i = 1; i <= n; i++) { split(lignes[i], c, " "); sorte[c[1]] = c[2] }
    }
    ($1 in sorte) { ko[sorte[$1]] += $2; total += $2; compte++ }
    END {
      detail = ""
      for (s in ko) detail = detail sprintf("%s%s %d Mo", detail == "" ? "" : ", ", s, ko[s] / 1024)
      printf "%d Mo | %s | %d\n", total / 1024, detail, compte
    }'
}

scenario() {
  local nom=$1; shift
  "$programme" "$@" >/dev/null 2>&1 &
  local pid=$!
  sleep "$attente"
  local resultat
  resultat=$(mesurer "$pid" || echo "? | ? | ?")
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  sleep 3
  echo "| $nom | $resultat |"
}

{
  echo "### Mémoire de Feather (macOS, empreinte après ${attente} s)"
  echo
  echo "| Scénario | Total | Détail | Processus |"
  echo "| --- | ---: | --- | ---: |"
  scenario "Accueil seul"
  scenario "${#sites[@]} sites" "${sites[@]}"
} | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
