# Écrit un contenu.json plein (2 000 pages de 4 000 caractères, avec accents),
# comme après des semaines de navigation. Utilisation : python remplir-index.py chemin
import json, random, sys, time
mots = ("navigateur léger mémoire été année élève forêt château fenêtre côté "
        "réseau sécurité numérique développeur journée première théâtre "
        "rapide page site onglet recherche texte article données").split()
random.seed(7)
now = int(time.time())
pages = []
for i in range(2000):
    texte = []
    taille = 0
    while taille < 4000:
        m = random.choice(mots)
        texte.append(m)
        taille += len(m) + 1
    pages.append({"url": f"https://exemple{i}.fr/article", "title": f"Article {i} · été",
                  "text": " ".join(texte)[:4000], "time": now - i * 60})
with open(sys.argv[1], "w", encoding="utf-8") as f:
    json.dump(pages, f, ensure_ascii=False)
