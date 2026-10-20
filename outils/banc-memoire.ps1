# Banc de mesure de la mémoire de Feather (Windows).
#
# Lance Feather deux fois : accueil seul, puis avec cinq sites ouverts, et
# additionne la mémoire de Feather et de tous ses processus enfants
# (msedgewebview2 : pages, réseau, affichage).
#
# Utilisation : outils\banc-memoire.ps1 -Programme chemin\vers\feather.exe [-Attente 30]
param(
    [Parameter(Mandatory = $true)][string]$Programme,
    [int]$Attente = 30
)
$ErrorActionPreference = 'Stop'

$sites = @(
    'https://fr.wikipedia.org/wiki/Rust_(langage)',
    'https://www.lemonde.fr/',
    'https://github.com/Shaashuu/navigateur',
    'https://developer.mozilla.org/fr/',
    'https://www.youtube.com/'
)

# Feather et tous ses descendants.
function Get-Arbre([int]$Racine) {
    $tous = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, WorkingSetSize
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

function Mesurer([string]$Nom, [string[]]$Adresses) {
    $arguments = if ($Adresses) { $Adresses } else { @() }
    $p = if ($arguments.Count) { Start-Process -FilePath $Programme -ArgumentList $arguments -PassThru } else { Start-Process -FilePath $Programme -PassThru }
    Start-Sleep -Seconds $Attente
    $arbre = @(Get-Arbre $p.Id)
    $mo = [math]::Round((($arbre | Measure-Object -Property WorkingSetSize -Sum).Sum) / 1MB)
    foreach ($q in $arbre) { Stop-Process -Id $q.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 3
    "| $Nom | $mo Mo | $($arbre.Count) |"
}

$lignes = @(
    "### Mémoire de Feather (Windows, après $Attente s)",
    '',
    '| Scénario | Mémoire totale | Processus |',
    '| --- | ---: | ---: |',
    (Mesurer 'Accueil seul' @()),
    (Mesurer "$($sites.Count) sites" $sites)
)
$lignes | Write-Output
if ($env:GITHUB_STEP_SUMMARY) { $lignes | Add-Content -Path $env:GITHUB_STEP_SUMMARY -Encoding utf8 }
