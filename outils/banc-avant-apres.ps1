# Compare la mémoire de deux versions de Feather sous Windows, avec un index du
# texte des pages plein et cinq sites, à 30 s, 3 min et 11 min.
param(
    [Parameter(Mandatory = $true)][string]$Avant,
    [Parameter(Mandatory = $true)][string]$Apres,
    [Parameter(Mandatory = $true)][string]$Index
)
$ErrorActionPreference = 'Stop'
$sites = @(
    'https://fr.wikipedia.org/wiki/Rust_(langage)',
    'https://www.lemonde.fr/',
    'https://github.com/Shaashuu/navigateur',
    'https://developer.mozilla.org/fr/',
    'https://www.youtube.com/'
)
$config = Join-Path $env:APPDATA 'Feather\Feather\config'
$moments = @(30, 180, 660)

function Get-Arbre([int]$Racine) {
    $tous = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, WorkingSetSize
    $ids = @{ $Racine = $true }
    do {
        $ajoutes = 0
        foreach ($p in $tous) {
            if (-not $ids.ContainsKey([int]$p.ProcessId) -and $ids.ContainsKey([int]$p.ParentProcessId)) {
                $ids[[int]$p.ProcessId] = $true; $ajoutes++
            }
        }
    } while ($ajoutes -gt 0)
    $tous | Where-Object { $ids.ContainsKey([int]$_.ProcessId) }
}

function Mesurer([string]$Programme) {
    Remove-Item -Recurse -Force $config -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $config | Out-Null
    Copy-Item $Index (Join-Path $config 'contenu.json')
    $p = Start-Process -FilePath $Programme -ArgumentList $sites -PassThru
    $debut = Get-Date
    $res = @()
    foreach ($s in $moments) {
        $reste = $s - ((Get-Date) - $debut).TotalSeconds
        if ($reste -gt 0) { Start-Sleep -Seconds $reste }
        $arbre = @(Get-Arbre $p.Id)
        $total = [math]::Round((($arbre | Measure-Object -Property WorkingSetSize -Sum).Sum) / 1MB)
        $seul = [math]::Round((($arbre | Where-Object { $_.ProcessId -eq $p.Id }).WorkingSetSize) / 1MB)
        $res += [pscustomobject]@{ Total = $total; Feather = $seul }
    }
    foreach ($q in @(Get-Arbre $p.Id)) { Stop-Process -Id $q.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 5
    $res
}

$a = Mesurer $Avant
$b = Mesurer $Apres
$lignes = @('### Avant / après (Windows, 5 sites, index plein de 2 000 pages)', '',
    '| Moment | 0.3.3 total | 0.3.4 total | 0.3.3 Feather seul | 0.3.4 Feather seul |',
    '| --- | ---: | ---: | ---: | ---: |')
for ($i = 0; $i -lt $moments.Count; $i++) {
    $lignes += "| $($moments[$i]) s | $($a[$i].Total) Mo | $($b[$i].Total) Mo | $($a[$i].Feather) Mo | $($b[$i].Feather) Mo |"
}
$lignes | Write-Output
if ($env:GITHUB_STEP_SUMMARY) { $lignes | Add-Content -Path $env:GITHUB_STEP_SUMMARY -Encoding utf8 }
