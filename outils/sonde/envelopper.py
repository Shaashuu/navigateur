# Sonde : enveloppe l'arrière-plan d'une extension décompressée.
# Usage : python3 envelopper.py <dossier> [compat] [sans-webrequest]
import json, os, shutil, sys

dossier, options = sys.argv[1], set(sys.argv[2:])
ici = os.path.dirname(os.path.abspath(__file__))
for f in ['feather-compat.js', 'sonde-diag.js', 'sonde-page.html', 'sonde-page.js']:
    shutil.copy(os.path.join(ici, f), dossier)
chemin = os.path.join(dossier, 'manifest.json')
m = json.load(open(chemin, encoding='utf-8'))
bg = m['background']
original = bg['service_worker'].lstrip('/')
avant = ['feather-compat.js'] if 'compat' in options else []
drapeau = 'self.__SANS_WEBREQUEST = true;\n' if 'sans-webrequest' in options else ''
open(os.path.join(dossier, 'sonde-drapeaux.js'), 'w').write(drapeau)
if bg.get('type') == 'module':
    lignes = [f"import './{f}';" for f in ['sonde-drapeaux.js', 'sonde-diag.js'] + avant] + [f"import './{original}';"]
    code = '\n'.join(lignes) + '\n'
else:
    liste = ', '.join(repr(f) for f in ['sonde-drapeaux.js', 'sonde-diag.js'] + avant)
    code = f"importScripts({liste});\ntry {{ importScripts({original!r}); self.__note('import ok'); }} catch (e) {{ self.__note('throw ' + e + ' ' + (e && e.stack)); }}\n"
open(os.path.join(dossier, 'sonde-bg.js'), 'w').write(code)
bg['service_worker'] = 'sonde-bg.js'
if 'sans-webrequest' in options:
    m['permissions'] = [p for p in m.get('permissions', []) if p != 'webRequest']
json.dump(m, open(chemin, 'w', encoding='utf-8'), indent=1)
print(dossier, options, '->', code)
