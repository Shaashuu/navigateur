# Banc de mesure de Feather (Windows) : mémoire, temps de démarrage, extensions.
#
# Lance Feather deux fois : accueil seul, puis avec cinq sites ouverts, et
# additionne la mémoire de Feather et de tous ses processus enfants
# (msedgewebview2 : pages, réseau, affichage).
#
# Le temps de démarrage est écrit par Feather lui-même (mode banc, voir
# src/banc.rs). Chaque chiffre est comparé à outils\banc-reference.txt : un
# écart au-delà de la marge donne une alerte dans la compilation, jamais un échec.
#
# Avec -Extensions, un troisième lancement installe Proton Pass et uBlock
# Origin Lite, puis vérifie que leur page s'affiche sans message d'erreur ; un
# quatrième relance Feather avec ces extensions (mémoire et démarrage).
# Réservé aux machines de compilation : elles restent installées ensuite.
#
# Utilisation : outils\banc-memoire.ps1 -Programme chemin\vers\feather.exe [-Attente 30] [-Extensions]
param(
    [Parameter(Mandatory = $true)][string]$Programme,
    [int]$Attente = 30,
    [switch]$Extensions
)
$ErrorActionPreference = 'Stop'

$sites = @(
    'https://fr.wikipedia.org/wiki/Rust_(langage)',
    'https://www.lemonde.fr/',
    'https://github.com/Shaashuu/navigateur',
    'https://developer.mozilla.org/fr/',
    'https://www.youtube.com/'
)
# Extensions testées : identifiant du Chrome Web Store et nom.
$extensionsTestees = [ordered]@{
    'ghmbeldphafepmbegfdlkpapadhbakde' = 'Proton Pass'
    'ddkjiahejlhfcafbddmgiahcphecmpfh' = 'uBlock Origin Lite'
}
$temp = Join-Path ([System.IO.Path]::GetTempPath()) "banc-feather-$PID"
New-Item -ItemType Directory -Force -Path $temp | Out-Null
# Alertes à afficher dans la compilation, après le tableau.
$alertes = [System.Collections.Generic.List[string]]::new()

# Référence : « clé valeur marge » par ligne (« - » : pas encore de référence).
$reference = @{}
foreach ($ligne in Get-Content (Join-Path $PSScriptRoot 'banc-reference.txt')) {
    $c = -split $ligne
    if ($c.Count -ge 3 -and -not $c[0].StartsWith('#') -and $c[1] -match '^\d+$') {
        $reference[$c[0]] = @([double]$c[1], [double]$c[2])
    }
}

function Comparer([string]$Cle, $Valeur, [string]$Unite, [string]$Libelle) {
    if ($null -eq $Valeur -or -not $reference.ContainsKey($Cle)) { return }
    $ref, $marge = $reference[$Cle]
    if ($ref -le 0) { return }
    $ecart = ($Valeur - $ref) * 100 / $ref
    if ($ecart -gt $marge) {
        $alertes.Add("$Libelle : $Valeur $Unite, référence $ref $Unite (+$([math]::Round($ecart)) %, marge $marge %)")
    }
}

# Valeur d'une ligne « clé valeur » du fichier écrit par Feather.
function Lire([string]$Fichier, [string]$Cle) {
    if (-not (Test-Path $Fichier)) { return $null }
    foreach ($l in Get-Content $Fichier) {
        $c = -split $l
        if ($c.Count -ge 2 -and $c[0] -eq $Cle) { return [int]$c[1] }
    }
    $null
}

# Feather et tous ses descendants.
function Get-Arbre([int]$Racine) {
    # Délai : WMI peut rester bloqué sur les machines de compilation.
    $tous = Get-CimInstance Win32_Process -OperationTimeoutSec 30 | Select-Object ProcessId, ParentProcessId, WorkingSetSize
    $ids = @{ $Racine = $true }
    do {
        $ajoutes = 0
        foreach ($p in $tous) {
            if (-not $ids.ContainsKey([int]$p.ProcessId) -and $ids.ContainsKey([int]$p.ParentProcessId)) {
                $ids[[int]$p.ProcessId] = $true
                $ajoutes++
            }
        }
    } while ($ajoutes -gt 0)
    $tous | Where-Object { $ids.ContainsKey([int]$_.ProcessId) }
}

# Lance Feather en mode banc (src/banc.rs) : il écrit dans $Banc.
function Lancer([string]$Banc, [string[]]$Adresses, [string]$Ids = '') {
    $env:FEATHER_BANC = $Banc
    $env:FEATHER_BANC_EXTENSIONS = $Ids
    try {
        if ($Adresses) { Start-Process -FilePath $Programme -ArgumentList $Adresses -PassThru }
        else { Start-Process -FilePath $Programme -PassThru }
    } finally {
        Remove-Item Env:FEATHER_BANC, Env:FEATHER_BANC_EXTENSIONS -ErrorAction SilentlyContinue
    }
}

# Mémoire totale (Mo) et nombre de processus, puis fermeture.
function Mesurer-Fermer($p) {
    $arbre = @(Get-Arbre $p.Id)
    $mo = [math]::Round((($arbre | Measure-Object -Property WorkingSetSize -Sum).Sum) / 1MB)
    foreach ($q in $arbre) { Stop-Process -Id $q.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 3
    @($mo, $arbre.Count)
}

function Scenario([string]$Nom, [string]$Cle, [string[]]$Adresses) {
    $p = Lancer (Join-Path $temp "$Cle.txt") $Adresses
    Start-Sleep -Seconds $Attente
    $mo, $nombre = Mesurer-Fermer $p
    Comparer "windows-$Cle" $mo 'Mo' "Windows, $Nom"
    "| $Nom | $mo Mo | $nombre |"
}

# Troisième lancement : les extensions s'installent, se chargent, et leur page
# est lue par Feather (lignes « extension … » et « fenetre … » du fichier).
function Scenario-Extensions {
    $banc = Join-Path $temp 'extensions.txt'
    $p = Lancer $banc @() ($extensionsTestees.Keys -join ',')
    # Jusqu'à 3 minutes : téléchargement, chargement, 30 s avant la lecture de la page.
    $fin = (Get-Date).AddMinutes(3)
    while ((Get-Date) -lt $fin) {
        Start-Sleep -Seconds 5
        $lignes = if (Test-Path $banc) { @(Get-Content $banc) } else { @() }
        if ($lignes | Where-Object { $_ -like 'extensions indisponibles*' }) { break }
        $finies = @($extensionsTestees.Keys | Where-Object {
                $id = $_
                $lignes | Where-Object { $_ -like "fenetre $id *" -or $_ -like "extension $id erreur*" }
            })
        if ($finies.Count -eq $extensionsTestees.Count) { break }
    }
    $null = Mesurer-Fermer $p
    $lignes = if (Test-Path $banc) { @(Get-Content $banc) } else { @() }
    $sortie = @('', '| Extension | Installée | Chargée | Page | Texte de la page |', '| --- | :---: | :---: | :---: | --- |')
    $indispo = $lignes | Where-Object { $_ -like 'extensions indisponibles*' } | Select-Object -First 1
    foreach ($id in $extensionsTestees.Keys) {
        $nom = $extensionsTestees[$id]
        if ($indispo) { $sortie += "| $nom | — | — | — | non testée : $($indispo -replace '^extensions indisponibles\s*', '') |"; continue }
        $installee = if ($lignes -contains "extension $id preparee") { 'oui' } else { 'non' }
        $chargee = if ($lignes -contains "extension $id chargee") { 'oui' } else { 'non' }
        $page = '?'; $texte = ''
        $fenetre = $lignes | Where-Object { $_ -like "fenetre $id *" } | Select-Object -First 1
        if ($fenetre) {
            $c = $fenetre -split ' ', 4
            $page = $c[2]
            if ($c.Count -ge 4) { $texte = $c[3].Replace('|', '/') }
            if ($texte.Length -gt 100) { $texte = $texte.Substring(0, 100) }
        }
        $erreur = $lignes | Where-Object { $_ -like "extension $id erreur*" } | Select-Object -First 1
        if ($erreur) { $texte = 'erreur : ' + ($erreur -replace "^extension $id erreur\s*", '').Replace('|', '/') }
        $sortie += "| $nom | $installee | $chargee | $page | $texte |"
        if ($chargee -ne 'oui' -or $page -ne 'ok') {
            $alertes.Add("Extension $nom (Windows) : installée $installee, chargée $chargee, page $page. $texte")
        }
    }
    if ($indispo) { return $sortie }
    # Relance avec les extensions déjà installées : c'est le démarrage de tous
    # les jours pour qui en a (piste 8 : elles ne doivent pas le retarder).
    $relance = Join-Path $temp 'relance.txt'
    $p = Lancer $relance @()
    Start-Sleep -Seconds $Attente
    $mo, $null = Mesurer-Fermer $p
    Comparer 'windows-relance' $mo 'Mo' 'Windows, relance avec extensions'
    Comparer 'windows-demarrage-extensions' (Lire $relance 'demarrage') 'ms' 'Windows, démarrage avec extensions'
    $sortie + @('', "Relance avec les deux extensions installées : $mo Mo après $Attente s ; $(Etapes $relance).")
}

# Les étapes du démarrage écrites par Feather, en une phrase.
function Etapes([string]$Fichier) {
    $v = foreach ($cle in 'rust', 'interface', 'demarrage') { $x = Lire $Fichier $cle; if ($null -ne $x) { $x } else { '?' } }
    "Rust prêt en $($v[0]) ms, barre prête en $($v[1]) ms, accueil chargé en $($v[2]) ms"
}

$lignes = @(
    "### Banc de Feather (Windows, après $Attente s)",
    '',
    '| Scénario | Mémoire totale | Processus |',
    '| --- | ---: | ---: |',
    (Scenario 'Accueil seul' 'accueil' @()),
    (Scenario "$($sites.Count) sites" 'sites' $sites)
)
$accueil = Join-Path $temp 'accueil.txt'
$lignes += @('', "Démarrage : $(Etapes $accueil).")
Comparer 'windows-demarrage' (Lire $accueil 'demarrage') 'ms' 'Windows, démarrage'
if ($Extensions) { $lignes += Scenario-Extensions }
if ($alertes.Count) { $lignes += @('', '**Alertes**', '') + ($alertes | ForEach-Object { "- $_" }) }

$lignes | Write-Output
if ($env:GITHUB_STEP_SUMMARY) { $lignes | Add-Content -Path $env:GITHUB_STEP_SUMMARY -Encoding utf8 }
# Annotations de la compilation : visibles sans ouvrir le résumé, jamais bloquantes.
foreach ($a in $alertes) { Write-Output "::warning title=Banc de Feather::$a" }
Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
