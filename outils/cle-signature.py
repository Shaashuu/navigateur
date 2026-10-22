#!/usr/bin/env python3
"""Crée la clé qui signe les mises à jour de Feather (à lancer une seule fois).

    python3 outils/cle-signature.py

Rien à installer : Python 3 de macOS suffit. Le script affiche deux lignes :

- la clé SECRÈTE, à coller dans le secret GitHub `FEATHER_SIGNATURE` du dépôt
  navigateur (Settings → Secrets and variables → Actions), puis à garder dans
  un gestionnaire de mots de passe. Elle ne doit aller nulle part ailleurs ;
- la clé PUBLIQUE, à recopier dans `src/update.rs` (`PUBLIC_KEY`) : elle peut
  être lue par tout le monde.

La clé est une clé Ed25519 (RFC 8032). La clé secrète est sa « graine » de 32
octets en base64, que build.yml transforme en fichier PEM pour `openssl`.
"""

import base64
import hashlib
import secrets

# Courbe Ed25519 (RFC 8032, section 5.1).
P = 2**255 - 19
D = (-121665 * pow(121666, P - 2, P)) % P
GY = 4 * pow(5, P - 2, P) % P


def x_from_y(y, sign):
    xx = (y * y - 1) * pow(D * y * y + 1, P - 2, P) % P
    x = pow(xx, (P + 3) // 8, P)
    if (x * x - xx) % P:
        x = x * pow(2, (P - 1) // 4, P) % P
    if x % 2 != sign:
        x = P - x
    return x


def add(a, b):
    (x1, y1), (x2, y2) = a, b
    t = D * x1 * x2 * y1 * y2 % P
    x3 = (x1 * y2 + x2 * y1) * pow(1 + t, P - 2, P) % P
    y3 = (y1 * y2 + x1 * x2) * pow(1 - t, P - 2, P) % P
    return x3, y3


def mul(k, point):
    result = (0, 1)
    while k:
        if k & 1:
            result = add(result, point)
        point = add(point, point)
        k >>= 1
    return result


def public_key(seed):
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    x, y = mul(a, (x_from_y(GY, 0), GY))
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


if __name__ == "__main__":
    seed = secrets.token_bytes(32)
    print("Clé SECRÈTE (secret GitHub FEATHER_SIGNATURE, à ne montrer à personne) :")
    print("  " + base64.b64encode(seed).decode())
    print()
    print("Clé PUBLIQUE (à recopier dans src/update.rs, peut être partagée) :")
    print("  " + base64.b64encode(public_key(seed)).decode())
