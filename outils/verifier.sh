#!/usr/bin/env bash
# Contrôle rapide avant d'envoyer une modification : vérifie que le code Mac
# et le code Windows compilent, sans attendre GitHub (20 à 40 s au lieu de
# 5 à 10 min). Marche depuis Linux ou macOS : seul l'édition de liens manque,
# donc les tests eux-mêmes restent sur GitHub.
#   bash outils/verifier.sh
set -euo pipefail
cd "$(dirname "$0")/.."

rustup target add aarch64-apple-darwin x86_64-pc-windows-msvc >/dev/null 2>&1 || true
# Une petite partie du code Mac est en Objective-C : clang sait la compiler
# pour macOS, le cc par défaut de Linux non.
export CC_aarch64_apple_darwin="${CC_aarch64_apple_darwin:-clang}"

echo "== macOS =="
cargo check --all-targets --target aarch64-apple-darwin
echo "== Windows =="
cargo check --all-targets --target x86_64-pc-windows-msvc
echo "Code Mac et Windows : compilation vérifiée."
