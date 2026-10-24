#!/usr/bin/env python3
"""Banc de comparaison : Feather face à Chrome, Firefox, Edge et Safari.

Même méthode pour tous les navigateurs, sur la même machine, l'un après
l'autre (jamais deux en même temps) :

- démarrage : du lancement du programme au chargement complet d'une page
  locale (la page prévient le petit serveur de ce script), une première fois
  avec un profil neuf, puis cinq fois avec le profil déjà créé (médiane) ;
- mémoire : démarrage à vide (page d'accueil propre à chaque navigateur),
  une page locale simple, 5 sites, 10 sites, relevée après un temps fixe ;
- processeur : temps de calcul consommé pendant 60 s sans rien toucher, 5 sites
  ouverts ;
- Speedometer 3.1 (le banc de vitesse de WebKit, Chrome et Firefox) : score
  sur 3 passages, servi en local.

Mémoire : macOS, empreinte de chaque processus (colonne « Mémoire » du
Moniteur d'activité, lue par `top`, comme outils/banc-memoire.sh) ; Windows,
mémoire de travail privée (colonne « Mémoire » du Gestionnaire des tâches) et
mémoire de travail totale (celle de outils/banc-memoire.ps1, qui compte les
pages partagées une fois par processus).

Processus comptés : le programme lancé et tous ses descendants ; sur macOS,
aussi les processus WebKit (pages, réseau, affichage) apparus après le
lancement, qui ne sont pas des enfants de Feather ni de Safari.

Utilisation : banc_navigateurs.py --feather chemin [--sortie resultats.json]
              [--navigateurs feather,feather-nu,safari,chrome,edge,firefox]
"""

import argparse
import http.server
import json
import os
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse

import psutil

MAC = sys.platform == "darwin"
WINDOWS = sys.platform == "win32"

PORT = 8770
LOCAL = f"http://127.0.0.1:{PORT}"

# Les cinq sites du banc de Feather (outils/banc-memoire.sh), puis cinq de plus.
SITES_5 = [
    "https://fr.wikipedia.org/wiki/Rust_(langage)",
    "https://www.lemonde.fr/",
    "https://github.com/Shaashuu/navigateur",
    "https://developer.mozilla.org/fr/",
    "https://www.youtube.com/",
]
SITES_10 = SITES_5 + [
    "https://www.bbc.com/news",
    "https://www.amazon.fr/",
    "https://news.ycombinator.com/",
    "https://stackoverflow.com/questions",
    "https://www.reddit.com/r/rust/",
]

# Temps d'attente avant chaque relevé (secondes).
ATTENTE_VIDE = 30
ATTENTE_SIMPLE = 30
ATTENTE_5 = 45
ATTENTE_10 = 60
FENETRE_CPU = 60
DEMARRAGES = int(os.environ.get("DEMARRAGES", "5"))
SPEEDOMETER_PASSAGES = int(os.environ.get("SPEEDOMETER_PASSAGES", "3"))
SPEEDOMETER_DELAI = 15 * 60


def journal(*morceaux):
    print(time.strftime("%H:%M:%S"), *morceaux, flush=True)


# ---------------------------------------------------------------- serveur local

class Serveur:
    """Sert la page locale et Speedometer, et note l'heure de chaque signal."""

    def __init__(self, dossier):
        self.dossier = dossier
        self.signaux = {}
        self.resultats = {}
        self.verrou = threading.Condition()
        serveur = self

        class Gestion(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *a, **k):
                super().__init__(*a, directory=serveur.dossier, **k)

            def log_message(self, *a):
                pass

            def end_headers(self):
                # Jamais de cache : chaque démarrage recharge vraiment la page.
                self.send_header("Cache-Control", "no-store")
                super().end_headers()

            def do_GET(self):
                url = urllib.parse.urlparse(self.path)
                if url.path == "/signal":
                    cle = urllib.parse.parse_qs(url.query).get("id", ["?"])[0]
                    with serveur.verrou:
                        serveur.signaux.setdefault(cle, time.perf_counter())
                        serveur.verrou.notify_all()
                    self.send_response(204)
                    self.end_headers()
                    return
                super().do_GET()

            def do_POST(self):
                url = urllib.parse.urlparse(self.path)
                taille = int(self.headers.get("Content-Length") or 0)
                corps = self.rfile.read(taille).decode("utf-8", "replace")
                if url.path == "/resultat":
                    cle = urllib.parse.parse_qs(url.query).get("id", ["?"])[0]
                    with serveur.verrou:
                        serveur.resultats[cle] = corps
                        serveur.verrou.notify_all()
                self.send_response(204)
                self.end_headers()

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Gestion)
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def attendre(self, table, cle, delai):
        fin = time.monotonic() + delai
        with self.verrou:
            while cle not in table:
                reste = fin - time.monotonic()
                if reste <= 0:
                    return None
                self.verrou.wait(reste)
            return table[cle]


PAGE_PRETE = """<!doctype html>
<html lang="fr"><head><meta charset="utf-8"><title>Banc</title>
<style>body{font:16px system-ui;margin:3em}p{max-width:40em}</style></head>
<body><h1>Banc de comparaison</h1>
<p>Page locale simple : un titre, un paragraphe et un petit script qui prévient
le banc quand la page a fini de charger.</p>
<script>
addEventListener("load", () => {
  const id = new URLSearchParams(location.search).get("id") || "?";
  fetch("/signal?id=" + encodeURIComponent(id)).catch(() => {});
});
</script></body></html>
"""


def preparer_speedometer(source, dossier):
    """Copie Speedometer et y ajoute l'envoi du score au banc."""
    cible = os.path.join(dossier, "speedometer")
    shutil.copytree(source, cible, ignore=shutil.ignore_patterns(".git", "node_modules", "tests"))
    main = os.path.join(cible, "resources", "main.mjs")
    with open(main, encoding="utf-8") as f:
        code = f.read()
    envoi = (
        '\n        fetch("/resultat?id=" + new URLSearchParams(location.search).get("banc"), '
        '{method: "POST", body: JSON.stringify({score: scoreResults.mean, '
        'passages: this._measuredValuesList.map((v) => v.score)})});'
    )
    repere = 'const scoreResults = this._computeResults(this._measuredValuesList, "score");'
    erreur_repere = "handleError(error) {"
    erreur = (
        '\n        fetch("/resultat?id=" + new URLSearchParams(location.search).get("banc"), '
        '{method: "POST", body: JSON.stringify({erreur: String(error)})});'
    )
    assert repere in code and erreur_repere in code, "Speedometer a changé : repères introuvables"
    code = code.replace(repere, repere + envoi, 1).replace(erreur_repere, erreur_repere + erreur, 1)
    with open(main, "w", encoding="utf-8") as f:
        f.write(code)
    # « banc » n'est pas un paramètre connu de Speedometer : on le retire avant
    # qu'il ne le refuse, après l'avoir lu.
    params = os.path.join(cible, "resources", "params.mjs")
    with open(params, encoding="utf-8") as f:
        code = f.read()
    repere = "this.startAutomatically = searchParams.has(\"startAutomatically\");"
    assert repere in code, "Speedometer a changé : params.mjs"
    code = code.replace(repere, 'searchParams.delete("banc");\n        ' + repere, 1)
    with open(params, "w", encoding="utf-8") as f:
        f.write(code)


# ---------------------------------------------------------------- mesures

def processus_webkit():
    """Numéros des processus WebKit (macOS), Safari compris."""
    trouves = set()
    for p in psutil.process_iter(["pid", "name"]):
        nom = p.info["name"] or ""
        # Le nom peut être coupé à 16 caractères (« com.apple.WebKi »).
        if nom.startswith("com.apple.WebK") or nom in ("Safari", "Safari Web Content", "Safari Networking"):
            trouves.add(p.info["pid"])
    return trouves


def arbre(racine):
    try:
        p = psutil.Process(racine)
        return {racine} | {c.pid for c in p.children(recursive=True)}
    except psutil.Error:
        return set()


def empreintes_mac(pids):
    """Empreinte (Mo) de chaque processus, lue par `top` (Moniteur d'activité)."""
    resultat = {}
    pids = sorted(pids)
    # `top` accepte une liste de -pid ; par paquets pour garder la commande courte.
    for i in range(0, len(pids), 40):
        args = ["top", "-l", "1", "-stats", "pid,mem"]
        for pid in pids[i:i + 40]:
            args += ["-pid", str(pid)]
        sortie = subprocess.run(args, capture_output=True, text=True).stdout
        for ligne in sortie.splitlines():
            c = ligne.split()
            if len(c) >= 2 and c[0].isdigit():
                v = c[1].rstrip("+-")
                m = re.match(r"([\d.]+)([BKMG]?)", v)
                if not m:
                    continue
                n = float(m.group(1))
                n = {"B": n / 1048576, "K": n / 1024, "M": n, "G": n * 1024}.get(m.group(2), n / 1048576)
                resultat[int(c[0])] = n
    return resultat


def memoire(pids):
    """Total en Mo, nombre de processus, détail par nom (Mo)."""
    detail = {}
    total = 0.0
    total_ws = 0.0
    compte = 0
    if MAC:
        valeurs = empreintes_mac(pids)
        for pid, mo in valeurs.items():
            try:
                nom = psutil.Process(pid).name()
            except psutil.Error:
                nom = "?"
            detail[nom] = detail.get(nom, 0) + mo
            total += mo
            compte += 1
        return {"total": round(total), "processus": compte,
                "detail": {k: round(v) for k, v in sorted(detail.items(), key=lambda x: -x[1])}}
    for pid in pids:
        try:
            p = psutil.Process(pid)
            nom = p.name()
            info = p.memory_full_info()
        except psutil.Error:
            continue
        mo = info.uss / 1048576
        detail[nom] = detail.get(nom, 0) + mo
        total += mo
        total_ws += info.rss / 1048576
        compte += 1
    return {"total": round(total), "travail_total": round(total_ws), "processus": compte,
            "detail": {k: round(v) for k, v in sorted(detail.items(), key=lambda x: -x[1])}}


def temps_cpu(pids):
    t = {}
    for pid in pids:
        try:
            c = psutil.Process(pid).cpu_times()
            t[pid] = c.user + c.system
        except psutil.Error:
            pass
    return t


# ---------------------------------------------------------------- navigateurs

class Navigateur:
    nom = "?"
    webkit = False  # processus WebKit à compter en plus de l'arbre (macOS)

    def __init__(self, chemin, travail):
        self.chemin = chemin
        self.travail = travail
        self.profil = os.path.join(travail, "profil-" + self.cle)
        self.proc = None
        self.avant = set()

    @property
    def cle(self):
        return re.sub(r"[^a-z0-9]+", "-", self.nom.lower()).strip("-")

    def neuf(self):
        """Profil neuf (le premier lancement le crée)."""
        shutil.rmtree(self.profil, ignore_errors=True)
        os.makedirs(self.profil, exist_ok=True)

    def commande(self, urls):
        raise NotImplementedError

    def lancer(self, urls):
        self.avant = processus_webkit() if MAC else set()
        t0 = time.perf_counter()
        self.proc = subprocess.Popen(self.commande(urls), stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, env=self.environnement())
        return t0

    def environnement(self):
        return os.environ.copy()

    def racine(self):
        return self.proc.pid if self.proc else None

    def pids(self):
        r = self.racine()
        trouves = arbre(r) if r else set()
        if MAC and self.webkit:
            trouves |= processus_webkit() - self.avant
        return trouves

    def fermer(self):
        pids = self.pids()
        r = self.racine()
        if r:
            if WINDOWS:
                # Comme le bouton de fermeture (WM_CLOSE), puis de force.
                subprocess.run(["taskkill", "/T", "/PID", str(r)], capture_output=True)
            else:
                try:
                    os.kill(r, signal.SIGTERM)
                except OSError:
                    pass
        fin = time.monotonic() + 10
        while time.monotonic() < fin and any(psutil.pid_exists(p) for p in pids):
            time.sleep(0.5)
        for p in pids:
            try:
                psutil.Process(p).kill()
            except psutil.Error:
                pass
        if WINDOWS and r:
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(r)], capture_output=True)
        if self.proc:
            try:
                self.proc.wait(5)
            except subprocess.TimeoutExpired:
                pass
        self.proc = None
        time.sleep(3)

    def version(self):
        return "?"

    def taille(self):
        """Taille sur le disque (Mo) du programme installé."""
        dossier = self.dossier_programme()
        if not dossier or not os.path.exists(dossier):
            return None
        if os.path.isfile(dossier):
            return round(os.path.getsize(dossier) / 1048576, 1)
        total = 0
        for base, _, fichiers in os.walk(dossier):
            for f in fichiers:
                try:
                    total += os.lstat(os.path.join(base, f)).st_size
                except OSError:
                    pass
        return round(total / 1048576, 1)

    def dossier_programme(self):
        if MAC:
            m = re.match(r"(.*?\.app)/", self.chemin)
            return m.group(1) if m else self.chemin
        return os.path.dirname(self.chemin)


class Feather(Navigateur):
    nom = "Feather"
    webkit = MAC
    bloqueurs = True

    def config(self):
        if MAC:
            return os.path.expanduser("~/Library/Application Support/app.Feather.Feather")
        return os.path.join(os.environ["APPDATA"], "Feather", "Feather", "config")

    def neuf(self):
        shutil.rmtree(self.config(), ignore_errors=True)
        if WINDOWS:
            # Données de WebView2 (cache, cookies) de Feather.
            local = os.environ.get("LOCALAPPDATA", "")
            shutil.rmtree(os.path.join(local, "Feather", "Feather"), ignore_errors=True)
        os.makedirs(self.config(), exist_ok=True)
        reglages = {"auto_update_check": False}
        if not self.bloqueurs:
            reglages.update(block_ads=False, block_trackers=False)
        with open(os.path.join(self.config(), "parametres.json"), "w") as f:
            json.dump(reglages, f)

    def commande(self, urls):
        return [self.chemin] + urls

    def version(self):
        return os.environ.get("FEATHER_VERSION", "?")

    def dossier_programme(self):
        if MAC:
            return super().dossier_programme()
        return self.chemin  # un seul fichier .exe


class FeatherNu(Feather):
    """Feather sans son bloqueur de publicités ni de traqueurs (comparaison à armes égales)."""
    nom = "Feather sans bloqueurs"
    bloqueurs = False


class Chromium(Navigateur):
    def commande(self, urls):
        return [self.chemin, f"--user-data-dir={self.profil}", "--no-first-run",
                "--no-default-browser-check", "--disable-session-crashed-bubble",
                "--hide-crash-restore-bubble", "--window-size=1280,900",
                "--window-position=0,0"] + urls

    def version(self):
        if WINDOWS:
            r = subprocess.run(["powershell", "-NoProfile", "-Command",
                                f"(Get-Item '{self.chemin}').VersionInfo.ProductVersion"],
                               capture_output=True, text=True)
            return r.stdout.strip() or "?"
        r = subprocess.run([self.chemin, "--version"], capture_output=True, text=True)
        return r.stdout.strip() or "?"


class Chrome(Chromium):
    nom = "Chrome"


class Edge(Chromium):
    nom = "Edge"


class Firefox(Navigateur):
    nom = "Firefox"

    def neuf(self):
        super().neuf()
        prefs = {
            "browser.shell.checkDefaultBrowser": False,
            "browser.startup.homepage_override.mstone": "ignore",
            "startup.homepage_welcome_url": "",
            "startup.homepage_welcome_url.additional": "",
            "browser.aboutwelcome.enabled": False,
            "trailhead.firstrun.didSeeAboutWelcome": True,
            "datareporting.policy.dataSubmissionPolicyBypassNotification": True,
            "toolkit.telemetry.reportingpolicy.firstRun": False,
            "browser.tabs.warnOnClose": False,
            "browser.warnOnQuit": False,
            "browser.sessionstore.resume_from_crash": False,
            "toolkit.startup.max_resumed_crashes": -1,
            "app.update.auto": False,
            "browser.privacy.segmentation.preferences.show": False,
            "browser.startup.upgradeDialog.enabled": False,
            "browser.tabs.loadInBackground": True,
        }
        with open(os.path.join(self.profil, "user.js"), "w") as f:
            for k, v in prefs.items():
                f.write(f"user_pref({json.dumps(k)}, {json.dumps(v)});\n")

    def commande(self, urls):
        return [self.chemin, "-profile", self.profil, "-no-remote", "-new-instance",
                "-width", "1280", "-height", "900"] + urls

    def environnement(self):
        env = os.environ.copy()
        env["MOZ_CRASHREPORTER_DISABLE"] = "1"
        return env

    def version(self):
        if WINDOWS:
            r = subprocess.run(["powershell", "-NoProfile", "-Command",
                                f"(Get-Item '{self.chemin}').VersionInfo.ProductVersion"],
                               capture_output=True, text=True)
            return r.stdout.strip() or "?"
        r = subprocess.run([self.chemin, "--version"], capture_output=True, text=True)
        return r.stdout.strip() or "?"


class Safari(Navigateur):
    nom = "Safari"
    webkit = True

    def neuf(self):
        self.oublier_fenetres()

    def oublier_fenetres(self):
        for d in ("~/Library/Saved Application State/com.apple.Safari.savedState",
                  "~/Library/Containers/com.apple.Safari/Data/Library/Saved Application State/com.apple.Safari.savedState"):
            shutil.rmtree(os.path.expanduser(d), ignore_errors=True)

    def commande(self, urls):
        # Safari ne peut pas être lancé directement : `open` passe par le système.
        return ["open", "-n", "-a", "Safari"] + urls

    def racine(self):
        for p in psutil.process_iter(["pid", "name"]):
            if p.info["name"] == "Safari":
                return p.info["pid"]
        return None

    def fermer(self):
        super().fermer()
        subprocess.run(["killall", "Safari"], capture_output=True)
        self.oublier_fenetres()

    def version(self):
        r = subprocess.run(["defaults", "read", "/Applications/Safari.app/Contents/Info.plist",
                            "CFBundleShortVersionString"], capture_output=True, text=True)
        return r.stdout.strip() or "?"


def navigateurs_installes(feather, travail, choix):
    if MAC:
        chemins = {
            "chrome": "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "edge": "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "firefox": "/Applications/Firefox.app/Contents/MacOS/firefox",
            "safari": "/Applications/Safari.app/Contents/MacOS/Safari",
        }
    else:
        pf = os.environ.get("ProgramFiles", r"C:\Program Files")
        pf86 = os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")
        chemins = {
            "chrome": next((c for c in (os.path.join(pf, r"Google\Chrome\Application\chrome.exe"),
                                         os.path.join(pf86, r"Google\Chrome\Application\chrome.exe"))
                            if os.path.exists(c)), ""),
            "edge": os.path.join(pf86, r"Microsoft\Edge\Application\msedge.exe"),
            "firefox": os.path.join(pf, r"Mozilla Firefox\firefox.exe"),
        }
    classes = {"chrome": Chrome, "edge": Edge, "firefox": Firefox, "safari": Safari}
    liste = []
    for cle in choix:
        if cle == "feather":
            liste.append(Feather(feather, travail))
        elif cle == "feather-nu":
            liste.append(FeatherNu(feather, travail))
        elif cle in chemins and chemins[cle] and os.path.exists(chemins[cle]):
            liste.append(classes[cle](chemins[cle], travail))
        else:
            journal(f"{cle} absent de cette machine, ignoré")
    return liste


# ---------------------------------------------------------------- scénarios

def scenario_demarrage(nav, serveur):
    """Premier lancement (profil neuf), puis DEMARRAGES lancements (profil existant)."""
    valeurs = []
    premier = None
    nav.neuf()
    for i in range(DEMARRAGES + 1):
        cle = f"{nav.cle}-demarrage-{i}"
        t0 = nav.lancer([f"{LOCAL}/pret.html?id={cle}"])
        t1 = serveur.attendre(serveur.signaux, cle, 60)
        time.sleep(2)
        nav.fermer()
        ms = round((t1 - t0) * 1000) if t1 else None
        journal(f"  {nav.nom} démarrage {i}: {ms} ms")
        if i == 0:
            premier = ms
        elif ms is not None:
            valeurs.append(ms)
    return {"premier": premier, "valeurs": valeurs,
            "mediane": round(statistics.median(valeurs)) if valeurs else None,
            "min": min(valeurs) if valeurs else None}


def scenario_memoire(nav, urls, attente, cpu=False):
    nav.lancer(urls)
    time.sleep(attente)
    resultat = memoire(nav.pids())
    if cpu:
        avant = temps_cpu(nav.pids())
        time.sleep(FENETRE_CPU)
        apres = temps_cpu(nav.pids())
        secondes = sum(max(0.0, t - avant.get(pid, 0.0)) for pid, t in apres.items())
        # Pourcentage d'un cœur pendant la fenêtre.
        resultat["cpu_pourcent"] = round(secondes * 100 / FENETRE_CPU, 1)
        resultat["cpu_secondes"] = round(secondes, 2)
        resultat["apres_cpu"] = memoire(nav.pids())["total"]
    nav.fermer()
    journal(f"  {nav.nom} {len(urls)} adresse(s): {resultat}")
    return resultat


def scenario_speedometer(nav, serveur):
    cle = f"{nav.cle}-speedometer"
    url = (f"{LOCAL}/speedometer/index.html?banc={cle}&startAutomatically"
           f"&iterationCount={SPEEDOMETER_PASSAGES}")
    nav.lancer([url])
    debut = time.monotonic()
    corps = serveur.attendre(serveur.resultats, cle, SPEEDOMETER_DELAI)
    duree = round(time.monotonic() - debut)
    pic = memoire(nav.pids())["total"]
    nav.fermer()
    if corps is None:
        journal(f"  {nav.nom} Speedometer: pas de résultat après {duree} s")
        return {"erreur": f"pas de résultat après {duree} s"}
    donnees = json.loads(corps)
    donnees["duree"] = duree
    donnees["memoire_fin"] = pic
    journal(f"  {nav.nom} Speedometer: {donnees}")
    return donnees


def machine():
    infos = {"systeme": platform.platform(), "processeur": platform.processor() or platform.machine(),
             "coeurs": psutil.cpu_count(), "memoire_go": round(psutil.virtual_memory().total / 2**30, 1)}
    if MAC:
        r = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True)
        infos["processeur"] = r.stdout.strip() or infos["processeur"]
        r = subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True)
        infos["macos"] = r.stdout.strip()
    return infos


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--feather", required=True)
    ap.add_argument("--speedometer", help="dossier source de Speedometer 3.1")
    ap.add_argument("--sortie", default="resultats.json")
    ap.add_argument("--navigateurs", default="feather,feather-nu,safari,chrome,edge,firefox")
    ap.add_argument("--scenarios", default="demarrage,vide,simple,sites5,sites10,speedometer")
    args = ap.parse_args()

    travail = tempfile.mkdtemp(prefix="banc-nav-")
    site = os.path.join(travail, "site")
    os.makedirs(site)
    with open(os.path.join(site, "pret.html"), "w", encoding="utf-8") as f:
        f.write(PAGE_PRETE)
    scenarios = args.scenarios.split(",")
    if "speedometer" in scenarios:
        if args.speedometer:
            preparer_speedometer(args.speedometer, site)
        else:
            scenarios.remove("speedometer")
    serveur = Serveur(site)

    navs = navigateurs_installes(os.path.abspath(args.feather), travail, args.navigateurs.split(","))
    resultats = {"machine": machine(), "navigateurs": {}, "date": time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime())}
    for nav in navs:
        resultats["navigateurs"][nav.nom] = {"version": nav.version(), "taille_mo": nav.taille()}
    journal("Machine :", resultats["machine"])
    journal("Navigateurs :", resultats["navigateurs"])

    def enregistrer():
        with open(args.sortie, "w", encoding="utf-8") as f:
            json.dump(resultats, f, ensure_ascii=False, indent=1)

    # Scénario par scénario, les navigateurs l'un après l'autre : une dérive de
    # la machine ou du réseau touche tout le monde à peu près pareil.
    for sc in scenarios:
        journal(f"== {sc}")
        for nav in navs:
            # « Feather sans bloqueurs » ne change rien hors des vrais sites.
            if isinstance(nav, FeatherNu) and sc not in ("sites5", "sites10"):
                continue
            r = resultats["navigateurs"][nav.nom]
            try:
                if sc == "demarrage":
                    r["demarrage"] = scenario_demarrage(nav, serveur)
                    continue
                nav.neuf()
                # Premier lancement jeté : création du profil, premier démarrage.
                nav.lancer([])
                time.sleep(10)
                nav.fermer()
                if sc == "vide":
                    r["vide"] = scenario_memoire(nav, [], ATTENTE_VIDE)
                elif sc == "simple":
                    r["simple"] = scenario_memoire(nav, [f"{LOCAL}/pret.html?id={nav.cle}-simple"], ATTENTE_SIMPLE)
                elif sc == "sites5":
                    r["sites5"] = scenario_memoire(nav, SITES_5, ATTENTE_5, cpu=True)
                elif sc == "sites10":
                    r["sites10"] = scenario_memoire(nav, SITES_10, ATTENTE_10)
                elif sc == "speedometer":
                    r["speedometer"] = scenario_speedometer(nav, serveur)
            except Exception as e:  # un navigateur en échec n'arrête pas les autres
                journal(f"  {nav.nom} {sc}: erreur {e!r}")
                r[sc] = {"erreur": repr(e)}
                try:
                    nav.fermer()
                except Exception:
                    pass
            enregistrer()
    enregistrer()
    print("=====RESULTATS-DEBUT=====")
    print(json.dumps(resultats, ensure_ascii=False))
    print("=====RESULTATS-FIN=====")


if __name__ == "__main__":
    main()
